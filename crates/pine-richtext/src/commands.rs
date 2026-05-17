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
use crate::transform::{can_join, find_wrapping, join_point, lift_target};

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
        Some(tr)
    })
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
/// `wrapIn(type, attrs)`.
pub fn wrap_in(node_type: impl Into<String>, attrs: Attrs) -> BoxedCommand {
    let node_type = node_type.into();
    boxed(move |state| {
        let range = block_range_for_selection(state)?;
        let target = state.schema().node_type(&node_type).ok()?;
        find_wrapping(&range, target, attrs.clone(), state.schema())?;
        let mut tr = state.tr();
        tr.wrap(range.start(), range.end(), &node_type, attrs.clone())
            .ok()?;
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
