//! Keystroke + beforeinput dispatch.
//!
//! The view installs two listeners on the contentEditable surface:
//! - **`keydown`** — looks the pressed key combo up in a small keymap
//!   built from `crate::commands::*`. Matched keys dispatch the
//!   corresponding command and `preventDefault()` so the browser
//!   doesn't also handle them.
//! - **`beforeinput`** — captures `insertText` events (typed
//!   characters), prevents the default DOM mutation, and inserts the
//!   text via `Transaction::insert_text` so the model stays the source
//!   of truth.
//!
//! Crucially the view does NOT listen on `selectionchange` to push the
//! DOM selection into the model. That would create a feedback loop:
//! every drag-move fires `selectionchange`, every fire dispatches a
//! Transaction, every Transaction re-renders + restores the cursor,
//! which kills the user's in-progress drag. Instead, the
//! [`state_provider`] passed to [`install_listeners`] reads
//! `window.getSelection()` at the moment a command actually needs it
//! and injects the resulting model `Selection` into the
//! reconstructed `EditorState`. Commands that need the live cursor
//! position (`delete_selection`, `toggle_mark`, `insert_text`, etc.)
//! get the up-to-date DOM selection that way.

#[cfg(target_arch = "wasm32")]
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{ClipboardEvent, Element, Event, InputEvent, KeyboardEvent, StaticRange};

use crate::commands::{self, Command};
use crate::inputrules::{plugin as inputrules_plugin, run_rules};
use crate::model::{Node, Slice};
use crate::runtime::EditorRuntime;
use crate::state::{EditorState, Selection, Transaction};

use super::node_view_manager::NodeViewManager;
use super::selection::dom_pos_to_model;

const INPUT_DEBUG_LOG_VERSION: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "@",
    env!("CARGO_PKG_VERSION"),
    ":debug-json-v1"
);

const PINE_SLICE_MIME: &str = "application/x-pine-richtext-slice+json";

/// Lookup table from key combo (e.g. `"Mod-b"`, `"Enter"`, `"Backspace"`)
/// to a boxed command. Built by [`default_keymap`] and consulted on
/// every keydown.
pub struct KeyMap {
    bindings: Vec<(String, Box<dyn Command>)>,
}

impl KeyMap {
    pub fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    pub fn bind(mut self, key: impl Into<String>, cmd: Box<dyn Command>) -> Self {
        self.bindings.push((key.into(), cmd));
        self
    }

    pub fn lookup(&self, key: &str) -> Option<&dyn Command> {
        self.bindings
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, cmd)| cmd.as_ref())
    }
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::new()
    }
}

/// PM-style default keymap for the given runtime. Covers the keys
/// most editors need: Backspace / Delete / Enter, horizontal selectable-inline
/// atom navigation, plus Mod-A for select-all.
///
/// Extensions can contribute extra bindings through
/// [`crate::extension::RichTextExtension::key_bindings`]; those are
/// merged in *after* the base 6 entries below so the base remains
/// inviolable (`KeyMap::lookup` is first-wins). The runtime's
/// extension chain determines which bindings appear — two surfaces
/// with different runtimes can have different keymaps.
pub fn default_keymap(runtime: &crate::runtime::EditorRuntime) -> KeyMap {
    let mut km = base_keymap();
    for (combo, factory) in runtime.merged_keymap_factories() {
        km = km.bind(combo.clone(), factory());
    }
    km
}

/// The 6 hardcoded bindings the framework always installs. Separated
/// from [`default_keymap`] so tests can assert the base set
/// independently of any extension contributions.
fn base_keymap() -> KeyMap {
    KeyMap::new()
        // Backspace tries `undo_input_rule` FIRST so the user's
        // backspace right after a rule fires rolls the rule back
        // (e.g. typing `--` → em-dash → Backspace → `--` again).
        // The command returns `None` when no rule has fired, so the
        // chain falls through to the normal delete path.
        .bind(
            "Backspace",
            commands::chain_commands(vec![
                inputrules_plugin::undo_input_rule(),
                commands::delete_selection(),
                commands::join_backward(),
                // Inline-atom-after-caret: delete it at the model level
                // so the native backspace can't collapse the paragraph
                // into an invalid block-less doc (#243).
                commands::delete_node_backward(),
                commands::select_node_backward(),
            ]),
        )
        .bind(
            "Delete",
            commands::chain_commands(vec![
                commands::delete_selection(),
                commands::join_forward(),
                commands::delete_node_forward(),
                commands::select_node_forward(),
            ]),
        )
        .bind(
            "Enter",
            commands::chain_commands(vec![
                commands::lift_empty_block(),
                commands::split_list_item(&["list_item", "task_item"]),
                commands::split_block(),
            ]),
        )
        .bind(
            "ArrowLeft",
            commands::chain_commands(vec![
                commands::leave_selected_inline_atom_backward(),
                commands::select_inline_atom_backward(),
            ]),
        )
        .bind(
            "ArrowRight",
            commands::chain_commands(vec![
                commands::leave_selected_inline_atom_forward(),
                commands::select_inline_atom_forward(),
            ]),
        )
        .bind("Mod-a", commands::select_all())
}

/// Translate a `KeyboardEvent` into the combo string a [`KeyMap`]
/// expects. Examples: `"a"`, `"Backspace"`, `"Mod-b"`, `"Shift-Enter"`.
/// `Mod` collapses Cmd (Mac) and Ctrl (everywhere else) so apps don't
/// have to register both.
pub fn key_combo(event: &KeyboardEvent) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(4);
    let mod_key = event.meta_key() || event.ctrl_key();
    if mod_key {
        parts.push("Mod");
    }
    if event.shift_key() {
        parts.push("Shift");
    }
    if event.alt_key() {
        parts.push("Alt");
    }
    let key = event.key();
    // Normalize single-character lowercase: "B" with shift becomes
    // "Shift-b" not "Shift-B".
    let key_str = if key.chars().count() == 1 {
        key.to_lowercase()
    } else {
        key
    };
    parts.push(&key_str);
    parts.join("-")
}

fn simple_text_delete_defer_reason(
    surface: &Element,
    event: &KeyboardEvent,
    combo: &str,
) -> &'static str {
    if !matches!(combo, "Backspace" | "Delete") {
        return "not_delete_key";
    }
    if event.meta_key() || event.ctrl_key() || event.alt_key() || event.shift_key() {
        return "modified_delete_key";
    }

    let Some(selection) = web_sys::window().and_then(|w| w.get_selection().ok().flatten()) else {
        return "missing_dom_selection";
    };
    if !selection.is_collapsed() {
        return "selection_not_collapsed";
    }
    let Some(anchor_node) = selection.anchor_node() else {
        return "missing_anchor_node";
    };
    if !surface.contains(Some(&anchor_node)) {
        return "selection_outside_surface";
    }
    if anchor_node.node_type() != web_sys::Node::TEXT_NODE {
        return "anchor_not_text_node";
    }

    let offset = selection.anchor_offset() as usize;
    let text_len = anchor_node
        .node_value()
        .map(|value| value.encode_utf16().count())
        .unwrap_or(0);
    match combo {
        "Backspace" if offset > 0 => "defer_text_delete_to_beforeinput",
        "Backspace" => "at_text_start",
        "Delete" if offset < text_len => "defer_text_delete_to_beforeinput",
        "Delete" => "at_text_end",
        _ => "not_delete_key",
    }
}

/// Wire `keydown`/`beforeinput`/`selectionchange` listeners on the
/// `surface` element. `dispatch` is invoked with the state snapshot the
/// command used plus the transaction it produced; the caller (the
/// component) decides how to commit it (typically: `state.apply(tr)` then
/// write back to its JSON signal).
///
/// The returned vector holds the live `Closure`s — the caller MUST keep
/// it alive (typically by stashing it in the component's state) or the
/// closures get dropped and the listeners stop firing.
pub(crate) fn install_listeners<F>(
    surface: Element,
    runtime: Arc<EditorRuntime>,
    state_provider: Rc<dyn Fn(bool) -> Option<EditorState>>,
    node_view_manager: Rc<RefCell<NodeViewManager>>,
    keymap: Rc<KeyMap>,
    debug_perf: bool,
    dispatch: F,
) -> Vec<Closure<dyn FnMut(Event)>>
where
    F: Fn(EditorState, Transaction, bool) + 'static,
{
    let dispatch = Rc::new(dispatch);
    let runtime_name = runtime.name().map(str::to_string);

    let mut closures: Vec<Closure<dyn FnMut(Event)>> = Vec::new();

    // An unconsumed press on component shell chrome selects the semantic
    // node. Form controls and the exact owned editable outlet keep their
    // browser/component behavior. `mousedown` is deliberate: existing view
    // controls use `@mousedown.prevent`, so Pine observes that consumption
    // before adding a selection default.
    {
        let state_provider = state_provider.clone();
        let dispatch = dispatch.clone();
        let manager = node_view_manager.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            let boundary = manager.borrow().event_boundary(&event);
            let Some(position) = boundary.pointer_selection(event.default_prevented()) else {
                return;
            };
            let Some(state) = state_provider(false) else {
                return;
            };
            let mut transaction = state.tr();
            if transaction
                .set_selection(Selection::node(position))
                .is_err()
            {
                return;
            }
            event.prevent_default();
            dispatch(state, transaction, true);
        }) as Box<dyn FnMut(Event)>);
        let _ = surface.add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref());
        closures.push(cb);
    }

    // keydown — keymap lookup.
    {
        let state_provider = state_provider.clone();
        let keymap = keymap.clone();
        let dispatch = dispatch.clone();
        let runtime_for_log = runtime_name.clone();
        let surface_for_keydown = surface.clone();
        let manager = node_view_manager.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            if !editor_owns_event(&event, &manager) {
                return;
            }
            let started_at = perf_now_ms();
            let Ok(ev) = event.clone().dyn_into::<KeyboardEvent>() else {
                return;
            };
            let combo = key_combo(&ev);
            let Some(cmd) = keymap.lookup(&combo) else {
                return;
            };
            let delete_defer_reason =
                simple_text_delete_defer_reason(&surface_for_keydown, &ev, &combo);
            let delete_defer_reason_value = if matches!(combo.as_str(), "Backspace" | "Delete") {
                json!(delete_defer_reason)
            } else {
                Value::Null
            };
            if delete_defer_reason == "defer_text_delete_to_beforeinput" {
                log_input_perf(debug_perf, "input.keydown", || {
                    json!({
                        "runtime": runtime_for_log.clone(),
                        "combo": combo.clone(),
                        "handled": false,
                        "reason": "defer_text_delete_to_beforeinput",
                        "total_ms": round_ms(perf_now_ms() - started_at),
                    })
                });
                return;
            }
            let state_started_at = perf_now_ms();
            let Some(state) = state_provider(true) else {
                log_input_perf(debug_perf, "input.keydown", || {
                    json!({
                        "runtime": runtime_for_log,
                        "combo": combo,
                        "handled": false,
                        "reason": "missing_state",
                        "delete_defer_reason": delete_defer_reason_value,
                        "state_ms": round_ms(perf_now_ms() - state_started_at),
                        "total_ms": round_ms(perf_now_ms() - started_at),
                    })
                });
                return;
            };
            let command_started_at = perf_now_ms();
            let Some(tr) = cmd.apply(&state) else {
                // Pine owns this contenteditable surface. If none of the
                // semantic delete commands can represent an edit at a text
                // boundary, consuming the key is the only safe no-op. Letting
                // the browser continue would mutate component/table DOM while
                // leaving the document model unchanged.
                let consumed_boundary_delete =
                    matches!(delete_defer_reason, "at_text_start" | "at_text_end");
                if consumed_boundary_delete {
                    ev.prevent_default();
                }
                log_input_perf(debug_perf, "input.keydown", || {
                    json!({
                        "runtime": runtime_for_log,
                        "combo": combo,
                        "handled": consumed_boundary_delete,
                        "reason": if consumed_boundary_delete {
                            "unmodeled_boundary_delete_consumed"
                        } else {
                            "command_none"
                        },
                        "delete_defer_reason": delete_defer_reason_value,
                        "state_ms": round_ms(command_started_at - state_started_at),
                        "command_ms": round_ms(perf_now_ms() - command_started_at),
                        "total_ms": round_ms(perf_now_ms() - started_at),
                    })
                });
                return;
            };
            ev.prevent_default();
            log_input_perf(debug_perf, "input.keydown", || {
                json!({
                    "runtime": runtime_for_log.clone(),
                    "combo": combo.clone(),
                    "handled": true,
                    "steps": tr.transform().steps().len(),
                    "state_ms": round_ms(command_started_at - state_started_at),
                    "command_ms": round_ms(perf_now_ms() - command_started_at),
                    "total_before_dispatch_ms": round_ms(perf_now_ms() - started_at),
                })
            });
            dispatch(state, tr, true);
            log_input_perf(debug_perf, "input.keydown.complete", || {
                json!({
                    "runtime": runtime_for_log.clone(),
                    "combo": combo.clone(),
                    "total_after_dispatch_ms": round_ms(perf_now_ms() - started_at),
                })
            });
            schedule_next_frame_perf(
                debug_perf,
                "input.next_frame",
                || {
                    json!({
                        "runtime": runtime_for_log.clone(),
                        "source": "keydown",
                        "combo": combo,
                    })
                },
                started_at,
            );
        }) as Box<dyn FnMut(Event)>);
        let _ = surface.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
        closures.push(cb);
    }

    // beforeinput — capture typed characters AND deletions. We can't
    // rely on the keymap chain for "Backspace in the middle of text"
    // because none of the standard commands handle that case (it's
    // PM's "browser default" path). If we let the browser delete the
    // char from the contentEditable surface, the model stays out of
    // sync and the next keystroke runs against stale text — the
    // classic "deleted character reappears" bug.
    //
    // The fix: handle every `delete*` input type here, using
    // `InputEvent.getTargetRanges()` to learn what the browser would
    // have deleted. We translate those DOM ranges into a model
    // delete transaction so the model stays authoritative.
    {
        let surface_for_input = surface.clone();
        let state_provider = state_provider.clone();
        let dispatch = dispatch.clone();
        let runtime_for_input = runtime.clone();
        let runtime_for_log = runtime_name.clone();
        let manager = node_view_manager.clone();
        let keymap_for_input = keymap.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            if !editor_owns_event(&event, &manager) {
                return;
            }
            let started_at = perf_now_ms();
            let Ok(ev) = event.clone().dyn_into::<InputEvent>() else {
                return;
            };
            let input_type = ev.input_type();
            match input_type.as_str() {
                // A soft keyboard's return key frequently produces no `keydown`
                // at all (or an unidentifiable `keyCode 229` one), so the
                // keymap never sees it and only this event arrives. Route it to
                // the same binding the physical key uses — otherwise the floor
                // arm below would refuse it and Enter would simply stop working
                // on mobile, which is the one regression this change must not
                // introduce.
                "insertParagraph" | "insertLineBreak" => {
                    ev.prevent_default();
                    let combo = if input_type == "insertLineBreak" {
                        "Shift-Enter"
                    } else {
                        "Enter"
                    };
                    // Fall back to the plain Enter binding: `Shift-Enter` is not
                    // in the base keymap, so an extension-less runtime would
                    // otherwise drop a soft line break entirely.
                    let cmd = keymap_for_input
                        .lookup(combo)
                        .or_else(|| keymap_for_input.lookup("Enter"));
                    let handled = (|| {
                        let cmd = cmd?;
                        let state = state_provider(true)?;
                        let tr = cmd.apply(&state)?;
                        dispatch(state, tr, true);
                        Some(())
                    })()
                    .is_some();
                    log_input_perf(debug_perf, "input.beforeinput", || {
                        json!({
                            "runtime": runtime_for_log.clone(),
                            "input_type": input_type.clone(),
                            "handled": handled,
                            "combo": combo,
                            "total_ms": round_ms(perf_now_ms() - started_at),
                        })
                    });
                }
                // `insertReplacementText` is the same edit as `insertText` with
                // a non-collapsed target range: the browser hands us the span to
                // replace via `getTargetRanges()` and the replacement via the
                // event, which is exactly what the machinery below already does
                // for a typed character. Autocorrect on a soft keyboard, the
                // desktop spellcheck menu's "correct to", and iOS predictive
                // replacement all arrive this way.
                "insertText" | "insertReplacementText" => {
                    let is_replacement = input_type == "insertReplacementText";
                    // A replacement carries its text on `dataTransfer` in some
                    // engines and on `data` in others; a typed character only
                    // ever uses `data`.
                    let data = if is_replacement {
                        ev.data_transfer()
                            .and_then(|dt| dt.get_data("text/plain").ok())
                            .filter(|s| !s.is_empty())
                            .or_else(|| ev.data())
                    } else {
                        ev.data()
                    };
                    let Some(data) = data else {
                        // Nothing to insert, but this surface is controlled:
                        // returning without preventing would hand the edit to
                        // the browser.
                        ev.prevent_default();
                        return;
                    };
                    if data.is_empty() {
                        ev.prevent_default();
                        return;
                    }
                    let data_chars = data.chars().count();
                    let range_started_at = perf_now_ms();
                    let target_range = target_range_only_to_model(&surface_for_input, &ev);
                    let target_range_found = target_range.is_some();
                    let mut target_range_applied = false;
                    let range_ms = perf_now_ms() - range_started_at;
                    let state_started_at = perf_now_ms();
                    let Some(mut state) = state_provider(!target_range_found) else {
                        log_input_perf(debug_perf, "input.beforeinput", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type.clone(),
                                "handled": false,
                                "reason": "missing_state",
                                "target_range": target_range_found,
                                "range_ms": round_ms(range_ms),
                                "state_ms": round_ms(perf_now_ms() - state_started_at),
                                "total_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        return;
                    };
                    if let Some((from, to)) = target_range {
                        if let Some(next) = state_with_text_selection(state, from, to) {
                            state = next;
                            target_range_applied = true;
                        } else if let Some(live_state) = state_provider(true) {
                            state = live_state;
                        } else {
                            log_input_perf(debug_perf, "input.beforeinput", || {
                                json!({
                                    "runtime": runtime_for_log.clone(),
                                    "input_type": input_type.clone(),
                                    "handled": false,
                                    "reason": "target_selection_error",
                                    "target_range": true,
                                    "range_ms": round_ms(range_ms),
                                    "state_ms": round_ms(perf_now_ms() - state_started_at),
                                    "total_ms": round_ms(perf_now_ms() - started_at),
                                })
                            });
                            return;
                        }
                    }

                    // Consult input rules FIRST. If any rule matches
                    // the text immediately preceding the cursor plus
                    // the just-typed character, dispatch the rule's
                    // transaction and skip the default insert.
                    let cursor_from = state.selection().from(state.doc());
                    let cursor_to = state.selection().to(state.doc());
                    let rules = runtime_for_input.input_rules();
                    // Rules are an as-you-type affordance keyed on the character
                    // the user just pressed. A spellcheck/autocorrect
                    // replacement is not a keystroke, so firing them here would
                    // let a correction silently trigger (say) the em-dash rule.
                    if !is_replacement
                        && !state.selection().is_cells()
                        && !rules.is_empty()
                        && let Some(fire) = run_rules(&state, cursor_from, cursor_to, &data, rules)
                    {
                        let mut tr = fire.transaction;
                        if fire.undoable {
                            tr.set_meta(
                                inputrules_plugin::INPUT_RULES_PLUGIN_KEY,
                                inputrules_plugin::rule_fire_meta(fire.from, fire.to, &fire.text),
                            );
                        }
                        ev.prevent_default();
                        log_input_perf(debug_perf, "input.beforeinput", || {
                            json!({
                                "runtime": runtime_for_log,
                                "input_type": input_type,
                                "handled": true,
                                "rule": true,
                                "data_chars": data_chars,
                                "target_range": target_range_found,
                                "target_range_applied": target_range_applied,
                                "range_ms": round_ms(range_ms),
                                "state_ms": round_ms(perf_now_ms() - state_started_at),
                                "total_before_dispatch_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        let input_type_for_log = input_type.clone();
                        dispatch(state, tr, false);
                        log_input_perf(debug_perf, "input.beforeinput.complete", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type_for_log.clone(),
                                "rule": true,
                                "total_after_dispatch_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        schedule_next_frame_perf(
                            debug_perf,
                            "input.next_frame",
                            || {
                                json!({
                                    "runtime": runtime_for_log.clone(),
                                    "source": "beforeinput",
                                    "input_type": input_type_for_log,
                                    "rule": true,
                                })
                            },
                            started_at,
                        );
                        return;
                    }

                    ev.prevent_default();
                    let transaction_started_at = perf_now_ms();
                    if let Some(tr) = insert_text_transaction(&state, data) {
                        log_input_perf(debug_perf, "input.beforeinput", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type.clone(),
                                "handled": true,
                                "rule": false,
                                "data_chars": data_chars,
                                "target_range": target_range_found,
                                "target_range_applied": target_range_applied,
                                "range_ms": round_ms(range_ms),
                                "state_ms": round_ms(transaction_started_at - state_started_at),
                                "transaction_ms": round_ms(perf_now_ms() - transaction_started_at),
                                "total_before_dispatch_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        let input_type_for_log = input_type.clone();
                        dispatch(state, tr, false);
                        log_input_perf(debug_perf, "input.beforeinput.complete", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type_for_log.clone(),
                                "rule": false,
                                "total_after_dispatch_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        schedule_next_frame_perf(
                            debug_perf,
                            "input.next_frame",
                            || {
                                json!({
                                    "runtime": runtime_for_log.clone(),
                                    "source": "beforeinput",
                                    "input_type": input_type_for_log,
                                    "rule": false,
                                })
                            },
                            started_at,
                        );
                    } else {
                        log_input_perf(debug_perf, "input.beforeinput", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type.clone(),
                                "handled": false,
                                "reason": "transaction_none",
                                "data_chars": data_chars,
                                "target_range": target_range_found,
                                "target_range_applied": target_range_applied,
                                "range_ms": round_ms(range_ms),
                                "state_ms": round_ms(transaction_started_at - state_started_at),
                                "transaction_ms": round_ms(perf_now_ms() - transaction_started_at),
                                "total_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                    }
                }
                // Drag-and-drop is not implemented, and it arrives as two
                // independent events: the source deletion (`deleteByDrag`) and
                // the destination insertion (`insertFromDrop`). Committing only
                // the deletion — which is what listing it below used to do —
                // removed the dragged text from the model while the browser
                // painted it at a destination the model never learned about.
                // That is net content loss on a common gesture, worse than
                // either half alone. Refuse both halves so a drag is inert
                // until it is implemented for real, which needs a
                // client-coordinates → model-position mapping this crate does
                // not have yet (`insertFromDrop` is refused by the floor arm).
                "deleteByDrag" => {
                    ev.prevent_default();
                    log_input_perf(debug_perf, "input.beforeinput", || {
                        json!({
                            "runtime": runtime_for_log.clone(),
                            "input_type": input_type.clone(),
                            "handled": false,
                            "reason": "drag_drop_unsupported_refused",
                            "total_ms": round_ms(perf_now_ms() - started_at),
                        })
                    });
                }
                "deleteContentBackward"
                | "deleteContentForward"
                | "deleteWordBackward"
                | "deleteWordForward"
                | "deleteSoftLineBackward"
                | "deleteSoftLineForward"
                | "deleteHardLineBackward"
                | "deleteHardLineForward"
                | "deleteContent"
                | "deleteByCut" => {
                    // Never let native deletion mutate this controlled DOM.
                    // Every recognized delete is either committed through a
                    // model transaction below or deliberately remains a no-op.
                    // This also protects non-keyboard deletion when a browser
                    // target range crosses an isolating node boundary.
                    ev.prevent_default();
                    let range_started_at = perf_now_ms();
                    let target_range = target_range_only_to_model(&surface_for_input, &ev);
                    let target_range_found = target_range.is_some();
                    let mut target_range_applied = false;
                    let target_range_ms = perf_now_ms() - range_started_at;
                    let state_started_at = perf_now_ms();
                    let Some(mut state) = state_provider(!target_range_found) else {
                        log_input_perf(debug_perf, "input.beforeinput", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type.clone(),
                                "handled": false,
                                "reason": "missing_state",
                                "target_range": target_range_found,
                                "range_ms": round_ms(target_range_ms),
                                "state_ms": round_ms(perf_now_ms() - state_started_at),
                                "total_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        return;
                    };
                    // `getTargetRanges()` tells us both what the browser would
                    // delete and where its live selection is. When that range
                    // is available we intentionally skipped the slower DOM
                    // selection read above, so install it on the transient
                    // state before building the transaction. Otherwise the
                    // delete step maps whatever stale selection the cache last
                    // held (often the document start), and the subsequent DOM
                    // cursor sync visibly jumps there.
                    if let Some((from, to)) = target_range {
                        if let Some(next) = state_with_text_selection(state, from, to) {
                            state = next;
                            target_range_applied = true;
                        } else if let Some(live_state) = state_provider(true) {
                            state = live_state;
                        } else {
                            log_input_perf(debug_perf, "input.beforeinput", || {
                                json!({
                                    "runtime": runtime_for_log.clone(),
                                    "input_type": input_type.clone(),
                                    "handled": false,
                                    "reason": "target_selection_error",
                                    "target_range": true,
                                    "range_ms": round_ms(target_range_ms),
                                    "state_ms": round_ms(perf_now_ms() - state_started_at),
                                    "total_ms": round_ms(perf_now_ms() - started_at),
                                })
                            });
                            return;
                        }
                    }
                    if !target_range_found && state.selection().is_cells() {
                        let mut tr = state.tr();
                        if tr.delete_selection().is_err() {
                            return;
                        }
                        dispatch(state, tr, false);
                        return;
                    }
                    let fallback_range_started_at = perf_now_ms();
                    let range = target_range.or_else(|| delete_fallback_range(&state, &input_type));
                    let range_ms = target_range_ms + (perf_now_ms() - fallback_range_started_at);
                    let Some((from, to)) = range else {
                        log_input_perf(debug_perf, "input.beforeinput", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type.clone(),
                                "handled": false,
                                "reason": "missing_range",
                                "target_range": target_range_found,
                                "range_ms": round_ms(range_ms),
                                "state_ms": round_ms(fallback_range_started_at - state_started_at),
                                "total_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        return;
                    };
                    if from == to {
                        log_input_perf(debug_perf, "input.beforeinput", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type.clone(),
                                "handled": false,
                                "reason": "empty_range",
                                "target_range": target_range_found,
                                "range_ms": round_ms(range_ms),
                                "state_ms": round_ms(fallback_range_started_at - state_started_at),
                                "total_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        return;
                    }
                    let transaction_started_at = perf_now_ms();
                    let mut tr = state.tr();
                    if tr.delete_range(from, to).is_err() {
                        log_input_perf(debug_perf, "input.beforeinput", || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "input_type": input_type.clone(),
                                "handled": false,
                                "reason": "transaction_error",
                                "from": from,
                                "to": to,
                                "target_range": target_range_found,
                                "state_ms": round_ms(fallback_range_started_at - state_started_at),
                                "range_ms": round_ms(range_ms),
                                "transaction_ms": round_ms(perf_now_ms() - transaction_started_at),
                                "total_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        return;
                    }
                    log_input_perf(debug_perf, "input.beforeinput", || {
                        json!({
                            "runtime": runtime_for_log.clone(),
                            "input_type": input_type.clone(),
                            "handled": true,
                            "from": from,
                            "to": to,
                            "deleted_units": to.saturating_sub(from),
                            "target_range": target_range_found,
                            "target_range_applied": target_range_applied,
                            "state_ms": round_ms(fallback_range_started_at - state_started_at),
                            "range_ms": round_ms(range_ms),
                            "transaction_ms": round_ms(perf_now_ms() - transaction_started_at),
                            "total_before_dispatch_ms": round_ms(perf_now_ms() - started_at),
                        })
                    });
                    let input_type_for_log = input_type.clone();
                    dispatch(state, tr, false);
                    log_input_perf(debug_perf, "input.beforeinput.complete", || {
                        json!({
                            "runtime": runtime_for_log,
                            "input_type": input_type_for_log.clone(),
                            "total_after_dispatch_ms": round_ms(perf_now_ms() - started_at),
                        })
                    });
                    schedule_next_frame_perf(
                        debug_perf,
                        "input.next_frame",
                        || {
                            json!({
                                "runtime": runtime_for_log.clone(),
                                "source": "beforeinput",
                                "input_type": input_type_for_log,
                            })
                        },
                        started_at,
                    );
                }
                // Composition is the one exception to the floor below, and it
                // has to be. An IME owns the DOM for the duration of a
                // composition — it needs to place, restyle and replace its own
                // preedit text — so refusing these would not "keep the model
                // authoritative", it would stop CJK and Android keyboards from
                // typing at all. We let the browser mutate, suppress the
                // reconciler so the composing text node survives (see
                // `composing` below), and reconcile the model at
                // `compositionend`.
                "insertCompositionText"
                | "insertFromComposition"
                | "deleteByComposition"
                | "deleteCompositionText" => {
                    log_input_perf(debug_perf, "input.beforeinput", || {
                        json!({
                            "runtime": runtime_for_log.clone(),
                            "input_type": input_type.clone(),
                            "handled": false,
                            "reason": "composition_native_until_compositionend",
                            "total_ms": round_ms(perf_now_ms() - started_at),
                        })
                    });
                }
                // Everything else: refuse it. This surface is controlled — the
                // model is authoritative and the DOM is a projection of it — so
                // an edit the model never sees is not a missing feature, it is
                // corruption. The browser can emit input types we don't
                // implement (`insertReplacementText` from autocorrect,
                // `historyUndo` from shake-to-undo, `formatBold` from the mobile
                // selection toolbar, `insertFromDrop`, `insertTranspose`,
                // `insertFromYank`, and whatever the next spec revision adds),
                // and letting one through desyncs the model silently: the DOM
                // gains content the model has no position for, so the next
                // `getTargetRanges` mapping resolves against a document that no
                // longer matches and the reconciler eventually repaints the
                // model over the user's text.
                //
                // Refusing turns an unimplemented input type into a visible
                // no-op instead. That is a *worse* editor and a *correct* one,
                // and it fails in the direction we can see. Implement the ones
                // that matter as explicit arms above; this is the floor.
                other => {
                    ev.prevent_default();
                    log_input_perf(debug_perf, "input.beforeinput", || {
                        json!({
                            "runtime": runtime_for_log.clone(),
                            "input_type": other,
                            "handled": false,
                            "reason": "unsupported_input_type_refused",
                            "total_ms": round_ms(perf_now_ms() - started_at),
                        })
                    });
                }
            }
        }) as Box<dyn FnMut(Event)>);
        let _ =
            surface.add_event_listener_with_callback("beforeinput", cb.as_ref().unchecked_ref());
        closures.push(cb);
    }

    // `drop` — the belt to the `deleteByDrag`/`insertFromDrop` braces above.
    // A native drop is not guaranteed to surface as a pair of `beforeinput`
    // events, so refusing those two is not on its own enough to keep dropped
    // content out of the controlled DOM. Refuse the drop itself as well.
    {
        let manager = node_view_manager.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            if !editor_owns_event(&event, &manager) {
                return;
            }
            event.prevent_default();
        }) as Box<dyn FnMut(Event)>);
        let _ = surface.add_event_listener_with_callback("drop", cb.as_ref().unchecked_ref());
        closures.push(cb);
    }

    // Always serialize clipboard output from the semantic model. Letting the
    // browser clone the live selection would leak typed component chrome,
    // resize handles, and other node-view DOM into `text/html`. Cell
    // selections use the same path; their `Selection::content` is rectangular.
    for (event_name, cut) in [("copy", false), ("cut", true)] {
        let state_provider = state_provider.clone();
        let dispatch = dispatch.clone();
        let runtime_for_clipboard = runtime.clone();
        let manager = node_view_manager.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            if !editor_owns_event(&event, &manager) {
                return;
            }
            let Ok(ev) = event.clone().dyn_into::<ClipboardEvent>() else {
                return;
            };
            let Some(state) = state_provider(false) else {
                return;
            };
            if state.selection().is_empty(state.doc()) {
                return;
            }
            let Some(clipboard) = ev.clipboard_data() else {
                return;
            };
            if !write_selection_clipboard(&state, &runtime_for_clipboard, &clipboard) {
                return;
            }
            ev.prevent_default();
            if cut {
                let mut tr = state.tr();
                if tr.delete_selection().is_ok() {
                    dispatch(state, tr, false);
                }
            }
        }) as Box<dyn FnMut(Event)>);
        let _ = surface.add_event_listener_with_callback(event_name, cb.as_ref().unchecked_ref());
        closures.push(cb);
    }

    // preventDefault is critical: the browser's default paste mutates
    // the contentEditable DOM directly, leaving the model stale and
    // silently losing pasted content on the next save.
    {
        let state_provider = state_provider.clone();
        let dispatch = dispatch.clone();
        let runtime_for_paste = runtime.clone();
        let runtime_for_log = runtime_name.clone();
        let manager = node_view_manager.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            if !editor_owns_event(&event, &manager) {
                return;
            }
            let started_at = perf_now_ms();
            let Ok(ev) = event.clone().dyn_into::<ClipboardEvent>() else {
                return;
            };
            let Some(clipboard) = ev.clipboard_data() else {
                return;
            };
            // Prevent FIRST, decide after. The editor owns this event either way:
            // a payload we can't read (an image, or `text/html` from an app that
            // omits a plain-text flavour) must become a no-op, never a native
            // paste into the controlled DOM. Returning before preventing let the
            // browser insert content the model never learned about — and it also
            // un-suppressed the follow-on `beforeinput: insertFromPaste`.
            ev.prevent_default();
            let encoded_slice = clipboard.get_data(PINE_SLICE_MIME).unwrap_or_default();
            let text = clipboard.get_data("text/plain").unwrap_or_default();
            if encoded_slice.is_empty() && text.is_empty() {
                log_input_perf(debug_perf, "input.paste", || {
                    json!({
                        "runtime": runtime_for_log.clone(),
                        "handled": false,
                        "reason": "no_readable_flavour",
                        "total_ms": round_ms(perf_now_ms() - started_at),
                    })
                });
                return;
            }
            let Some(state) = state_provider(true) else {
                log_input_perf(debug_perf, "input.paste", || {
                    json!({
                        "runtime": runtime_for_log.clone(),
                        "handled": false,
                        "reason": "missing_state",
                        "total_ms": round_ms(perf_now_ms() - started_at),
                    })
                });
                return;
            };
            let parse_started_at = perf_now_ms();
            let custom = (!encoded_slice.is_empty())
                .then(|| runtime_for_paste.import_clipboard_json(&encoded_slice).ok())
                .flatten()
                .and_then(|slice| {
                    let mut tr = state.tr();
                    tr.replace_selection(slice).ok()?;
                    Some(tr)
                });
            let (tr_opt, mode) = if let Some(transaction) = custom {
                (Some(transaction), "pine_slice")
            } else {
                let parser = runtime_for_paste.markdown_parser();
                let parsed = parser.parse(&text, state.schema());
                match parsed {
                    Ok(doc) => match paste_transaction_from_doc(&state, &doc) {
                        Some(tr) => (Some(tr), "markdown"),
                        None => (
                            insert_text_transaction(&state, text.clone()),
                            "plain_fallback",
                        ),
                    },
                    Err(_) => (
                        insert_text_transaction(&state, text.clone()),
                        "plain_fallback",
                    ),
                }
            };
            let parse_ms = perf_now_ms() - parse_started_at;
            let Some(tr) = tr_opt else {
                log_input_perf(debug_perf, "input.paste", || {
                    json!({
                        "runtime": runtime_for_log.clone(),
                        "handled": false,
                        "reason": "no_transaction",
                        "mode": mode,
                        "parse_ms": round_ms(parse_ms),
                        "total_ms": round_ms(perf_now_ms() - started_at),
                    })
                });
                return;
            };
            log_input_perf(debug_perf, "input.paste", || {
                json!({
                    "runtime": runtime_for_log.clone(),
                    "handled": true,
                    "mode": mode,
                    "chars": text.chars().count(),
                    "parse_ms": round_ms(parse_ms),
                    "total_before_dispatch_ms": round_ms(perf_now_ms() - started_at),
                })
            });
            dispatch(state, tr, false);
            schedule_next_frame_perf(
                debug_perf,
                "input.next_frame",
                || {
                    json!({
                        "runtime": runtime_for_log,
                        "source": "paste",
                    })
                },
                started_at,
            );
        }) as Box<dyn FnMut(Event)>);
        let _ = surface.add_event_listener_with_callback("paste", cb.as_ref().unchecked_ref());
        closures.push(cb);
    }

    // No selectionchange listener — see module docs. Drag-select would
    // be broken if we tried to round-trip every move through the model.

    let _ = surface; // keep `surface` named even after we drop the
    // selectionchange listener that used it.
    closures
}

fn editor_owns_event(event: &Event, manager: &Rc<RefCell<NodeViewManager>>) -> bool {
    manager
        .borrow()
        .event_boundary(event)
        .editor_handles(event.default_prevented())
}

fn write_selection_clipboard(
    state: &EditorState,
    runtime: &EditorRuntime,
    clipboard: &web_sys::DataTransfer,
) -> bool {
    let Ok(slice) = state.selection().content(state.doc(), state.schema()) else {
        return false;
    };
    let Ok(export) = runtime.export_clipboard(&slice) else {
        return false;
    };
    clipboard.set_data(PINE_SLICE_MIME, &export.json).is_ok()
        && clipboard.set_data("text/html", &export.html).is_ok()
        && clipboard.set_data("text/plain", &export.plain_text).is_ok()
}

/// Build a paste transaction from a markdown-parsed document.
///
/// Per-edge open-depth heuristic, mirroring ProseMirror's default
/// paste behaviour:
/// - If the leading block is a paragraph, open the slice at start
///   (depth 1) so its inline content merges into the destination
///   paragraph at the cursor.
/// - If the trailing block is a paragraph, open the slice at end
///   so its inline content joins the destination's trailing
///   content.
/// - Headings, lists, blockquotes and other structural blocks stay
///   closed at the boundary and insert as-is.
///
/// Pasting "**foo**" (single paragraph) thus merges inline; pasting
/// `## title\n\n- item` keeps the heading and bullet_list as blocks
/// (splitting the cursor's paragraph instead of dissolving them
/// into inline text).
fn paste_transaction_from_doc(state: &EditorState, doc: &Node) -> Option<Transaction> {
    let content = doc.content().clone();
    if content.is_empty() {
        return None;
    }
    let first_is_para = content
        .child(0)
        .map(|n| n.type_name() == "paragraph")
        .unwrap_or(false);
    let last_is_para = content
        .child(content.len().saturating_sub(1))
        .map(|n| n.type_name() == "paragraph")
        .unwrap_or(false);
    let open_start = usize::from(first_is_para);
    let open_end = usize::from(last_is_para);

    // Try the per-edge open heuristic first. Falls back to a closed
    // (0, 0) slice if the fitter can't accept asymmetric opens —
    // pine's `fit_block_slice_inside_textblock` only handles fully
    // closed slices today, so e.g. [heading, paragraph] with
    // open_end=1 needs the closed-slice retry to land at all.
    try_replace_selection(state, &content, open_start, open_end)
        .or_else(|| try_replace_selection(state, &content, 0, 0))
}

fn try_replace_selection(
    state: &EditorState,
    content: &crate::model::Fragment,
    open_start: usize,
    open_end: usize,
) -> Option<Transaction> {
    let mut tr = state.tr();
    tr.replace_selection(Slice::new(content.clone(), open_start, open_end))
        .ok()?;
    Some(tr)
}

fn insert_text_transaction(state: &EditorState, text: String) -> Option<Transaction> {
    let mut tr = state.tr();
    if tr.insert_text(text.clone()).is_ok() {
        return Some(tr);
    }

    let pos = state.selection().from(state.doc());
    let near = Selection::near(state.doc(), state.schema(), pos, 1).ok()?;
    let mut tr = state.tr();
    tr.set_selection(near).ok()?;
    tr.insert_text(text).ok()?;
    Some(tr)
}

fn state_with_text_selection(state: EditorState, from: usize, to: usize) -> Option<EditorState> {
    let selection = Selection::text_between_near(state.doc(), state.schema(), from, to, None)
        .unwrap_or(Selection::Text {
            anchor: from,
            head: to,
        });
    let mut tr = state.tr();
    tr.set_selection(selection).ok()?;
    state.apply(tr).ok()
}

/// Translate the first range in `InputEvent.getTargetRanges()` into a
/// `(from, to)` model position pair. Browsers populate that array for
/// modern `beforeinput` edits, which lets us avoid a separate
/// `window.getSelection()` scan on the hot typing/deletion path.
fn target_range_only_to_model(surface: &Element, event: &InputEvent) -> Option<(usize, usize)> {
    let ranges = event.get_target_ranges();
    if ranges.length() > 0
        && let Ok(range) = ranges.get(0).dyn_into::<StaticRange>()
    {
        let start = dom_pos_to_model(surface, &range.start_container(), range.start_offset());
        let end = dom_pos_to_model(surface, &range.end_container(), range.end_offset());
        if let (Some(a), Some(b)) = (start, end) {
            let (from, to) = if a <= b { (a, b) } else { (b, a) };
            return Some((from, to));
        }
    }
    None
}

/// Fallback for browsers or edit shapes that do not provide a usable
/// target range. This path relies on the model selection, so callers
/// should request live selection when they expect to use it.
fn delete_fallback_range(state: &EditorState, input_type: &str) -> Option<(usize, usize)> {
    // Fallback: derive a sensible single-step range from the model
    // selection. Used when the browser didn't supply target ranges
    // (older WebKit on `deleteWordBackward`, e.g.) or when the range
    // pointed outside the surface.
    let sel = state.selection();
    if sel.is_cells() {
        return None;
    }
    let from = sel.from(state.doc());
    let to = sel.to(state.doc());
    if from != to {
        return Some((from, to));
    }
    let backward = matches!(
        input_type,
        "deleteContentBackward"
            | "deleteWordBackward"
            | "deleteSoftLineBackward"
            | "deleteHardLineBackward"
    );
    if backward {
        if from == 0 {
            return None;
        }
        Some((from - 1, from))
    } else {
        let doc_size = state.doc().content_size();
        if to >= doc_size {
            return None;
        }
        Some((to, to + 1))
    }
}

/// Read the live `window.getSelection()` range and translate it into a
/// model [`crate::state::Selection`]. Returns `None` if there's no
/// selection or if it doesn't live inside `surface`. Used by
/// [`super::root::PineRichTextRoot`]'s state provider to inject the
/// up-to-date cursor into the state every time a command runs.
pub fn read_dom_selection(surface: &Element) -> Option<crate::state::Selection> {
    let sel = web_sys::window().and_then(|w| w.get_selection().ok().flatten())?;
    let anchor_node = sel.anchor_node()?;
    if !surface.contains(Some(&anchor_node)) {
        return None;
    }
    let focus_node = sel.focus_node()?;
    if !surface.contains(Some(&focus_node)) {
        return None;
    }
    let anchor = dom_pos_to_model(surface, &anchor_node, sel.anchor_offset())?;
    let head = dom_pos_to_model(surface, &focus_node, sel.focus_offset())?;
    Some(crate::state::Selection::text_between(anchor, head))
}

fn log_input_perf(enabled: bool, event: &str, payload: impl FnOnce() -> Value) {
    if !enabled {
        return;
    }
    let value = json!({
        "debug_version": INPUT_DEBUG_LOG_VERSION,
        "event": event,
        "payload": payload(),
    });
    let Ok(message) = serde_json::to_string(&value) else {
        return;
    };
    log_to_console_with_label("pine-richtext:perf", &message);
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static NEXT_FRAME_PERF_PENDING: Cell<bool> = const { Cell::new(false) };
}

#[cfg(target_arch = "wasm32")]
fn schedule_next_frame_perf(
    enabled: bool,
    event: &'static str,
    payload: impl FnOnce() -> Value,
    started_at: f64,
) {
    if !enabled {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let already_pending = NEXT_FRAME_PERF_PENDING.with(|pending| {
        if pending.get() {
            true
        } else {
            pending.set(true);
            false
        }
    });
    if already_pending {
        return;
    }
    // Enabled and no frame pending → build the payload now (once).
    let mut payload = payload();
    let callback = Closure::once_into_js(move || {
        NEXT_FRAME_PERF_PENDING.with(|pending| pending.set(false));
        if let Value::Object(ref mut map) = payload {
            map.insert(
                "next_frame_ms".to_string(),
                json!(round_ms(perf_now_ms() - started_at)),
            );
        }
        log_input_perf(true, event, || payload);
    });
    let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
}

#[cfg(not(target_arch = "wasm32"))]
fn schedule_next_frame_perf(
    enabled: bool,
    event: &'static str,
    payload: impl FnOnce() -> Value,
    started_at: f64,
) {
    // Currently a stub. `payload` is a closure so its `json!` is never built
    // unless this is wired up — no wasted per-keystroke allocation.
    let _ = enabled;
    let _ = event;
    let _ = payload;
    let _ = started_at;
}

fn round_ms(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(target_arch = "wasm32")]
fn perf_now_ms() -> f64 {
    // `performance.now()` gives sub-millisecond resolution (microseconds
    // in Chrome). `Date.now()` only resolves to whole milliseconds, which
    // hides sub-ms hot paths in the dispatch fan-out. Fallback to
    // `Date.now()` if `window.performance` is unavailable (no DOM —
    // happens in some test embeddings).
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or_else(js_sys::Date::now)
}

#[cfg(not(target_arch = "wasm32"))]
fn perf_now_ms() -> f64 {
    0.0
}

#[cfg(target_arch = "wasm32")]
fn log_to_console_with_label(label: &str, message: &str) {
    web_sys::console::log_2(&label.into(), &message.into());
}

#[cfg(not(target_arch = "wasm32"))]
fn log_to_console_with_label(label: &str, message: &str) {
    let _ = label;
    let _ = message;
}

#[cfg(test)]
mod tests {
    use super::{insert_text_transaction, paste_transaction_from_doc};
    use crate::markdown::MarkdownParser;
    use crate::schema_basic;
    use crate::state::{EditorState, EditorStateConfig, Selection};

    fn state_with_paragraph(text: &str, cursor: usize) -> EditorState {
        let schema = schema_basic::schema();
        let doc = schema_basic::doc(vec![
            schema_basic::paragraph(vec![schema_basic::text(text, Vec::new()).unwrap()]).unwrap(),
        ])
        .unwrap();
        EditorState::create(EditorStateConfig::new(schema, doc).selection(Selection::text(cursor)))
            .unwrap()
    }

    #[test]
    fn insert_text_recovers_from_doc_boundary_selection() {
        let state = state_with_paragraph("hello", 0);

        let tr = insert_text_transaction(&state, "G".to_string())
            .expect("typing at a doc boundary should move into the first textblock");
        let next = state.apply(tr).unwrap();

        assert_eq!(next.doc().text_content(), "Ghello");
        assert_eq!(next.selection(), &Selection::text(2));
    }

    #[test]
    fn paste_inline_markdown_merges_into_current_paragraph() {
        // Cursor between "hel|lo" — pasting "**bold**" should drop a
        // strong-marked "bold" run into the same paragraph.
        let state = state_with_paragraph("hello", 4);
        let parser = MarkdownParser::new();
        let parsed = parser
            .parse("**bold**", state.schema())
            .expect("markdown parses");
        let tr = paste_transaction_from_doc(&state, &parsed).expect("paste transaction builds");
        let next = state.apply(tr).unwrap();
        assert_eq!(
            next.doc().text_content(),
            "helboldlo",
            "pasted inline content merges into the current paragraph"
        );
        assert_eq!(next.doc().child_count(), 1, "still one block");
    }

    #[test]
    fn paste_multi_block_markdown_splits_and_inserts_blocks() {
        // Cursor between "hel|lo" — pasting two paragraphs should split
        // the current paragraph and drop the new block in between.
        let state = state_with_paragraph("hello", 4);
        let parser = MarkdownParser::new();
        let parsed = parser
            .parse("alpha\n\nbeta", state.schema())
            .expect("markdown parses");
        let tr = paste_transaction_from_doc(&state, &parsed).expect("paste transaction builds");
        let next = state.apply(tr).unwrap();
        assert_eq!(next.doc().child_count(), 2);
        assert_eq!(next.doc().child(0).unwrap().text_content(), "helalpha");
        assert_eq!(next.doc().child(1).unwrap().text_content(), "betalo");
    }

    #[test]
    fn paste_preserves_heading_when_first_block_is_heading() {
        // Multi-block markdown starting with a heading: heading stays
        // a heading block (per-edge open heuristic: open_start=0 when
        // the first block is structural), the cursor's paragraph
        // splits, and the trailing paragraph content of the pasted
        // doc merges into the cursor's tail.
        let state = state_with_paragraph("hello", 5);
        let parser = MarkdownParser::new();
        let parsed = parser
            .parse("# Title\n\nbody", state.schema())
            .expect("markdown parses");
        let tr = paste_transaction_from_doc(&state, &parsed).expect("paste transaction builds");
        let next = state.apply(tr).unwrap();
        let text = next.doc().text_content();
        assert!(text.contains("Title"), "heading text present: {text:?}");
        assert!(text.contains("body"), "body text present: {text:?}");
        let mut saw_heading = false;
        for i in 0..next.doc().child_count() {
            if next.doc().child(i).unwrap().type_name() == "heading" {
                saw_heading = true;
                break;
            }
        }
        assert!(
            saw_heading,
            "pasted heading should survive as a heading block"
        );
    }
}

/// Browser tests for the `beforeinput` dispatch itself.
///
/// The arms in `install_listeners` were previously unreachable from any test —
/// nothing in the repo constructed an `InputEvent` — so the whole match was
/// covered only by whatever a human happened to try by hand. These drive real
/// events at a real mounted surface and assert on both halves of the contract:
/// what the model ends up holding, and whether the browser was allowed to touch
/// the DOM (`default_prevented`).
#[cfg(all(test, target_arch = "wasm32"))]
mod input_event_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
    use web_sys::{Element, InputEvent, InputEventInit};

    use super::{KeyMap, base_keymap, install_listeners};
    use crate::render::render_doc_to_html;
    use crate::runtime::{EditorRuntime, RuntimeBuilder};
    use crate::schema_basic;
    use crate::state::{EditorState, EditorStateConfig, Selection};
    use crate::view::node_view_manager::NodeViewManager;

    wasm_bindgen_test_configure!(run_in_browser);

    fn runtime() -> Arc<EditorRuntime> {
        RuntimeBuilder::new().build()
    }

    fn state_with(text: &str, cursor: usize) -> EditorState {
        let schema = schema_basic::schema();
        let doc = schema_basic::doc(vec![
            schema_basic::paragraph(vec![schema_basic::text(text, Vec::new()).unwrap()]).unwrap(),
        ])
        .unwrap();
        EditorState::create(EditorStateConfig::new(schema, doc).selection(Selection::text(cursor)))
            .unwrap()
    }

    /// Mount a surface rendered from `state` and install the real listeners over
    /// it. Returns the surface, the cell the dispatch writes into, and the
    /// closures (which must stay alive or the listeners detach).
    #[allow(clippy::type_complexity)]
    fn mount(
        state: EditorState,
    ) -> (
        Element,
        Rc<RefCell<EditorState>>,
        Vec<wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>>,
    ) {
        let document = web_sys::window().unwrap().document().unwrap();
        let surface = document.create_element("div").unwrap();
        let rt = runtime();
        surface.set_inner_html(&render_doc_to_html(&rt, state.doc()));
        document.body().unwrap().append_child(&surface).unwrap();

        let live = Rc::new(RefCell::new(state));
        let for_provider = live.clone();
        let for_dispatch = live.clone();
        let closures = install_listeners(
            surface.clone(),
            rt,
            Rc::new(move |_live_selection: bool| Some(for_provider.borrow().clone())),
            Rc::new(RefCell::new(NodeViewManager::new(runtime()))),
            Rc::new(base_keymap()) as Rc<KeyMap>,
            false,
            move |state: EditorState, tr, _scroll| {
                let next = state.apply(tr).expect("transaction applies");
                *for_dispatch.borrow_mut() = next;
            },
        );
        (surface, live, closures)
    }

    fn fire_beforeinput(surface: &Element, input_type: &str, data: Option<&str>) -> InputEvent {
        let init = InputEventInit::new();
        init.set_bubbles(true);
        init.set_cancelable(true);
        init.set_input_type(input_type);
        if let Some(data) = data {
            init.set_data(Some(data));
        }
        let ev = InputEvent::new_with_event_init_dict("beforeinput", &init).unwrap();
        surface
            .dispatch_event(ev.unchecked_ref::<web_sys::Event>())
            .unwrap();
        ev
    }

    fn doc_text(state: &EditorState) -> String {
        state
            .doc()
            .text_between(0, state.doc().content_size(), "\n")
            .unwrap()
    }

    #[wasm_bindgen_test]
    fn an_unsupported_input_type_is_refused_rather_than_left_to_the_browser() {
        // The regression this whole change exists for: an inputType we do not
        // implement used to fall through to native DOM mutation, desyncing the
        // model silently. It must now be refused instead.
        let (surface, live, _closures) = mount(state_with("hello", 6));

        for input_type in [
            "historyUndo",
            "historyRedo",
            "formatBold",
            "insertFromDrop",
            "insertTranspose",
            "insertFromYank",
            "insertLink",
            // Not in any spec — proves the floor covers what we've never heard of.
            "insertSomethingInventedLater",
        ] {
            let ev = fire_beforeinput(&surface, input_type, None);
            assert!(
                ev.default_prevented(),
                "`{input_type}` reached the browser; it must be refused"
            );
            assert_eq!(
                doc_text(&live.borrow()),
                "hello",
                "`{input_type}` must not change the model either"
            );
        }
    }

    #[wasm_bindgen_test]
    fn composition_is_deliberately_left_to_the_browser() {
        // The one exception: an IME owns the DOM while composing. Refusing these
        // would stop CJK and Android keyboards typing at all, so they are
        // allowed through and reconciled at compositionend instead.
        let (surface, _live, _closures) = mount(state_with("hello", 6));

        for input_type in [
            "insertCompositionText",
            "insertFromComposition",
            "deleteByComposition",
            "deleteCompositionText",
        ] {
            let ev = fire_beforeinput(&surface, input_type, Some("ni"));
            assert!(
                !ev.default_prevented(),
                "`{input_type}` must stay native — refusing it breaks IME input"
            );
        }
    }

    #[wasm_bindgen_test]
    fn insert_text_still_commits_through_the_model() {
        // Guards the refactor: the arm that used to be `"insertText"` alone now
        // also serves insertReplacementText, so prove the plain path is intact.
        let (surface, live, _closures) = mount(state_with("hello", 6));
        let ev = fire_beforeinput(&surface, "insertText", Some("!"));
        assert!(ev.default_prevented());
        assert_eq!(doc_text(&live.borrow()), "hello!");
    }

    #[wasm_bindgen_test]
    fn insert_replacement_text_is_handled_not_refused() {
        // Autocorrect. Without a target range the browser is telling us to
        // replace at the caret, so this lands as an insert; the important part
        // is that it is handled by the model at all rather than falling through
        // to a native DOM rewrite the model never sees.
        let (surface, live, _closures) = mount(state_with("hello", 6));
        let ev = fire_beforeinput(&surface, "insertReplacementText", Some(" there"));
        assert!(
            ev.default_prevented(),
            "a replacement must never reach the browser"
        );
        assert_eq!(doc_text(&live.borrow()), "hello there");
    }

    #[wasm_bindgen_test]
    fn a_soft_keyboard_return_still_splits_the_block() {
        // insertParagraph is refused by the floor unless it is routed to the
        // keymap's Enter binding. A soft keyboard often sends no usable keydown,
        // so without that routing Enter would simply stop working on mobile.
        let (surface, live, _closures) = mount(state_with("hello", 6));
        let ev = fire_beforeinput(&surface, "insertParagraph", None);
        assert!(ev.default_prevented());
        assert_eq!(
            live.borrow().doc().child_count(),
            2,
            "Enter should have split the paragraph in two"
        );
    }
}
