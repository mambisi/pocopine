//! Helper constructors for the two canonical input-rule shapes:
//!
//! * [`wrapping_input_rule`] — wrap the current textblock in a new
//!   node type when a marker is typed. Used for markdown bullet /
//!   ordered / blockquote shortcuts (`* `, `1. `, `> `, etc.).
//! * [`textblock_type_input_rule`] — change the current textblock's
//!   type when a marker is typed. Used for markdown heading shortcuts
//!   (`# `, `## `, etc.) and code-block markers (``` ``` ```).
//!
//! Mirrors `prosemirror-inputrules/src/rulebuilders.ts`. The pine
//! variants take node-type **names** (strings) rather than resolved
//! `NodeType` references — consistent with how
//! [`crate::commands::wrap_in`] and `set_block_type` accept names.
//! The schema lookup happens at apply time against the runtime's
//! schema, so a builder helper isn't tied to a particular runtime.

use std::sync::Arc;

use regex::{Captures, Regex};

use crate::model::Attrs;
use crate::state::Selection;
use crate::transform::{can_join, find_wrapping};

use super::rule::InputRule;

/// Build a rule that wraps the current textblock in a new node type
/// when its pattern matches. Mirrors
/// `prosemirror-inputrules::wrappingInputRule`.
///
/// `regexp` should typically start with `^` so the pattern only
/// matches at the start of a textblock (markdown shortcuts have this
/// shape — `^\s*([-+*])\s$` for bullet lists, etc.).
///
/// `node_type_name` is the wrapper to insert. The schema must
/// declare it; otherwise the rule is a silent no-op at apply time.
/// `get_attrs` either supplies a fixed `Attrs` or computes them
/// from the regex captures (mirrors upstream's `getAttrs`).
///
/// If `join_predicate` is `None`, the rule auto-joins with a
/// preceding same-type wrapper (so `* item` immediately after
/// another bullet list extends that list rather than creating a
/// second sibling list). The predicate can override that — for
/// markdown shortcuts that don't want auto-joining, pass
/// `Some(|_, _| false)`.
/// Optional join predicate type. `None` means "auto-join with a
/// preceding same-type wrapper" (the default for markdown bullet /
/// blockquote shortcuts). `Some(predicate)` is the custom check
/// upstream PM's `wrappingInputRule(_, _, _, joinPredicate)`
/// accepts — used for ordered lists where joining depends on the
/// new list's `order` attr being contiguous with the prior list.
pub type JoinPredicate =
    Box<dyn for<'a> Fn(&'a Captures<'a>, &crate::model::Node) -> bool + Send + Sync + 'static>;

pub fn wrapping_input_rule<F>(
    regexp: Regex,
    node_type_name: impl Into<String>,
    get_attrs: F,
    join_predicate: Option<JoinPredicate>,
) -> InputRule
where
    F: for<'a> Fn(&'a Captures<'a>) -> Attrs + Send + Sync + 'static,
{
    let node_type_name: String = node_type_name.into();
    let get_attrs = Arc::new(get_attrs);
    let join_predicate: Option<Arc<JoinPredicate>> = join_predicate.map(Arc::new);

    InputRule::new(regexp, move |state, captures, start, end| {
        let attrs = (get_attrs)(captures);
        // Compute the block range on the ORIGINAL (pre-mutation)
        // doc — the paragraph still has its marker text there, so
        // `block_range` resolves to the right depth. (For an
        // empty post-delete paragraph, pine's `block_range`
        // returns a range at the inner depth, which `wrap_chain`
        // rejects with "wrap range does not cover sibling nodes."
        // The upstream PM port can do delete-first because PM's
        // `blockRange` returns the outer range even for empty
        // blocks; pine's doesn't.)
        let pre_resolved = state.doc().resolve(start).ok()?;
        let range = pre_resolved.block_range(None)?;
        let target = state.schema().node_type(&node_type_name).ok()?;
        let chain = find_wrapping(&range, target, attrs.clone(), state.schema())?;
        let chain_len = chain.len();
        let range_start = range.start();
        let range_end = range.end();

        let mut tr = state.tr();
        // Wrap first — adds `chain.len()` wrapper open tokens
        // before the wrapped range. Positions inside `[range_start,
        // range_end]` shift by `chain_len`.
        tr.wrap_chain(range_start, range_end, chain).ok()?;
        // Delete the marker at its post-wrap position. The
        // marker's pre-wrap range was `[start, end]`; both shift
        // by `chain_len`.
        let new_start = start + chain_len;
        let new_end = end + chain_len;
        if new_end > new_start {
            tr.delete(new_start, new_end).ok()?;
        }

        // Auto-join with a preceding same-type wrapper unless the
        // predicate vetoes. The boundary between the previous
        // sibling and the newly inserted wrapper is at the pre-
        // wrap `range_start` itself — NOT `range_start - 1`, which
        // would resolve inside the previous wrapper's content
        // and surface the wrong node from `node_before` (the
        // inner paragraph instead of the wrapper).
        let mut joined = false;
        let pre_join_doc = tr.doc().clone();
        if let Ok(join_resolved) = pre_join_doc.resolve(range_start) {
            if let Some(before) = join_resolved.node_before() {
                let allow = join_predicate
                    .as_ref()
                    .map(|pred| pred(captures, &before))
                    .unwrap_or(true);
                if allow
                    && before.type_name() == node_type_name
                    && can_join(tr.doc(), range_start, state.schema())
                    && tr.join(range_start).is_ok()
                {
                    joined = true;
                }
            }
        }

        // Place the cursor INSIDE the now-empty wrapped paragraph
        // so the next keystroke types into the new item. We set
        // the selection AFTER any auto-join because pine's join
        // is implemented as a whole-doc replace, whose StepMap
        // covers the entire doc and would clamp any inside-range
        // position to the doc's end. The post-join cursor is
        // `new_start` shifted by -2 (the merged wrapper's old
        // close + new open tokens disappear at the join
        // boundary). Without the join, `new_start` already points
        // inside the new paragraph.
        let cursor = if joined {
            new_start.saturating_sub(2)
        } else {
            new_start
        };
        tr.set_selection(Selection::text(cursor)).ok()?;
        Some(tr)
    })
    // Multi-step wrapping rules don't currently produce valid
    // undo metadata (run_rules reads only the first step map's
    // range, which for wrap_chain covers the entire wrap output
    // — restoring that range with the pre-wrap text would
    // corrupt the doc). Mark not_undoable so Backspace falls
    // through to the normal delete path.
    .not_undoable()
}

/// Build a rule that changes the current textblock's type when the
/// pattern matches. Mirrors
/// `prosemirror-inputrules::textblockTypeInputRule`.
///
/// Used for the markdown heading shortcut (`# `, `## `, …) and
/// code-block marker (``` ``` ```): the marker is deleted and the
/// current paragraph is converted to the new type.
pub fn textblock_type_input_rule<F>(
    regexp: Regex,
    node_type_name: impl Into<String>,
    get_attrs: F,
) -> InputRule
where
    F: for<'a> Fn(&'a Captures<'a>) -> Attrs + Send + Sync + 'static,
{
    let node_type_name: String = node_type_name.into();
    let get_attrs = Arc::new(get_attrs);

    InputRule::new(regexp, move |state, captures, start, end| {
        // Verify the parent textblock can be replaced with the
        // target type at this position. `node_type(-1)` is the
        // grandparent (the parent of the current textblock); we ask
        // it whether it would accept the new type at the current
        // textblock's index.
        let resolved = state.doc().resolve(start).ok()?;
        let grandparent_depth = resolved.depth().saturating_sub(1);
        let parent_index = resolved.index(grandparent_depth)?;
        let grandparent = resolved.node(grandparent_depth)?;
        if !grandparent.can_replace_with(
            parent_index,
            parent_index + 1,
            &node_type_name,
            &[],
            state.schema(),
        ) {
            return None;
        }

        let attrs = (get_attrs)(captures);
        let mut tr = state.tr();
        if end > start {
            tr.delete(start, end).ok()?;
        }
        tr.set_block_type(start, start, &node_type_name, attrs)
            .ok()?;
        // Place the cursor INSIDE the converted textblock so the
        // next keystroke types into the new heading / code_block.
        // Without this the selection-map result lands outside the
        // converted block and the next character appears in a
        // sibling paragraph.
        tr.set_selection(Selection::text(start)).ok()?;
        Some(tr)
    })
    // Multi-step block-shortcut rules don't currently produce
    // valid undo metadata (run_rules reads only the first step
    // map). Mark not_undoable so Backspace falls through to the
    // normal delete path instead of running a broken undo.
    .not_undoable()
}
