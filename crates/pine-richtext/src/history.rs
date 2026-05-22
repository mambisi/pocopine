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

/// Meta key on transactions carrying a wall-clock millisecond
/// timestamp from the dispatch site. History reads this to
/// decide whether to merge a new event into the previous one
/// (rapid typing → one undo step) and to drop merging when the
/// caller is operating in non-wasm contexts where no clock is
/// plugged in.
///
/// Set by `pine_richtext::view::root::dispatch` to
/// `performance.now()`; tests may set it explicitly, and
/// transactions without it skip the merge step entirely
/// (still recorded, just never merged).
pub const HISTORY_COMMIT_MS_META: &str = "commit_ms";

/// Cap on `history.done.len()`. Matches the PM default. The
/// `undone` branch isn't capped — it only grows from undos
/// which the user has already paid for.
const HISTORY_DEPTH: usize = 100;

/// Window during which adjacent typing events get folded into
/// the same undo step. Matches PM's `newGroupDelay`. Long
/// enough that "type a sentence" undoes as one step; short
/// enough that "type, pause, type" undoes in two.
const MERGE_WINDOW_MS: u64 = 500;

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
    /// Slow-path deserializer used by `undo` / `redo` to read the
    /// branches. The hot path (per-keystroke `apply`) mutates the
    /// underlying Value directly via `apply_history_value_in_place`
    /// and never calls this.
    fn from_value(value: &Value) -> RichTextResult<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone()).map_err(RichTextError::from)
    }
}

/// Build the history plugin. Install it via `EditorStateConfig::plugins`.
///
/// The plugin records inverted steps for every non-history transaction
/// it sees on a state, exposing them to [`undo`] and [`redo`].
///
/// Uses `state_field_in_place` so each commit can push one event
/// directly into the Value's `done` array (O(1) amortized) instead of
/// running `serde_json::from_value` + `to_value` over the entire
/// (growing) history on every keystroke. The earlier round-trip form
/// turned typing in long sessions quadratic — see
/// `tests/playwright/richtext.perf.spec.mjs` for the regression
/// scenario.
pub fn history_plugin() -> Plugin {
    Plugin::builder(HISTORY_KEY)
        .state_field_in_place(
            |_state| Ok(json!({ "done": [], "undone": [] })),
            |transaction, value, old_state, _new_state| {
                apply_history_value_in_place(value, transaction, old_state)
            },
        )
        .finish()
}

/// Mutate the plugin's Value in place to reflect `transaction`. The
/// hot path (every typed character) is the `else` branch: it pushes
/// one event JSON to `done` and clears `undone`. The
/// undo/redo meta branch pops one event from the source branch and
/// pushes the inverse payload onto the other.
fn apply_history_value_in_place(
    value: &mut Value,
    transaction: &Transaction,
    old_state: &EditorState,
) -> RichTextResult<()> {
    // Ensure the value has the expected shape. New states initialize
    // through `init`, but defensive normalization is cheap.
    if !value.is_object() {
        *value = json!({ "done": [], "undone": [] });
    }

    let is_history_tx = transaction
        .meta(HISTORY_META)
        .and_then(Value::as_str)
        .is_some();

    if is_history_tx {
        let label = transaction
            .meta(HISTORY_META)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let payload = transaction.meta("history_payload").cloned();
        let obj = value.as_object_mut().unwrap();
        match label.as_str() {
            "undo" => {
                if let Some(done) = obj.get_mut("done").and_then(Value::as_array_mut) {
                    done.pop();
                }
                if let Some(payload) = payload {
                    push_value_array(obj, "undone", payload);
                }
            }
            "redo" => {
                if let Some(undone) = obj.get_mut("undone").and_then(Value::as_array_mut) {
                    undone.pop();
                }
                if let Some(payload) = payload {
                    push_value_array(obj, "done", payload);
                }
            }
            _ => {}
        }
        return Ok(());
    }

    let steps_after = transaction.transform().steps();
    if steps_after.is_empty() {
        return Ok(());
    }

    let docs = transaction.transform().docs();
    let mut inverted_json: Vec<Value> = Vec::with_capacity(steps_after.len());
    for (i, step) in steps_after.iter().enumerate() {
        let before = docs.get(i).ok_or_else(|| {
            RichTextError::Transform("history: missing intermediate doc for step".to_string())
        })?;
        let inverted = step.invert(before, old_state.schema())?;
        inverted_json.push(inverted.to_json());
    }

    let is_typing = forward_steps_are_typing(steps_after);
    let now_ms = transaction
        .meta(HISTORY_COMMIT_MS_META)
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let event = json!({
        "steps": inverted_json,
        "selection": old_state.selection().bookmark(),
        "is_typing": is_typing,
        "ms": now_ms,
    });

    let obj = value.as_object_mut().unwrap();
    let done = obj.entry("done".to_string()).or_insert_with(|| json!([]));
    if !done.is_array() {
        *done = json!([]);
    }
    let done_arr = done.as_array_mut().unwrap();
    if !(is_typing && try_merge_into_last(done_arr, &event, now_ms)) {
        done_arr.push(event);
    }
    // Cap depth. Drop from the front (oldest first) one at a
    // time — the loop only runs when push exceeded the cap, so
    // it's amortized O(1).
    while done_arr.len() > HISTORY_DEPTH {
        done_arr.remove(0);
    }
    // Any new edit clears the redo branch — standard editor semantics.
    obj.insert("undone".to_string(), json!([]));
    Ok(())
}

/// True when every step in `steps` is a pure forward
/// insertion (no deletion, no mark / attr changes, short
/// slice). That's the "user is typing" shape — merging two
/// such consecutive events into one undo unit gives
/// keyboard-friendly undo granularity without any model-side
/// step-merging logic.
fn forward_steps_are_typing(steps: &[Step]) -> bool {
    if steps.is_empty() || steps.len() > 4 {
        return false;
    }
    steps.iter().all(|step| match step {
        Step::Replace(rs) => rs.from == rs.to && rs.slice.size() > 0 && rs.slice.size() <= 4,
        _ => false,
    })
}

/// Fold a typing event into the most recent typing event in
/// `done` when they're close enough in time. Returns `true`
/// when a merge happened (caller should NOT push as a new
/// event). Falls back to "not merged" whenever any of the
/// shape checks fail — never silently drops data.
fn try_merge_into_last(done: &mut [Value], new_event: &Value, now_ms: u64) -> bool {
    if now_ms == 0 {
        return false;
    }
    let Some(last) = done.last_mut() else {
        return false;
    };
    if !last
        .get("is_typing")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    let last_ms = last.get("ms").and_then(Value::as_u64).unwrap_or(0);
    if last_ms == 0 || now_ms.saturating_sub(last_ms) > MERGE_WINDOW_MS {
        return false;
    }
    // Append the new event's inverted steps onto the last
    // event's. Undoing the last event then unwinds the whole
    // merged run in one shot.
    if let (Some(last_steps), Some(new_steps)) = (
        last.get_mut("steps").and_then(Value::as_array_mut),
        new_event.get("steps").and_then(Value::as_array),
    ) {
        for step in new_steps.iter().cloned() {
            last_steps.push(step);
        }
    }
    if let Some(obj) = last.as_object_mut() {
        obj.insert("ms".to_string(), json!(now_ms));
    }
    true
}

fn push_value_array(obj: &mut serde_json::Map<String, Value>, key: &str, value: Value) {
    let entry = obj.entry(key.to_string()).or_insert_with(|| json!([]));
    if !entry.is_array() {
        *entry = json!([]);
    }
    entry.as_array_mut().unwrap().push(value);
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
///
/// O(1) — reads the Value array's length directly instead of
/// deserializing the full state, since toolbar code may poll this
/// every render to toggle button enablement.
pub fn undo_depth(state: &EditorState) -> usize {
    branch_len(state, "done")
}

/// Total number of redoable events on the `undone` branch.
pub fn redo_depth(state: &EditorState) -> usize {
    branch_len(state, "undone")
}

fn branch_len(state: &EditorState, key: &str) -> usize {
    state
        .plugin_state(HISTORY_KEY)
        .and_then(|v| v.get(key))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}
