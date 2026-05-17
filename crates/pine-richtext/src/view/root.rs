//! The `<pine-rich-text>` root component.
//!
//! Holds an [`EditorState`](crate::state::EditorState) as a JSON-shaped
//! `#[model] doc` field (pocopine derives `Serialize`/`Deserialize` on
//! all model props; `EditorState` itself isn't `Serialize` but its
//! `to_json` / `from_json` round-trip is). On `on_ready` the component
//! paints the surface, installs keystroke + beforeinput +
//! selectionchange listeners, and watches the `doc` JSON to re-paint
//! when commands or external mutations land.

use std::cell::RefCell;
use std::rc::Rc;

use pocopine::prelude::*;
use pocopine::{current_scope_id, refs, watch_scope_field_scoped};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{CustomEvent, Element, Event, Range};

use crate::commands::{self, BoxedCommand};
use crate::history::{self as history, history_plugin};
use crate::model::Attrs;
use crate::render::node_views;
use crate::schema_basic;
use crate::state::{EditorState, EditorStateConfig, Plugin, Transaction};
use crate::transform::{AttrStep, Step};

use super::input::{default_keymap, install_listeners, read_dom_selection};
use super::reconciler::reconcile_surface_with_outcome;
use super::selection::model_pos_to_dom;

const DEBUG_LOG_VERSION: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "@",
    env!("CARGO_PKG_VERSION"),
    ":debug-json-v1"
);

/// CustomEvent name the surface listens for so external toolbars
/// (anything outside the surface scope) can dispatch commands without
/// having to keep their own doc copy. Routing every command through
/// the surface's authoritative state avoids the `pp-model` round-trip
/// race where a parent's mirrored `doc` is one `tick::next` stale.
pub const COMMAND_EVENT: &str = "pine:richtext:command";

/// Payload of a [`COMMAND_EVENT`] CustomEvent. Variants intentionally
/// stay close to `pine_richtext::commands::*` so the wire shape stays
/// thin — a toolbar serializes one of these into the `detail`, the
/// surface deserializes it, builds the command, and runs it through
/// the same state pipeline keystrokes use.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandRequest {
    /// Toggle a named mark (`"strong"`, `"em"`, `"code"`, etc.) across
    /// the live selection.
    ToggleMark { mark: String },
    /// Change the type of every block in the selection.
    SetBlockType {
        node_type: String,
        #[serde(default)]
        attrs: Attrs,
    },
    /// Wrap the selection in a node of the given type. Generic
    /// wrapping preserves `wrap_in` semantics, so list toolbar buttons
    /// should use [`CommandRequest::WrapInList`] instead.
    WrapIn {
        node_type: String,
        #[serde(default)]
        attrs: Attrs,
    },
    /// Wrap the selected sibling blocks in a list, producing one item
    /// per selected block.
    WrapInList {
        list_type: String,
        item_type: String,
        #[serde(default)]
        attrs: Attrs,
    },
    /// Lift the selected blocks out of their wrapper.
    Lift,
    /// Undo the last edit.
    Undo,
    /// Redo the most recently undone edit.
    Redo,
    /// Set or remove an attribute on the node at `pos`. `value: null`
    /// removes the attribute; any other JSON sets it. Used by node-view
    /// components (the checklist checkbox) so attribute mutations land
    /// in the same transaction history as everything else.
    SetNodeAttr {
        pos: usize,
        attr: String,
        #[serde(default)]
        value: serde_json::Value,
    },
    /// Replace the entire editor state JSON. Used by the demo's
    /// "Reset" button so the surface can swap docs through the same
    /// event pipeline it uses for every other command.
    ReplaceState { doc: serde_json::Value },
}

impl CommandRequest {
    fn into_command(self) -> Option<BoxedCommand> {
        Some(match self {
            CommandRequest::ToggleMark { mark } => {
                let mark = schema_basic::schema().mark(&mark, Attrs::new()).ok()?;
                commands::toggle_mark(mark)
            }
            CommandRequest::SetBlockType { node_type, attrs } => {
                commands::set_block_type(node_type, attrs)
            }
            CommandRequest::WrapIn { node_type, attrs } => commands::wrap_in(node_type, attrs),
            CommandRequest::WrapInList {
                list_type,
                item_type,
                attrs,
            } => commands::wrap_in_list(list_type, item_type, attrs),
            CommandRequest::Lift => commands::lift(),
            CommandRequest::Undo => history::undo(),
            CommandRequest::Redo => history::redo(),
            CommandRequest::SetNodeAttr { pos, attr, value } => {
                let value = if value.is_null() { None } else { Some(value) };
                Box::new(move |state: &EditorState| {
                    let mut tr = state.tr();
                    tr.step(Step::Attr(AttrStep {
                        pos,
                        attr: attr.clone(),
                        value: value.clone(),
                    }))
                    .ok()?;
                    Some(tr)
                })
            }
            CommandRequest::ReplaceState { .. } => return None,
        })
    }
}

/// Plugin set every state-materialization path uses. Kept in one place
/// so the surface's `state_provider`, the initial seed, and the dispatch
/// path all install the same plugins — otherwise the history branches
/// in the JSON would silently drop on the next round-trip.
fn default_plugins() -> Vec<Plugin> {
    vec![history_plugin()]
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineRichTextRoot.poco", role = "scope", display = "block")]
pub struct PineRichTextRoot {
    /// Emit copyable JSON snapshots to the browser console while
    /// applying state transitions. Intended for debugging view/state
    /// selection issues.
    #[prop]
    pub debug_json: bool,
    /// Seed document the surface initializes with. Read once at
    /// `on_setup`; subsequent updates to `initial_doc` are ignored.
    /// Pass through `pp-bind:initial-doc="…"` to seed without making
    /// the parent the source of truth.
    #[prop]
    pub initial_doc: Value,
    /// Authoritative editor state JSON. Owned by this component.
    /// Toolbars / external dispatchers should NOT bind to this field
    /// directly — fire a `pine:richtext:command` CustomEvent instead.
    pub doc: Value,
}

#[handlers]
impl PineRichTextRoot {
    fn on_setup(&mut self) {
        // Seed `doc` from either a static `initial-doc` attribute or
        // fall back to a single empty paragraph. The dynamic
        // `pp-bind:initial-doc` path is picked up later by an effect
        // installed in `on_ready` — it can't land here because
        // pp-bind effects haven't fired yet at on_setup time.
        if self.doc.is_null() {
            if !self.initial_doc.is_null() {
                self.doc = self.initial_doc.clone();
            } else if let Some(seed) = Self::default_state_json() {
                self.doc = seed;
            }
        }
    }

    fn on_ready(&self, _refs: pocopine::Refs) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(surface_el) = refs::get_on(scope, "surface") else {
            return;
        };
        let handle = this::<Self>();

        // Pick up the dynamic `pp-bind:initial-doc` seed if it landed
        // after `on_setup` ran (the common case — pp-bind effects fire
        // on a microtask following the synchronous mount). If the
        // attribute was static, `on_setup` already absorbed it and
        // this is a no-op. The watcher is one-shot: it copies the
        // bound value into `doc` only while `doc` is still null /
        // empty, so subsequent parent re-binds don't clobber edits.
        {
            let handle_for_seed = handle.clone();
            watch_scope_field_scoped::<Value, _>(scope, "initial_doc", move |seed, _| {
                if seed.is_null() {
                    return;
                }
                let seed = seed.clone();
                handle_for_seed.update(|root: &mut PineRichTextRoot| {
                    if root.doc.is_null()
                        || root.doc == empty_doc_seed_state_json().unwrap_or_default()
                    {
                        root.doc = seed;
                    }
                });
            });
        }

        // Initial paint + cursor sync. Use a sentinel empty doc as
        // `old_doc` so the reconciler's full-render branch
        // fires once at mount time.
        let empty_doc = empty_doc_node();
        paint_initial(&surface_el, &empty_doc, &self.doc);
        mount_registered_node_views(&surface_el);
        sync_cursor_from_doc(&surface_el, &self.doc);
        log_debug_json(self.debug_json, "mount", json!({ "state": self.doc }));

        // Track the last-rendered doc so the diff path knows what to
        // compare against on each change.
        let last_doc = Rc::new(RefCell::new(
            materialize_doc(&self.doc).unwrap_or(empty_doc),
        ));

        // Install keymap + beforeinput + selectionchange listeners.
        let keymap = Rc::new(default_keymap());
        let debug_json = self.debug_json;

        // `state_provider` resolves the current EditorState whenever an
        // event listener needs one. The model state is reconstructed
        // from the latest JSON; the model's selection is then OVERWRITTEN
        // with whatever `window.getSelection()` currently has so commands
        // operate on the live cursor — no separate selectionchange
        // listener required (which would break drag-select).
        let handle_for_provider = handle.clone();
        let surface_for_provider = surface_el.clone();
        let state_provider: Rc<dyn Fn() -> Option<EditorState>> = Rc::new(move || {
            let state = handle_for_provider.with(|root: &PineRichTextRoot| {
                if root.doc.is_null() {
                    return None;
                }
                EditorState::from_json(schema_basic::schema(), default_plugins(), root.doc.clone())
                    .ok()
            })?;
            // Try to inject the live DOM selection. If the cursor isn't
            // inside the surface (e.g., the user clicked outside), keep
            // the model's stored selection.
            if let Some(live) = read_dom_selection(&surface_for_provider) {
                let mut tr = state.tr();
                if tr.set_selection(live).is_ok() {
                    let next = state.apply(tr).ok()?;
                    log_debug_json(
                        debug_json,
                        "state_provider.live_selection",
                        json!({ "state": state_debug_json(&next) }),
                    );
                    return Some(next);
                }
            }
            Some(state)
        });

        // dispatch closure: apply the transaction against the same
        // state snapshot that produced it. That snapshot has the live
        // DOM selection injected; rebuilding from JSON here would
        // apply the edit with a stale model selection and pollute
        // history bookmarks.
        let handle_for_dispatch = handle.clone();
        let dispatch = move |state: EditorState, tr: Transaction| {
            handle_for_dispatch.update(move |root: &mut PineRichTextRoot| {
                let Ok(current) = EditorState::from_json(
                    schema_basic::schema(),
                    default_plugins(),
                    root.doc.clone(),
                ) else {
                    return;
                };
                if current.doc() != state.doc() {
                    log_debug_json(
                        debug_json,
                        "dispatch.stale_doc",
                        json!({
                            "current": state_debug_json(&current),
                            "transaction": transaction_debug_json(&tr),
                            "transaction_state": state_debug_json(&state),
                        }),
                    );
                    return;
                }
                let transaction = if debug_json {
                    Some(transaction_debug_json(&tr))
                } else {
                    None
                };
                let before = if debug_json {
                    Some(state_debug_json(&state))
                } else {
                    None
                };
                let Ok(next) = state.apply(tr) else { return };
                log_debug_json(
                    debug_json,
                    "dispatch.apply",
                    json!({
                        "before": before,
                        "transaction": transaction,
                        "after": state_debug_json(&next),
                    }),
                );
                if let Ok(json) = next.to_json() {
                    root.doc = json;
                }
            });
            // Force the reactive queue to drain *before* dispatch returns.
            // The auto-flush microtask `Handle::update` schedules is
            // delivered after the current handler frame unwinds, which is
            // fine for keystrokes (the listener returns to the JS event
            // loop immediately and the microtask runs) but not for
            // commands triggered from inside a parent `pp-on:click`
            // handler: the inner `dispatch_event` call returns synchronously
            // into a still-active outer `scope.invoke`, and in that
            // re-entrant frame the queued effects never ran in practice.
            // `flush_sync` makes reconciliation + cursor sync land deterministically.
            pocopine_core::flush_sync();
        };

        let closures = install_listeners(
            surface_el.clone(),
            state_provider.clone(),
            keymap,
            dispatch.clone(),
        );
        // Keep the listener Closures alive for the component's lifetime;
        // drop them on unmount.
        type ClosureSlot = Rc<RefCell<Vec<Closure<dyn FnMut(Event)>>>>;
        let slot: ClosureSlot = Rc::new(RefCell::new(closures));

        // External-toolbar bridge: anything outside the surface scope
        // dispatches a `pine:richtext:command` CustomEvent onto the
        // surface; we run it through the same state_provider + dispatch
        // the keymap uses. This is what keeps a parent toolbar from
        // racing with `pp-model`'s `tick::next` propagation and
        // overwriting typed-but-unflushed edits.
        {
            let state_provider = state_provider.clone();
            let dispatch = dispatch.clone();
            let handle_for_replace = handle.clone();
            let cb = Closure::wrap(Box::new(move |event: Event| {
                let Ok(custom) = event.dyn_into::<CustomEvent>() else {
                    return;
                };
                let Ok(request) = serde_wasm_bindgen::from_value::<CommandRequest>(custom.detail())
                else {
                    return;
                };
                if let CommandRequest::ReplaceState { doc } = request {
                    handle_for_replace.update(|root: &mut PineRichTextRoot| {
                        root.doc = doc;
                    });
                    pocopine_core::flush_sync();
                    return;
                }
                let cmd: BoxedCommand = match request.into_command() {
                    Some(cmd) => cmd,
                    None => return,
                };
                let Some(state) = state_provider() else {
                    return;
                };
                let Some(tr) = commands::Command::apply(cmd.as_ref(), &state) else {
                    return;
                };
                dispatch(state, tr);
            }) as Box<dyn FnMut(Event)>);
            let _ = surface_el
                .add_event_listener_with_callback(COMMAND_EVENT, cb.as_ref().unchecked_ref());
            slot.borrow_mut().push(cb);
        }

        let slot_for_drop = slot.clone();
        pocopine::on_scope_unmount(move || {
            slot_for_drop.borrow_mut().clear();
        });

        // Repaint when the doc changes. The reconciler walks the model
        // and DOM trees together so unchanged siblings, node-view
        // chrome, focus, IME state, scroll position, and unrelated
        // event listeners survive. The cursor is only re-synced when
        // reconciliation reports a structural DOM mutation — otherwise
        // selection-only updates would stomp over an in-progress drag.
        let surface_for_watch = surface_el;
        let last_doc_for_watch = last_doc;
        watch_scope_field_scoped::<Value, _>(scope, "doc", move |new_value, _| {
            let Some(new_doc) = materialize_doc(new_value) else {
                return;
            };
            let reconcile_outcome = {
                let old = last_doc_for_watch.borrow();
                reconcile_surface_with_outcome(&surface_for_watch, &old, &new_doc)
            };
            *last_doc_for_watch.borrow_mut() = new_doc;
            log_debug_json(
                debug_json,
                "watch.doc",
                json!({
                    "dom_changed": reconcile_outcome.dom_changed(),
                    "patch": reconcile_outcome.as_str(),
                    "state": new_value,
                }),
            );
            if reconcile_outcome.should_mount_node_views() {
                mount_registered_node_views(&surface_for_watch);
            }
            if reconcile_outcome.should_sync_cursor() {
                sync_cursor_from_doc(&surface_for_watch, new_value);
            }
        });
    }

    fn default_state_json() -> Option<Value> {
        empty_doc_seed_state_json()
    }
}

fn empty_doc_seed_state_json() -> Option<Value> {
    let schema = schema_basic::schema();
    let para = schema_basic::paragraph(Vec::new()).ok()?;
    let document = schema_basic::doc(vec![para]).ok()?;
    let state =
        EditorState::create(EditorStateConfig::new(schema, document).plugins(default_plugins()))
            .ok()?;
    state.to_json().ok()
}

/// Initial mount paint. Reconciles from an empty doc to the seed doc so
/// the full-render path lays down the surface markup once.
fn paint_initial(surface: &Element, old_doc: &crate::model::Node, doc_json: &Value) {
    let Some(new_doc) = materialize_doc(doc_json) else {
        return;
    };
    reconcile_surface_with_outcome(surface, old_doc, &new_doc);
}

fn materialize_doc(doc_json: &Value) -> Option<crate::model::Node> {
    EditorState::from_json(schema_basic::schema(), default_plugins(), doc_json.clone())
        .ok()
        .map(|s| s.doc().clone())
}

fn mount_registered_node_views(surface: &Element) {
    for tag in node_views::registered_tags() {
        let Ok(matches) = surface.query_selector_all(&tag) else {
            continue;
        };
        for i in 0..matches.length() {
            let Some(node) = matches.item(i) else {
                continue;
            };
            let Ok(host) = node.dyn_into::<Element>() else {
                continue;
            };
            pocopine::__private::mount_child_component(&host, &tag);
            pocopine::__private::finalize_compiled_subtree(&host);
        }
    }
}

fn empty_doc_node() -> crate::model::Node {
    schema_basic::doc(vec![schema_basic::paragraph(Vec::new()).unwrap()]).unwrap()
}

/// Push the model selection into `window.getSelection()` so the visible
/// caret matches what the model thinks the cursor is.
fn sync_cursor_from_doc(surface: &Element, doc_json: &Value) {
    let Some(state) =
        EditorState::from_json(schema_basic::schema(), default_plugins(), doc_json.clone()).ok()
    else {
        return;
    };
    let (anchor, head) = match state.selection() {
        crate::state::Selection::Text { anchor, head } => (*anchor, *head),
        crate::state::Selection::Node { anchor } => (*anchor, *anchor),
        crate::state::Selection::All => (0, state.doc().content_size()),
    };
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Ok(range) = document.create_range() else {
        return;
    };
    apply_position(&range, surface, state.doc(), anchor, true);
    apply_position(&range, surface, state.doc(), head, false);
    let Some(sel) = window.get_selection().ok().flatten() else {
        return;
    };
    let _ = sel.remove_all_ranges();
    let _ = sel.add_range(&range);
}

fn apply_position(
    range: &Range,
    surface: &Element,
    doc: &crate::model::Node,
    pos: usize,
    is_start: bool,
) {
    let Some((node, offset)) = model_pos_to_dom(surface, doc, pos) else {
        return;
    };
    if is_start {
        let _ = range.set_start(&node, offset);
    } else {
        let _ = range.set_end(&node, offset);
    }
}

fn state_debug_json(state: &EditorState) -> Value {
    state
        .to_json()
        .unwrap_or_else(|err| json!({ "error": err.to_string() }))
}

fn transaction_debug_json(transaction: &Transaction) -> Value {
    let steps: Vec<Value> = transaction
        .transform()
        .steps()
        .iter()
        .map(|step| step.to_json())
        .collect();
    json!({
        "selection": transaction.selection(),
        "stored_marks": transaction.stored_marks(),
        "steps": steps,
        "maps": transaction.transform().maps(),
        "meta": transaction.meta_map(),
    })
}

fn log_debug_json(enabled: bool, event: &str, payload: Value) {
    if !enabled {
        return;
    }
    let value = json!({
        "debug_version": DEBUG_LOG_VERSION,
        "event": event,
        "payload": payload,
    });
    let Ok(message) = serde_json::to_string_pretty(&value) else {
        return;
    };
    log_to_console(&message);
}

#[cfg(target_arch = "wasm32")]
fn log_to_console(message: &str) {
    web_sys::console::log_2(&"pine-richtext:json".into(), &message.into());
}

#[cfg(not(target_arch = "wasm32"))]
fn log_to_console(message: &str) {
    let _ = message;
}
