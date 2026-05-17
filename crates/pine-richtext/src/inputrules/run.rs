//! Pure matching engine for [`super::rule::InputRule`].
//!
//! Mirrors `prosemirror-inputrules/src/inputrules.ts::run`. Reads the
//! current [`crate::state::EditorState`] (no view dependency), reads
//! the cursor-adjacent text, evaluates each rule's regex against
//! `<prior text> + <about-to-be-inserted character>`, and returns
//! the [`crate::state::Transaction`] produced by the first matching
//! rule (or `None` when no rule fires).
//!
//! The view-side integration that calls `run_rules` from
//! `beforeinput` lands in Phase 5 C2. Keeping the matcher pure makes
//! it directly testable from unit tests without touching the DOM.

use crate::state::{EditorState, Transaction};

use super::rule::{InCodePolicy, InputRule};

/// What a successful rule fire reports back.
///
/// The three position/text fields describe the rule's *effect*:
/// where its output landed in the post-rule document and what the
/// original text was. [`super::plugin::undo_input_rule`] uses this
/// info to roll back the rule by replacing `[from, to]` in the
/// post-rule document with `text`.
///
/// * `from` — start of the rule's output in the post-rule document
/// * `to` — end of the rule's output (cursor head after the rule
///   fires)
/// * `text` — the original matched buffer slice (prior text + the
///   just-typed character). Restoring this at `[from, to]` reverts
///   the rule's effect to the typed-character state.
pub struct RuleFire {
    /// The transaction the rule's handler produced.
    pub transaction: Transaction,
    /// Start of the rule's output range in the post-rule doc.
    pub from: usize,
    /// End of the rule's output range in the post-rule doc.
    pub to: usize,
    /// Original matched text — what would be restored on undo.
    pub text: String,
    /// Whether the rule's `undoable` option is on. Surfaced so the
    /// view hook can decide whether to set rule-fire meta at all
    /// (rules with `not_undoable` skip the meta and Backspace gets
    /// normal-delete semantics).
    pub undoable: bool,
}

/// Maximum number of characters before the cursor the matcher will
/// scan. Mirrors upstream's `MAX_MATCH = 500`. Rules whose patterns
/// require more context than this are out of luck — the regex sees
/// only the trailing 500 characters of the textblock plus the
/// just-typed text. In practice 500 is far beyond any reasonable
/// rule (smart quotes need ~2 chars, markdown headings need ~7).
pub const MAX_MATCH: usize = 500;

/// Try every rule in `rules` against the text immediately preceding
/// `to` (within the textblock that contains `from`), with `text`
/// appended. Returns the first rule's emitted transaction.
///
/// Arguments mirror upstream's `run(view, from, to, text, rules)`:
/// * `state` — the current editor state
/// * `from` — the model position right before the just-typed text
/// * `to` — the model position right after the just-typed text
/// * `text` — the just-typed characters (usually 1, sometimes 0 for
///   compositionend post-processing)
/// * `rules` — every registered rule to evaluate, in registration
///   order. First matching rule wins.
pub fn run_rules(
    state: &EditorState,
    from: usize,
    to: usize,
    text: &str,
    rules: &[InputRule],
) -> Option<RuleFire> {
    let resolved_from = state.doc().resolve(from).ok()?;
    let parent = resolved_from.parent();

    // Skip when the cursor's parent can't hold inline content — only
    // textblocks contribute meaningful prefix text for matchers.
    if !parent_is_textblock(state, parent) {
        return None;
    }

    let parent_offset = resolved_from.parent_offset();
    let scan_start = parent_offset.saturating_sub(MAX_MATCH);
    let prior = parent.text_between(scan_start, parent_offset, "").ok()?;
    let buffer = format!("{prior}{text}");

    let parent_is_code = node_is_code(state, parent);
    let cursor_marks = resolved_from.marks(state.schema());
    let in_code_mark = cursor_marks
        .iter()
        .any(|mark| is_code_mark(mark.type_name()));

    for rule in rules {
        if !rule_applies(rule, parent_is_code, in_code_mark) {
            continue;
        }
        let Some(captures) = rule.regex.captures(&buffer) else {
            continue;
        };
        let full_match = captures.get(0)?;
        // Only treat the match as fresh-from-this-keystroke if it
        // overlaps the just-typed `text`. Prevents stale patterns
        // earlier in the buffer from re-firing on every keystroke.
        if full_match.as_str().chars().count() < text.chars().count() {
            continue;
        }

        // Translate the match's byte offsets back to model positions.
        // Pine-richtext positions count chars (not bytes), so we
        // convert via `.chars().count()` on the relevant slices.
        let buffer_char_count = buffer.chars().count();
        let match_char_start = buffer[..full_match.start()].chars().count();
        let from_in_buffer = buffer_char_count - text.chars().count();
        let start_pos = from.saturating_sub(from_in_buffer - match_char_start);

        if let Some(transaction) = (rule.handler)(state, &captures, start_pos, to) {
            // Compute the rule's output range from the transform's
            // FIRST step map. Each `StepMap` records the exact
            // `{old_start, old_size, new_size}` of a replacement,
            // so the rule's effect spans
            // `[old_start, old_start + new_size]` in the post-rule
            // doc. Works uniformly for full-match replacement (rule
            // deletes some prior chars, inserts at the same start)
            // and capture-group replacement (rule deletes nothing,
            // inserts at the cursor).
            //
            // **Limitation:** for handlers that emit multiple steps
            // (rare; the predefined rules use single-step
            // `replace_with`), only the first step's range is
            // considered. Custom multi-step handlers should report
            // their own undo range via the future
            // `not_undoable()` opt-out or wait for a later
            // refinement.
            let (output_from, output_to, replaced_old_size) = transaction
                .transform()
                .maps()
                .first()
                .and_then(|map| map.ranges.first())
                .map(|range| {
                    (
                        range.old_start,
                        range.old_start + range.new_size,
                        range.old_size,
                    )
                })
                .unwrap_or((start_pos, to, 0));

            // The original undo text is "what was at the rule's
            // CONSUMED PRIOR range BEFORE the rule fired" + "the
            // just-typed text". The consumed prior range is
            // `[output_from, from]` — from the rule's output start
            // to the cursor's start (which is the caller's `from`).
            // Crucially this excludes content inside the input
            // range `[from, to]` (a non-empty selection at the
            // cursor): that content is what NORMAL TYPING would
            // have replaced, and undo should restore the typed
            // character at that position, not the selected content
            // back.
            //
            // For full-match replacement (`--` → `—`, no selection):
            //   output_from=6, from=7 → pre = "-", typed = "-" →
            //   undo text = "--". ✓
            // For capture-group replacement (smart-quote):
            //   output_from=7, from=7 → pre = "", typed = `"` →
            //   undo text = `"`. ✓
            // For `--` over selection "X" (selecting X, typing -):
            //   output_from=6, from=7 → pre = "-" (just the dash,
            //   NOT the X), typed = "-" → undo text = "--". ✓
            //   Without this distinction undo would resurrect the
            //   selected X.
            // Suppress the unused-variable warning since the multi-
            // step extension uses replaced_old_size in a later phase.
            let _ = replaced_old_size;
            let pre_consumed_len = from.saturating_sub(output_from);
            let pre_replaced = pre_rule_replaced_text(state, output_from, pre_consumed_len);
            let original_text = format!("{pre_replaced}{text}");

            return Some(RuleFire {
                transaction,
                from: output_from,
                to: output_to,
                text: original_text,
                undoable: rule.options.undoable,
            });
        }
    }

    None
}

/// Extract the text in the pre-rule doc at `[output_from,
/// output_from + replaced_size]`. Used to reconstruct the undo's
/// "original text" payload — what to put back at the rule's
/// output range after Backspace.
fn pre_rule_replaced_text(state: &EditorState, output_from: usize, replaced_size: usize) -> String {
    if replaced_size == 0 {
        return String::new();
    }
    let Ok(resolved) = state.doc().resolve(output_from) else {
        return String::new();
    };
    let parent = resolved.parent();
    let parent_start = output_from - resolved.parent_offset();
    let parent_from = output_from.saturating_sub(parent_start);
    let parent_to = (output_from + replaced_size).saturating_sub(parent_start);
    parent
        .text_between(parent_from, parent_to, "")
        .unwrap_or_default()
}

fn rule_applies(rule: &InputRule, parent_is_code: bool, in_code_mark: bool) -> bool {
    let in_code_node_ok = match rule.options.in_code {
        InCodePolicy::Disallow => !parent_is_code,
        InCodePolicy::Allow => true,
        InCodePolicy::Only => parent_is_code,
    };
    if !in_code_node_ok {
        return false;
    }
    match rule.options.in_code_mark {
        InCodePolicy::Disallow => !in_code_mark,
        InCodePolicy::Allow => true,
        InCodePolicy::Only => in_code_mark,
    }
}

fn parent_is_textblock(state: &EditorState, parent: &crate::model::Node) -> bool {
    state
        .schema()
        .node_type(parent.type_name())
        .map(|nt| nt.inline_content(state.schema()))
        .unwrap_or(false)
}

fn node_is_code(state: &EditorState, parent: &crate::model::Node) -> bool {
    state
        .schema()
        .node_type(parent.type_name())
        .map(|nt| nt.is_code())
        .unwrap_or(false)
}

/// `code` is the canonical mark name for inline-code (schema_basic +
/// most PM-derived schemas). The mark spec doesn't carry a "code"
/// flag the way node-types do; we identify by name.
fn is_code_mark(mark_type_name: &str) -> bool {
    mark_type_name == "code"
}
