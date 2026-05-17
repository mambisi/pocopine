//! Editor commands — a Rust port of `prosemirror-commands`.
//!
//! A [`Command`] is a function-like object that, given an [`EditorState`],
//! either produces a [`Transaction`] that would apply it (`Some(tr)`) or
//! reports that it doesn't apply at this state (`None`). Mirrors PM's
//! `(state, dispatch, view) => boolean` shape collapsed to "build a
//! transaction or don't" — `dispatch` is a caller concern in pine (just call
//! `state.apply(tr)`) and `view` is out of scope for this model-only crate.
//!
//! Commands compose via [`chain_commands`] and bind to key names via
//! [`base_keymap`]. Most commands are produced by free functions returning
//! a boxed trait object so they can be stored in a `HashMap<&str, _>`.

use crate::model::{Attrs, Mark, Node, NodeRange, ResolvedPos};
use crate::state::{EditorState, Selection, Transaction};
use crate::transform::{can_join, find_wrapping, join_point, lift_target, AttrStep, Step};

/// A reusable editor command. Implementors decide whether the command
/// applies at the given state and, if so, return the transaction that
/// would commit it. Returning `None` means "this command doesn't apply"
/// — callers can fall through to the next command in a [`chain_commands`]
/// chain.
pub trait Command: Send + Sync {
    /// Build the transaction this command produces at `state`, or `None`
    /// if the command doesn't apply.
    fn apply(&self, state: &EditorState) -> Option<Transaction>;
}

/// Every `Fn(&EditorState) -> Option<Transaction>` is automatically a
/// `Command`, so plain closures work as commands without any adapter.
impl<F> Command for F
where
    F: Fn(&EditorState) -> Option<Transaction> + Send + Sync,
{
    fn apply(&self, state: &EditorState) -> Option<Transaction> {
        self(state)
    }
}

/// Boxed command trait object — what every `commands::*` factory returns.
pub type BoxedCommand = Box<dyn Command>;

fn boxed(f: impl Fn(&EditorState) -> Option<Transaction> + Send + Sync + 'static) -> BoxedCommand {
    Box::new(f)
}

// ====================================================================
// Selection commands (no transform required)
// ====================================================================

/// Delete the current selection.
pub fn delete_selection() -> BoxedCommand {
    boxed(|state| {
        if state.selection().is_empty(state.doc()) {
            return None;
        }
        let mut tr = state.tr();
        tr.delete_selection().ok()?;
        Some(tr)
    })
}

/// Select the entire document.
pub fn select_all() -> BoxedCommand {
    boxed(|state| {
        if matches!(state.selection(), Selection::All) {
            return None;
        }
        let mut tr = state.tr();
        tr.set_selection(Selection::All).ok()?;
        Some(tr)
    })
}

/// Select the parent block of the current selection.
pub fn select_parent_node() -> BoxedCommand {
    boxed(|state| {
        let sel = state.selection();
        let from = sel.from(state.doc());
        let to = sel.to(state.doc());
        let resolved = state.doc().resolve(from).ok()?;
        let same_depth = resolved.shared_depth(to);
        for depth in (1..=same_depth).rev() {
            let before = resolved.before(depth)?;
            let mut tr = state.tr();
            if tr.set_selection(Selection::node(before)).is_ok() {
                return Some(tr);
            }
        }
        None
    })
}

/// Move the cursor to the start of the deepest textblock around the selection.
/// Mirrors PM's `selectTextblockStart`.
pub fn select_textblock_start() -> BoxedCommand {
    boxed(|state| {
        let sel = state.selection();
        let pos = sel.from(state.doc());
        let resolved = state.doc().resolve(pos).ok()?;
        let mut depth = resolved.depth();
        loop {
            let node = resolved.node(depth)?;
            let node_type = state.schema().node_type(node.type_name()).ok()?;
            if !node_type.is_inline() && node_type.inline_content(state.schema()) {
                let target = resolved.start(depth)?;
                let mut tr = state.tr();
                tr.set_selection(Selection::text(target)).ok()?;
                return Some(tr);
            }
            if depth == 0 {
                return None;
            }
            depth -= 1;
        }
    })
}

/// Move the cursor to the end of the deepest textblock around the selection.
/// Mirrors PM's `selectTextblockEnd`.
pub fn select_textblock_end() -> BoxedCommand {
    boxed(|state| {
        let sel = state.selection();
        let pos = sel.to(state.doc());
        let resolved = state.doc().resolve(pos).ok()?;
        let mut depth = resolved.depth();
        loop {
            let node = resolved.node(depth)?;
            let node_type = state.schema().node_type(node.type_name()).ok()?;
            if !node_type.is_inline() && node_type.inline_content(state.schema()) {
                let target = resolved.end(depth)?;
                let mut tr = state.tr();
                tr.set_selection(Selection::text(target)).ok()?;
                return Some(tr);
            }
            if depth == 0 {
                return None;
            }
            depth -= 1;
        }
    })
}

// ====================================================================
// Block-level commands: lift / join / split
// ====================================================================

fn block_range_for_selection(state: &EditorState) -> Option<NodeRange> {
    let from = state
        .doc()
        .resolve(state.selection().from(state.doc()))
        .ok()?;
    let to = state
        .doc()
        .resolve(state.selection().to(state.doc()))
        .ok()?;
    from.block_range(Some(&to))
}

/// Lift the closest ancestor block of the selection out of its parent.
/// Mirrors PM's `lift`.
pub fn lift() -> BoxedCommand {
    boxed(|state| {
        let range = block_range_for_selection(state)?;
        lift_target(&range, state.schema())?;
        let mut tr = state.tr();
        tr.lift(range.start(), range.end()).ok()?;
        Some(tr)
    })
}

/// Lift the current block if it's empty. The natural "Backspace at the
/// start of an empty paragraph" behavior. Mirrors PM's `liftEmptyBlock`.
pub fn lift_empty_block() -> BoxedCommand {
    boxed(|state| {
        let sel = state.selection();
        let from = sel.from(state.doc());
        let to = sel.to(state.doc());
        if from != to {
            return None;
        }
        let resolved = state.doc().resolve(from).ok()?;
        if resolved.parent().content_size() > 0 || resolved.depth() == 0 {
            return None;
        }
        // Lift the boundary AROUND the empty block, not the zero-size cursor
        // range — `transform::lift` needs at least one sibling in scope.
        let from_pos = resolved.before(resolved.depth())?;
        let to_pos = resolved.after(resolved.depth())?;
        let mut tr = state.tr();
        tr.lift(from_pos, to_pos).ok()?;
        Some(tr)
    })
}

/// Join the selected block (or closest joinable ancestor) with the sibling
/// before it. Mirrors PM's `joinUp`.
pub fn join_up() -> BoxedCommand {
    boxed(|state| {
        let sel = state.selection();
        let from = sel.from(state.doc());
        let point = match sel {
            Selection::Node { anchor } => {
                let resolved = state.doc().resolve(*anchor).ok()?;
                let node = resolved.node_after()?;
                let node_type = state.schema().node_type(node.type_name()).ok()?;
                if node_type.inline_content(state.schema())
                    || !can_join(state.doc(), *anchor, state.schema())
                {
                    return None;
                }
                *anchor
            }
            _ => join_point(state.doc(), from, -1, state.schema())?,
        };
        let mut tr = state.tr();
        tr.join(point).ok()?;
        Some(tr)
    })
}

/// Join the selected block (or closest joinable ancestor) with the sibling
/// after it. Mirrors PM's `joinDown`.
pub fn join_down() -> BoxedCommand {
    boxed(|state| {
        let sel = state.selection();
        let to = sel.to(state.doc());
        let point = match sel {
            Selection::Node { .. } => {
                let resolved = state.doc().resolve(to).ok()?;
                let node = resolved.node_before()?;
                let node_type = state.schema().node_type(node.type_name()).ok()?;
                if node_type.inline_content(state.schema())
                    || !can_join(state.doc(), to, state.schema())
                {
                    return None;
                }
                to
            }
            _ => join_point(state.doc(), to, 1, state.schema())?,
        };
        let mut tr = state.tr();
        tr.join(point).ok()?;
        Some(tr)
    })
}

/// Split the current block at the cursor. Mirrors PM's `splitBlock`.
pub fn split_block() -> BoxedCommand {
    boxed(|state| {
        let sel = state.selection();
        let from = sel.from(state.doc());
        let to = sel.to(state.doc());
        let resolved = state.doc().resolve(from).ok()?;
        if resolved.depth() == 0 {
            return None;
        }
        let mut tr = state.tr();
        if from < to {
            tr.delete(from, to).ok()?;
        }
        let pos = tr.selection().map(|s| s.from(tr.doc())).unwrap_or(from);
        if !crate::transform::can_split(tr.doc(), pos, 1, state.schema()) {
            return None;
        }
        tr.split(pos, 1).ok()?;
        set_selection_near(&mut tr, state, pos + 2).ok()?;
        Some(tr)
    })
}

/// Split the enclosing list-item-like node so Enter creates a new
/// sibling item. Mirrors PM's `splitListItem(itemType)`, generalized to
/// accept multiple item types so one binding covers both `list_item`
/// and `task_item`.
///
/// Returns `None` when the cursor isn't inside any of the named types —
/// callers should chain this before plain `split_block` so a regular
/// paragraph still splits.
pub fn split_list_item(item_types: &[&str]) -> BoxedCommand {
    let names: Vec<String> = item_types.iter().map(|s| (*s).to_string()).collect();
    boxed(move |state| {
        let sel = state.selection();
        let from = sel.from(state.doc());
        let to = sel.to(state.doc());
        let resolved = state.doc().resolve(from).ok()?;
        let item_depth = (0..=resolved.depth()).rev().find(|&d| {
            resolved
                .node(d)
                .map(|n| names.iter().any(|name| name == n.type_name()))
                .unwrap_or(false)
        })?;
        // +1 because split_depth counts from the cursor's parent outward,
        // and we want to also split the list_item itself.
        let split_depth = resolved.depth() - item_depth + 1;
        let mut tr = state.tr();
        if from < to {
            tr.delete(from, to).ok()?;
        }
        let pos = tr.selection().map(|s| s.from(tr.doc())).unwrap_or(from);
        if !crate::transform::can_split(tr.doc(), pos, split_depth, state.schema()) {
            return None;
        }
        let original_item = resolved.node(item_depth)?;
        let original_item_type = original_item.type_name().to_string();
        tr.split(pos, split_depth).ok()?;

        // pine's `tr.split` clones the original node's attrs onto both
        // halves. For `task_item` that means hitting Enter on a checked
        // item produces a new checked sibling — surprising UX. Reset
        // `checked` to the schema default on the right-half item.
        if original_item_type == "task_item" {
            // After `tr.split(pos, depth)` the split point grows by
            // `depth` close tokens before the new right-half opens, so
            // the right-half item's outer position is `pos + depth`.
            let new_item_pos = pos + split_depth;
            let _ = tr.step(Step::Attr(AttrStep {
                pos: new_item_pos,
                attr: "checked".to_string(),
                value: Some(serde_json::json!(false)),
            }));
        }
        set_selection_near(&mut tr, state, pos + split_depth * 2).ok()?;
        Some(tr)
    })
}

fn set_selection_near(
    tr: &mut Transaction,
    state: &EditorState,
    pos: usize,
) -> crate::RichTextResult<()> {
    let selection = Selection::near(tr.doc(), state.schema(), pos, 1)?;
    tr.set_selection(selection)?;
    Ok(())
}

// ====================================================================
// Cursor-position helpers (model-only — no view dependency)
// ====================================================================

/// Resolved cursor at the model start of its textblock, or `None`. PM's
/// view-aware variant also calls `view.endOfTextblock("backward", state)`
/// to account for bidi; pine's caller is expected to feed an already-
/// correct `Selection`, so the model check is sufficient.
fn at_block_start(state: &EditorState) -> Option<ResolvedPos> {
    if !state.selection().is_empty(state.doc()) {
        return None;
    }
    let cursor = state
        .doc()
        .resolve(state.selection().from(state.doc()))
        .ok()?;
    if cursor.parent_offset() > 0 {
        return None;
    }
    Some(cursor)
}

/// Resolved cursor at the model end of its textblock, or `None`.
fn at_block_end(state: &EditorState) -> Option<ResolvedPos> {
    if !state.selection().is_empty(state.doc()) {
        return None;
    }
    let cursor = state
        .doc()
        .resolve(state.selection().from(state.doc()))
        .ok()?;
    if cursor.parent_offset() != cursor.parent().content_size() {
        return None;
    }
    Some(cursor)
}

/// Walk outward from `pos` looking for an ancestor whose path has a sibling
/// to its left; resolve the position right before that ancestor. PM:
/// `findCutBefore` in commands.ts.
fn find_cut_before(state: &EditorState, pos: &ResolvedPos) -> Option<ResolvedPos> {
    let parent_type = state.schema().node_type(pos.parent().type_name()).ok()?;
    if parent_type.is_isolating() {
        return None;
    }
    for d in (0..pos.depth()).rev() {
        if pos.index(d)? > 0 {
            return state.doc().resolve(pos.before(d + 1)?).ok();
        }
        let ancestor = pos.node(d)?;
        let ancestor_type = state.schema().node_type(ancestor.type_name()).ok()?;
        if ancestor_type.is_isolating() {
            return None;
        }
    }
    None
}

/// Symmetric to `find_cut_before`. PM: `findCutAfter`.
fn find_cut_after(state: &EditorState, pos: &ResolvedPos) -> Option<ResolvedPos> {
    let parent_type = state.schema().node_type(pos.parent().type_name()).ok()?;
    if parent_type.is_isolating() {
        return None;
    }
    for d in (0..pos.depth()).rev() {
        let parent = pos.node(d)?;
        if pos.index(d)? + 1 < parent.child_count() {
            return state.doc().resolve(pos.after(d + 1)?).ok();
        }
        let parent_type = state.schema().node_type(parent.type_name()).ok()?;
        if parent_type.is_isolating() {
            return None;
        }
    }
    None
}

/// Walk from `start` toward the first/last textblock leaf, honoring
/// isolating boundaries. PM's `joinTextblocksAround` open-loop equivalent.
fn descend_to_textblock<'a>(state: &EditorState, start: &'a Node, first: bool) -> Option<&'a Node> {
    let mut node = start;
    loop {
        let node_type = state.schema().node_type(node.type_name()).ok()?;
        if node_type.is_inline() {
            // Reached text/inline content — its parent was the textblock.
            return None;
        }
        if node_type.inline_content(state.schema()) {
            return Some(node);
        }
        if node_type.is_isolating() {
            return None;
        }
        let child = if first {
            node.content().child(0)?
        } else {
            let last = node.child_count().checked_sub(1)?;
            node.content().child(last)?
        };
        node = child;
    }
}

// ====================================================================
// Backward / forward join + select-node
// ====================================================================

/// If the cursor sits at the start of a textblock, join it with the
/// textblock that ends right before it. Mirrors PM's
/// `joinTextblockBackward` collapsed to the model-only branch.
pub fn join_textblock_backward() -> BoxedCommand {
    boxed(|state| {
        let cursor = at_block_start(state)?;
        let cut = find_cut_before(state, &cursor)?;
        let before = cut.node_before()?;
        let after = cut.node_after()?;
        // Drill both sides down to their innermost textblocks; the join
        // boundary must connect two textblocks, otherwise we'd violate
        // the parent's content match.
        descend_to_textblock(state, &before, false)?;
        descend_to_textblock(state, &after, true)?;
        let mut tr = state.tr();
        // The boundary at `cut.pos` is the position right after the
        // `before` node and right before the `after` node. Deleting one
        // position to each side collapses the textblocks together.
        tr.delete(cut.pos().saturating_sub(1), cut.pos() + 1).ok()?;
        Some(tr)
    })
}

/// Symmetric counterpart. Mirrors PM's `joinTextblockForward`.
pub fn join_textblock_forward() -> BoxedCommand {
    boxed(|state| {
        let cursor = at_block_end(state)?;
        let cut = find_cut_after(state, &cursor)?;
        let before = cut.node_before()?;
        let after = cut.node_after()?;
        descend_to_textblock(state, &before, false)?;
        descend_to_textblock(state, &after, true)?;
        let mut tr = state.tr();
        tr.delete(cut.pos().saturating_sub(1), cut.pos() + 1).ok()?;
        Some(tr)
    })
}

/// Backspace behavior: try joining with the textblock before, falling
/// back to lifting or deleting a preceding atom. Mirrors PM's
/// `joinBackward` model-only path.
pub fn join_backward() -> BoxedCommand {
    boxed(|state| {
        let cursor = at_block_start(state)?;
        if let Some(cut) = find_cut_before(state, &cursor) {
            // Try the textblock join.
            let before = cut.node_before()?;
            let after = cut.node_after()?;
            if descend_to_textblock(state, &before, false).is_some()
                && descend_to_textblock(state, &after, true).is_some()
            {
                let mut tr = state.tr();
                if tr
                    .delete(cut.pos().saturating_sub(1), cut.pos() + 1)
                    .is_ok()
                {
                    return Some(tr);
                }
            }
            // If the preceding sibling is an atom (image, hr), delete it.
            let before_type = state.schema().node_type(before.type_name()).ok()?;
            if before_type.is_atom() && cut.depth() == cursor.depth().saturating_sub(1) {
                let mut tr = state.tr();
                let size = before.node_size();
                tr.delete(cut.pos().saturating_sub(size), cut.pos()).ok()?;
                return Some(tr);
            }
            return None;
        }
        // No cut before — lift the current block out of its parent instead.
        let range = cursor.block_range(None)?;
        lift_target(&range, state.schema())?;
        let mut tr = state.tr();
        tr.lift(range.start(), range.end()).ok()?;
        Some(tr)
    })
}

/// Forward-delete at the end of a textblock. Mirror of `join_backward`.
pub fn join_forward() -> BoxedCommand {
    boxed(|state| {
        let cursor = at_block_end(state)?;
        let cut = find_cut_after(state, &cursor)?;
        let before = cut.node_before()?;
        let after = cut.node_after()?;
        if descend_to_textblock(state, &before, false).is_some()
            && descend_to_textblock(state, &after, true).is_some()
        {
            let mut tr = state.tr();
            if tr
                .delete(cut.pos().saturating_sub(1), cut.pos() + 1)
                .is_ok()
            {
                return Some(tr);
            }
        }
        let after_type = state.schema().node_type(after.type_name()).ok()?;
        if after_type.is_atom() && cut.depth() == cursor.depth().saturating_sub(1) {
            let mut tr = state.tr();
            tr.delete(cut.pos(), cut.pos() + after.node_size()).ok()?;
            return Some(tr);
        }
        None
    })
}

/// If the cursor sits at the start of a textblock and the sibling before
/// is a selectable atom, select it. Mirrors PM's `selectNodeBackward`.
pub fn select_node_backward() -> BoxedCommand {
    boxed(|state| {
        let cursor = at_block_start(state)?;
        let cut = find_cut_before(state, &cursor)?;
        let before = cut.node_before()?;
        let before_type = state.schema().node_type(before.type_name()).ok()?;
        if !before_type.is_atom() || !before_type.is_selectable() {
            return None;
        }
        let mut tr = state.tr();
        tr.set_selection(Selection::node(
            cut.pos().saturating_sub(before.node_size()),
        ))
        .ok()?;
        Some(tr)
    })
}

/// Symmetric to `select_node_backward`. Mirrors PM's `selectNodeForward`.
pub fn select_node_forward() -> BoxedCommand {
    boxed(|state| {
        let cursor = at_block_end(state)?;
        let cut = find_cut_after(state, &cursor)?;
        let after = cut.node_after()?;
        let after_type = state.schema().node_type(after.type_name()).ok()?;
        if !after_type.is_atom() || !after_type.is_selectable() {
            return None;
        }
        let mut tr = state.tr();
        tr.set_selection(Selection::node(cut.pos())).ok()?;
        Some(tr)
    })
}

// ====================================================================
// Wrap / setBlockType / toggleMark
// ====================================================================

/// Wrap the selection in a node of the given type. Mirrors PM's
/// `wrapIn(type, attrs)`. When the schema needs intermediate wrappers
/// between the gap parent and the target (e.g. `bullet_list` is
/// `list_item+`, but we're handed a `paragraph` selection), the
/// resolver's chain is applied as a single `wrap_chain` step so the
/// inner wrapper absorbs the paragraphs and the schema check passes.
pub fn wrap_in(node_type: impl Into<String>, attrs: Attrs) -> BoxedCommand {
    let node_type = node_type.into();
    boxed(move |state| {
        let range = block_range_for_selection(state)?;
        let target = state.schema().node_type(&node_type).ok()?;
        let chain = find_wrapping(&range, target, attrs.clone(), state.schema())?;
        let mut tr = state.tr();
        tr.wrap_chain(range.start(), range.end(), chain).ok()?;
        Some(tr)
    })
}

/// Wrap the selection in a list, making every selected sibling block
/// its own list item. This is the list-specific command equivalent to
/// ProseMirror's `wrapInList`: `wrap_in("bullet_list")` intentionally
/// wraps the selected blocks in one `list_item`, while this command
/// performs the follow-up splits that list toolbar buttons need.
pub fn wrap_in_list(
    list_type: impl Into<String>,
    item_type: impl Into<String>,
    attrs: Attrs,
) -> BoxedCommand {
    let list_type = list_type.into();
    let item_type = item_type.into();
    boxed(move |state| {
        let range = block_range_for_selection(state)?;
        let target = state.schema().node_type(&list_type).ok()?;
        let chain = find_wrapping(&range, target, attrs.clone(), state.schema())?;

        let found_list = chain
            .iter()
            .position(|spec| spec.type_name == list_type)
            .map(|index| index + 1)?;
        if !chain.iter().any(|spec| spec.type_name == item_type) {
            return None;
        }
        let split_depth = chain.len().checked_sub(found_list)?;
        if split_depth == 0 {
            return None;
        }

        let parent = range.parent().clone();
        let start = range.start();
        let end = range.end();
        let start_index = range.start_index();
        let end_index = range.end_index();
        let mut tr = state.tr();
        tr.wrap_chain(start, end, chain.clone()).ok()?;

        let mut split_pos = start + chain.len();
        for index in start_index..end_index {
            if index > start_index {
                if !crate::transform::can_split(tr.doc(), split_pos, split_depth, state.schema()) {
                    return None;
                }
                tr.split(split_pos, split_depth).ok()?;
                split_pos += 2 * split_depth;
            }
            let child = parent.content().child(index)?;
            split_pos += child.node_size();
        }

        Some(tr)
    })
}

/// Change the type of every textblock in the selection. Mirrors PM's
/// `setBlockType(type, attrs)`.
pub fn set_block_type(node_type: impl Into<String>, attrs: Attrs) -> BoxedCommand {
    let node_type = node_type.into();
    boxed(move |state| {
        let sel = state.selection();
        let from = sel.from(state.doc());
        let to = sel.to(state.doc());
        let mut tr = state.tr();
        tr.set_block_type(from, to, &node_type, attrs.clone())
            .ok()?;
        if tr.transform().steps().is_empty() {
            return None;
        }
        Some(tr)
    })
}

/// Toggle a mark across the selection. If the entire range carries the
/// mark, remove it; otherwise add it. Mirrors PM's `toggleMark(mark)`.
pub fn toggle_mark(mark: Mark) -> BoxedCommand {
    boxed(move |state| {
        let sel = state.selection();
        let from = sel.from(state.doc());
        let to = sel.to(state.doc());
        if from == to {
            // Toggle stored marks for the next inserted character.
            let mut tr = state.tr();
            let mut stored: Vec<Mark> = state
                .stored_marks()
                .map(<[Mark]>::to_vec)
                .unwrap_or_default();
            if stored.iter().any(|m| m == &mark) {
                stored.retain(|m| m != &mark);
            } else {
                stored = mark.add_to_set(state.schema(), &stored).ok()?;
            }
            tr.set_stored_marks(stored);
            return Some(tr);
        }
        let mut tr = state.tr();
        let has_mark = state.doc().range_has_mark(from, to, &mark).ok()?;
        if has_mark {
            tr.remove_mark(from, to, mark.clone()).ok()?;
        } else {
            tr.add_mark(from, to, mark.clone()).ok()?;
        }
        if tr.transform().steps().is_empty() {
            return None;
        }
        Some(tr)
    })
}

// ====================================================================
// Combinators
// ====================================================================

/// Try each command in turn; return the first one that applies. Mirrors
/// PM's `chainCommands(...commands)`.
pub fn chain_commands(commands: Vec<BoxedCommand>) -> BoxedCommand {
    Box::new(ChainedCommands { commands })
}

struct ChainedCommands {
    commands: Vec<BoxedCommand>,
}

impl Command for ChainedCommands {
    fn apply(&self, state: &EditorState) -> Option<Transaction> {
        self.commands.iter().find_map(|c| c.apply(state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_basic;
    use crate::state::{EditorState, EditorStateConfig};

    /// End of "one" inside `<ul><li><p>one</p></li></ul>` → cursor is at
    /// model position 4 (doc 0 → ul opens, 1 → li opens, 2 → p opens,
    /// 3..6 → "one", 6 → p closes). `split_list_item` should turn the
    /// single `<li>` into two — the original holding "one" and a new
    /// empty one after it.
    #[test]
    fn split_list_item_creates_a_new_sibling_li() {
        let li = schema_basic::list_item(vec![schema_basic::paragraph(vec![schema_basic::text(
            "one",
            Vec::new(),
        )
        .unwrap()])
        .unwrap()])
        .unwrap();
        let ul = schema_basic::bullet_list(vec![li]).unwrap();
        let doc = schema_basic::doc(vec![ul]).unwrap();

        let state = EditorState::create(
            EditorStateConfig::new(schema_basic::schema(), doc).selection(Selection::text(6)),
        )
        .unwrap();
        let cmd = split_list_item(&["list_item", "task_item"]);
        let tr = cmd.apply(&state).expect("command should apply");
        let next = state.apply(tr).unwrap();
        let ul = next.doc().content().child(0).unwrap();
        assert_eq!(ul.type_name(), "bullet_list");
        assert_eq!(ul.child_count(), 2, "should produce two list items");
        assert_eq!(ul.content().child(0).unwrap().text_content(), "one");
        assert_eq!(ul.content().child(1).unwrap().text_content(), "");
    }

    /// Selecting text that spans two paragraphs and dispatching
    /// `wrap_in("bullet_list")` should produce one bullet list with
    /// the two paragraphs wrapped in a single list_item (matching
    /// PM's behavior — Enter inside the wrap splits the list_item
    /// further if the user wants distinct items).
    #[test]
    fn wrap_in_bullet_list_with_multi_paragraph_selection() {
        let p1 = schema_basic::paragraph(vec![schema_basic::text("alpha", Vec::new()).unwrap()])
            .unwrap();
        let p2 = schema_basic::paragraph(vec![schema_basic::text("bravo", Vec::new()).unwrap()])
            .unwrap();
        let doc = schema_basic::doc(vec![p1, p2]).unwrap();

        let state = EditorState::create(
            EditorStateConfig::new(schema_basic::schema(), doc)
                // anchor inside p1 text, head inside p2 text
                .selection(Selection::text_between(3, 10)),
        )
        .unwrap();
        let cmd = wrap_in("bullet_list", Attrs::new());
        let tr = cmd.apply(&state).expect("command should apply");
        let next = state.apply(tr).unwrap();
        let outer = next.doc().content().child(0).unwrap();
        assert_eq!(outer.type_name(), "bullet_list");
        assert_eq!(
            outer.child_count(),
            1,
            "single list_item wraps both paragraphs"
        );
        let item = outer.content().child(0).unwrap();
        assert_eq!(item.type_name(), "list_item");
        assert_eq!(item.child_count(), 2);
        assert_eq!(item.content().child(0).unwrap().text_content(), "alpha");
        assert_eq!(item.content().child(1).unwrap().text_content(), "bravo");
    }

    #[test]
    fn wrap_in_list_bullet_list_splits_selected_blocks_into_items() {
        let p1 = schema_basic::paragraph(vec![schema_basic::text("alpha", Vec::new()).unwrap()])
            .unwrap();
        let p2 = schema_basic::paragraph(vec![schema_basic::text("bravo", Vec::new()).unwrap()])
            .unwrap();
        let doc = schema_basic::doc(vec![p1, p2]).unwrap();

        let state = EditorState::create(
            EditorStateConfig::new(schema_basic::schema(), doc)
                .selection(Selection::text_between(3, 10)),
        )
        .unwrap();
        let cmd = wrap_in_list("bullet_list", "list_item", Attrs::new());
        let tr = cmd.apply(&state).expect("command should apply");
        let next = state.apply(tr).unwrap();
        let list = next.doc().content().child(0).unwrap();
        assert_eq!(list.type_name(), "bullet_list");
        assert_eq!(list.child_count(), 2);
        assert_eq!(list.content().child(0).unwrap().type_name(), "list_item");
        assert_eq!(list.content().child(0).unwrap().text_content(), "alpha");
        assert_eq!(list.content().child(1).unwrap().type_name(), "list_item");
        assert_eq!(list.content().child(1).unwrap().text_content(), "bravo");
    }

    #[test]
    fn wrap_in_list_ordered_list_resolves_default_order_attr() {
        let blocks = ["one", "two", "three"]
            .into_iter()
            .map(|value| {
                schema_basic::paragraph(vec![schema_basic::text(value, Vec::new()).unwrap()])
                    .unwrap()
            })
            .collect();
        let doc = schema_basic::doc(blocks).unwrap();

        let state = EditorState::create(
            EditorStateConfig::new(schema_basic::schema(), doc)
                .selection(Selection::text_between(2, 14)),
        )
        .unwrap();
        let cmd = wrap_in_list("ordered_list", "list_item", Attrs::new());
        let tr = cmd.apply(&state).expect("command should apply");
        let next = state.apply(tr).unwrap();
        let list = next.doc().content().child(0).unwrap();
        assert_eq!(list.type_name(), "ordered_list");
        assert_eq!(
            list.attrs().get("order"),
            Some(&serde_json::json!(1)),
            "wrapper attrs should be resolved through the schema"
        );
        assert_eq!(list.child_count(), 3);
        assert_eq!(list.content().child(0).unwrap().text_content(), "one");
        assert_eq!(list.content().child(1).unwrap().text_content(), "two");
        assert_eq!(list.content().child(2).unwrap().text_content(), "three");
    }

    #[test]
    fn wrap_in_list_task_list_creates_unchecked_task_items() {
        let p1 =
            schema_basic::paragraph(vec![schema_basic::text("todo", Vec::new()).unwrap()]).unwrap();
        let p2 = schema_basic::paragraph(vec![schema_basic::text("later", Vec::new()).unwrap()])
            .unwrap();
        let doc = schema_basic::doc(vec![p1, p2]).unwrap();

        let state = EditorState::create(
            EditorStateConfig::new(schema_basic::schema(), doc)
                .selection(Selection::text_between(2, 10)),
        )
        .unwrap();
        let cmd = wrap_in_list("task_list", "task_item", Attrs::new());
        let tr = cmd.apply(&state).expect("command should apply");
        let next = state.apply(tr).unwrap();
        let list = next.doc().content().child(0).unwrap();
        assert_eq!(list.type_name(), "task_list");
        assert_eq!(list.child_count(), 2);
        for index in 0..2 {
            let item = list.content().child(index).unwrap();
            assert_eq!(item.type_name(), "task_item");
            assert_eq!(
                item.attrs().get("checked").and_then(|v| v.as_bool()),
                Some(false)
            );
        }
        assert_eq!(list.content().child(0).unwrap().text_content(), "todo");
        assert_eq!(list.content().child(1).unwrap().text_content(), "later");
    }

    /// Same flow for `task_item` — the new sibling inherits the type but
    /// gets the schema's default `checked: false` attribute.
    #[test]
    fn split_list_item_creates_a_new_task_item_with_default_attrs() {
        let item = schema_basic::task_item(
            true,
            vec![
                schema_basic::paragraph(vec![schema_basic::text("done", Vec::new()).unwrap()])
                    .unwrap(),
            ],
        )
        .unwrap();
        let list = schema_basic::task_list(vec![item]).unwrap();
        let doc = schema_basic::doc(vec![list]).unwrap();

        let state = EditorState::create(
            EditorStateConfig::new(schema_basic::schema(), doc).selection(Selection::text(7)),
        )
        .unwrap();
        let cmd = split_list_item(&["list_item", "task_item"]);
        let tr = cmd.apply(&state).expect("command should apply");
        let next = state.apply(tr).unwrap();
        let list = next.doc().content().child(0).unwrap();
        assert_eq!(list.child_count(), 2);
        let new_item = list.content().child(1).unwrap();
        assert_eq!(new_item.type_name(), "task_item");
        assert_eq!(
            new_item.attrs().get("checked").and_then(|v| v.as_bool()),
            Some(false),
            "new task_item should default to unchecked"
        );
    }
}
