//! [`input_rules`] plugin factory and the [`undo_input_rule`]
//! command.
//!
//! Mirrors `prosemirror-inputrules/src/inputrules.ts::inputRules` and
//! `::undoInputRule`. The plugin keeps a one-frame memory of "did the
//! last transaction come from an input rule?" — when yes,
//! [`undo_input_rule`] (typically bound to Backspace right after the
//! rule fires) restores the original typed text.
//!
//! The plugin state is stored as JSON via the existing
//! [`crate::state::Plugin`] state-field API. Specifically:
//!
//! ```text
//! {
//!   "from": <usize>,           // model pos before the rule fired
//!   "to":   <usize>,           // model pos after the rule fired
//!   "text": <string>           // original text replaced by the rule
//! }
//! ```
//!
//! When no rule has fired (or the user has typed since the last
//! rule), the state is `null`.

use serde_json::{Value, json};

use crate::commands::BoxedCommand;
use crate::model::Fragment;
use crate::state::{EditorState, Plugin, Transaction};

/// Plugin key the [`input_rules`] factory registers under. Apps that
/// want to invalidate the rule via meta on a transaction can set
/// `tr.meta(INPUT_RULES_PLUGIN_KEY)` to `Value::Null`.
pub const INPUT_RULES_PLUGIN_KEY: &str = "pine_richtext_input_rules";

/// Plugin-state field name in serialized state JSON. Matches the
/// plugin key today; the alias exists so future restructuring can
/// rename one without breaking the other.
const STATE_FIELD: &str = INPUT_RULES_PLUGIN_KEY;

/// Build the input-rules state-tracking plugin.
///
/// The plugin's only job is to remember the last-applied rule's
/// `{from, to, text}` (as JSON) across one transaction frame so
/// [`undo_input_rule`] can restore the original typed text after
/// Backspace.
///
/// **The plugin does NOT carry the rules themselves.** Rules are
/// contributed by `RichTextExtension::input_rules()` (Phase 5 C2)
/// and the view-side `beforeinput` hook reads them from the
/// resolved runtime, not the plugin. Keeping the plugin rule-list-
/// free means the same plugin instance serves every runtime — no
/// cloning required per surface mount.
pub fn input_rules() -> Plugin {
    Plugin::builder(INPUT_RULES_PLUGIN_KEY)
        .state_field(
            |_state: &EditorState| Ok(Value::Null),
            |tr: &Transaction,
             prev: &Value,
             old: &EditorState,
             new: &EditorState|
             -> crate::RichTextResult<Value> {
                // If the dispatch carried explicit plugin meta, use
                // it (the rule-fire path sets this via
                // `rule_fire_meta` after a rule matches).
                if let Some(stored) = tr.meta(STATE_FIELD) {
                    return Ok(stored.clone());
                }
                // Doc-changing transactions invalidate the prior
                // rule's memory — Backspace right after a rule
                // means "undo the rule;" Backspace later means
                // "delete a character."
                if !tr.transform().steps().is_empty() {
                    return Ok(Value::Null);
                }
                // Selection-changing transactions also invalidate.
                // Compare old vs new selection rather than checking
                // `tr.selection().is_some()` — `state.tr()` pre-
                // populates `Transaction.selection`, so the Option-
                // presence check would also wipe state on meta-only
                // transactions (incorrect: a no-op selection
                // refresh shouldn't forget the rule).
                if old.selection() != new.selection() {
                    return Ok(Value::Null);
                }
                Ok(prev.clone())
            },
        )
        .finish()
}

/// Build the rule-fire meta value the input-rules view hook attaches
/// to its dispatched transaction. The plugin's state-field `apply`
/// reads this through `tr.meta(INPUT_RULES_PLUGIN_KEY)`.
pub fn rule_fire_meta(from: usize, to: usize, text: &str) -> Value {
    json!({
        "from": from,
        "to": to,
        "text": text,
    })
}

/// Command: roll back the most recent input-rule fire. Typically
/// bound to Backspace so a user hitting Backspace right after a
/// rule fires gets their original typed text back instead of
/// deleting a character of the rule's output.
///
/// Returns `None` (command-not-applicable) when no rule has fired
/// since the last document edit / selection change.
pub fn undo_input_rule() -> BoxedCommand {
    Box::new(|state: &EditorState| -> Option<Transaction> {
        let stored = state.plugin_state(INPUT_RULES_PLUGIN_KEY)?;
        if stored.is_null() {
            return None;
        }
        let from = stored.get("from")?.as_u64()? as usize;
        let to = stored.get("to")?.as_u64()? as usize;
        let text = stored.get("text")?.as_str()?.to_string();

        let mut tr = state.tr();
        // Replace the rule's emitted output with the original text
        // using explicit-position `replace_with` (not the selection-
        // relative `insert_text` / `delete`+`insert_text` pair). The
        // rule's `from..to` range was recorded at fire time; that's
        // where we want to splice the original text back in,
        // regardless of where the state's selection happens to be.
        // Block-level rules (`wrapping_input_rule`,
        // `textblock_type_input_rule`) carry their own inverse via
        // C2/C3 when those builders land.
        let current_doc_size = tr.doc().content_size();
        let clamped_to = to.min(current_doc_size);
        let clamped_from = from.min(clamped_to);
        // Inherit marks from the cursor's resolved position so the
        // restored text doesn't strip formatting at a mark boundary.
        let marks = state
            .doc()
            .resolve(clamped_to)
            .map(|resolved| resolved.marks(state.schema()))
            .unwrap_or_default();
        let content = if text.is_empty() {
            Fragment::empty()
        } else {
            Fragment::from(state.schema().text(text, marks).ok()?)
        };
        tr.replace_with(clamped_from, clamped_to, content).ok()?;
        // Clear the plugin's memory so a second Backspace deletes
        // characters normally instead of looping the undo.
        tr.set_meta(STATE_FIELD, Value::Null);
        Some(tr)
    })
}
