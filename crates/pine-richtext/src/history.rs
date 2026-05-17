//! Undo / redo for pine-richtext.
//!
//! A Rust port of `prosemirror-history`. The shape is simpler than
//! upstream — pine doesn't use rope sequences and doesn't merge adjacent
//! edits in v1. Each transaction becomes one history event; undo pops the
//! latest event, applies the previously-inverted steps in reverse, and
//! pushes the event onto the redo branch.
//!
//! History state lives in a [`pine_richtext::state::Plugin`] keyed
//! `"history"`. The plugin's state is JSON (every step round-trips
//! through [`Step::to_json`] / [`Step::from_json`]), so it survives the
//! editor-state-to-JSON serialization that pocopine apps rely on.
//!
//! The integration shape:
//!
//! ```ignore
//! use pine_richtext::history::{history_plugin, undo, redo};
//!
//! let state = EditorState::create(
//!     EditorStateConfig::new(schema, doc).plugins(vec![history_plugin()]),
//! )?;
//!
//! // User types something. Then:
//! if let Some(tr) = undo().apply(&state) {
//!     state = state.apply(tr)?;
//! }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::commands::{BoxedCommand, Command};
use crate::error::{RichTextError, RichTextResult};
use crate::state::{EditorState, Plugin, SelectionBookmark, Transaction};
use crate::transform::Step;

/// Plugin key used for the history state field.
pub const HISTORY_KEY: &str = "history";

/// Meta key set on transactions that originate from `undo` or `redo`.
/// The plugin's apply hook skips recording when this key is `true`.
pub const HISTORY_META: &str = "history";

/// One undoable event: the inverted steps of a single transaction plus
/// the selection that should be restored after applying them. Stored
/// internally as JSON so it round-trips through the plugin's state field.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Event {
    /// Inverted steps in the order they were produced by the original
    /// transaction. Applying them in REVERSE order undoes the transaction.
    steps: Vec<Value>,
    /// Selection at the START of the original transaction (i.e., where
    /// the cursor should land after `undo`).
    selection: Option<SelectionBookmark>,
}

/// Plugin state — the two branches plus their depths.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct HistoryState {
    done: Vec<Event>,
    undone: Vec<Event>,
}

impl HistoryState {
    fn from_value(value: &Value) -> RichTextResult<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone()).map_err(RichTextError::from)
    }

    fn to_value(&self) -> RichTextResult<Value> {
        serde_json::to_value(self).map_err(RichTextError::from)
    }
}

/// Build the history plugin. Install it via `EditorStateConfig::plugins`.
///
/// The plugin records inverted steps for every non-history transaction
/// it sees on a state, exposing them to [`undo`] and [`redo`].
pub fn history_plugin() -> Plugin {
    Plugin::builder(HISTORY_KEY)
        .state_field(
            |_state| HistoryState::default().to_value(),
            |transaction, value, old_state, _new_state| {
                let mut state = HistoryState::from_value(value)?;

                // Skip recording for history transactions themselves.
                let is_history_tx = transaction
                    .meta(HISTORY_META)
                    .and_then(Value::as_str)
                    .is_some();
                if is_history_tx {
                    apply_history_meta(&mut state, transaction)?;
                    return state.to_value();
                }

                // Non-history transaction: invert each step against the
                // doc state it was applied to and record the event.
                let steps_after = transaction.transform().steps().to_vec();
                if steps_after.is_empty() {
                    return state.to_value();
                }

                let docs = transaction.transform().docs();
                let mut inverted_json: Vec<Value> = Vec::with_capacity(steps_after.len());
                for (i, step) in steps_after.iter().enumerate() {
                    let before = docs.get(i).ok_or_else(|| {
                        RichTextError::Transform(
                            "history: missing intermediate doc for step".to_string(),
                        )
                    })?;
                    let inverted = step.invert(before, old_state.schema())?;
                    inverted_json.push(inverted.to_json());
                }

                let event = Event {
                    steps: inverted_json,
                    selection: Some(old_state.selection().bookmark()),
                };
                state.done.push(event);
                // Any new edit clears the redo branch — standard editor
                // semantics.
                state.undone.clear();

                state.to_value()
            },
        )
        .finish()
}

/// Apply the history meta. `undo` and `redo` set the meta to their name;
/// the plugin pops the just-consumed event off the originating branch
/// and pushes the inverse event (carried via `history_payload`) onto the
/// opposite branch.
fn apply_history_meta(state: &mut HistoryState, tr: &Transaction) -> RichTextResult<()> {
    let label = tr.meta(HISTORY_META).and_then(Value::as_str).unwrap_or("");
    let payload: Option<Event> = tr
        .meta("history_payload")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()?;
    match (label, payload) {
        ("undo", Some(event)) => {
            state.done.pop();
            state.undone.push(event);
        }
        ("redo", Some(event)) => {
            state.undone.pop();
            state.done.push(event);
        }
        _ => {}
    }
    Ok(())
}

/// Pop the most recent event off the `done` branch, build a transaction
/// that applies the inverted steps in reverse, and stash the would-be
/// redo event in the transaction's meta so the plugin can move it onto
/// the `undone` branch.
pub fn undo() -> BoxedCommand {
    Box::new(UndoRedo { dir: Dir::Undo })
}

/// Pop the most recent event off the `undone` branch and replay it.
pub fn redo() -> BoxedCommand {
    Box::new(UndoRedo { dir: Dir::Redo })
}

#[derive(Clone, Copy)]
enum Dir {
    Undo,
    Redo,
}

struct UndoRedo {
    dir: Dir,
}

impl Command for UndoRedo {
    fn apply(&self, state: &EditorState) -> Option<Transaction> {
        let raw = state.plugin_state(HISTORY_KEY)?;
        let history = HistoryState::from_value(raw).ok()?;
        let (branch, label) = match self.dir {
            Dir::Undo => (&history.done, "undo"),
            Dir::Redo => (&history.undone, "redo"),
        };
        let event = branch.last()?.clone();

        // Build a transaction whose steps undo (or redo) the recorded event.
        // PM's history applies the inverted steps to walk the doc back; pine
        // does the same by re-inverting against the current doc.
        let mut tr = state.tr();
        let mut tracked_doc = state.doc().clone();
        let mut redo_steps: Vec<Value> = Vec::with_capacity(event.steps.len());
        // Apply inverted steps in REVERSE order — each one undoes one
        // forward step of the original transaction.
        for step_json in event.steps.iter().rev() {
            let step = Step::from_json(state.schema(), step_json.clone()).ok()?;
            // Record the inverse-of-the-inverse so the OTHER branch can
            // replay this when toggled.
            let reinverted = step.invert(&tracked_doc, state.schema()).ok()?;
            let applied = step.apply(&tracked_doc, state.schema()).ok()?;
            tracked_doc = applied.doc;
            redo_steps.push(reinverted.to_json());
            tr.step(step).ok()?;
        }

        // Restore the selection if one was recorded.
        if let Some(bookmark) = &event.selection {
            let resolved = bookmark.resolve(tr.doc()).ok()?;
            tr.set_selection(resolved).ok()?;
        }

        // Stash the OTHER event in meta for the plugin's apply hook.
        let payload = Event {
            steps: redo_steps,
            selection: Some(state.selection().bookmark()),
        };
        tr.set_meta(HISTORY_META, json!(label));
        tr.set_meta("history_payload", serde_json::to_value(&payload).ok()?);

        Some(tr)
    }
}

/// Total number of undoable events on the `done` branch.
pub fn undo_depth(state: &EditorState) -> usize {
    let Some(raw) = state.plugin_state(HISTORY_KEY) else {
        return 0;
    };
    HistoryState::from_value(raw)
        .map(|s| s.done.len())
        .unwrap_or(0)
}

/// Total number of redoable events on the `undone` branch.
pub fn redo_depth(state: &EditorState) -> usize {
    let Some(raw) = state.plugin_state(HISTORY_KEY) else {
        return 0;
    };
    HistoryState::from_value(raw)
        .map(|s| s.undone.len())
        .unwrap_or(0)
}
