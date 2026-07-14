//! The `<pine-rich-text>` root component.
//!
//! Holds an [`EditorState`](crate::state::EditorState) as a JSON-shaped
//! `#[model] doc` field (pocopine derives `Serialize`/`Deserialize` on
//! all model props; `EditorState` itself isn't `Serialize` but its
//! `to_json` / `from_json` round-trip is). On `on_ready` the component
//! paints the surface, installs keystroke + beforeinput +
//! selectionchange listeners, and watches the `doc` JSON to re-paint
//! when commands or external mutations land.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use pocopine::prelude::*;
use pocopine::{current_scope_id, refs, watch_scope_field_scoped};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{CustomEvent, Element, Event, Range};

use crate::commands::{self, BoxedCommand};
use crate::history::{self as history};
use crate::model::{Attrs, Node};
use crate::runtime::{self, EditorRuntime};
use crate::state::{EditorState, EditorStateConfig, Plugin, Selection, Transaction};
use crate::transform::{AttrStep, Step};

use super::input::{default_keymap, install_listeners, read_dom_selection};
use super::node_view_handle::{NodeViewEditorBinding, TransactionDispatch};
use super::node_view_manager::NodeViewManager;
use super::reconciler::{ReconcileOutcome, reconcile_surface_with_manager};
use super::selection::model_pos_to_dom;
use super::selection_observer::SelectionChangeSubscription;

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

/// CustomEvent name external code dispatches at the surface to
/// request a JSON snapshot of the current editor state. The
/// surface responds by dispatching an
/// [`EXPORT_STATE_RESULT_EVENT`] (carrying the state JSON as
/// the event's `detail`) on the same surface element. Apps
/// typically consume this via the typed
/// [`view::Editor::get`](super::Editor::get) helper rather
/// than wiring the listener by hand — the helper additionally
/// runs the requested [`ContentFormat`](super::ContentFormat)
/// over the result.
pub const EXPORT_STATE_REQUEST_EVENT: &str = "pine:richtext:export-state";

/// CustomEvent the surface dispatches in response to
/// [`EXPORT_STATE_REQUEST_EVENT`]. The `detail` is a JS object
/// carrying the current
/// [`EditorState::to_json`](crate::state::EditorState::to_json)
/// payload, or `null` when the state is unavailable
/// (uninitialized surface, serializer failure).
pub const EXPORT_STATE_RESULT_EVENT: &str = "pine:richtext:export-state-result";

/// Fired when initial content, an external replacement, or a bound seed cannot
/// be migrated/materialized against the selected runtime. The event bubbles;
/// `detail` contains `runtime`, `error`, and the untouched `input` value.
pub const LOAD_ERROR_EVENT: &str = "pine:richtext:load-error";

/// Internal request event used by [`super::Editor::selection_snapshot`]. The
/// response carries only selection context, not serialized document content.
pub(crate) const SELECTION_SNAPSHOT_REQUEST_EVENT: &str = "pine:richtext:selection-snapshot";

/// Internal response event paired with [`SELECTION_SNAPSHOT_REQUEST_EVENT`].
pub(crate) const SELECTION_SNAPSHOT_RESULT_EVENT: &str = "pine:richtext:selection-snapshot-result";

/// CustomEvent the surface dispatches after every committed
/// transaction. The `detail` is a JS object carrying the
/// current [`EditorState::to_json`](crate::state::EditorState::to_json)
/// payload.
///
/// Useful for subscriptions that need to react to typing
/// (preview panes, autosave watchers, sync drivers). Apps
/// typically consume this via
/// [`view::Editor::on_update`](super::Editor::on_update),
/// which additionally runs the requested
/// [`ContentFormat`](super::ContentFormat) over the payload.
pub const DOC_CHANGED_EVENT: &str = "pine:richtext:doc-changed";

/// Lightweight per-commit signal, dispatched on **every** transaction —
/// independent of [`PineRichTextRoot::suppress_doc_changed`] (which only gates
/// the heavy [`DOC_CHANGED_EVENT`]). Its detail carries only cheap fields
/// ([`view::ChangeInfo`](super::ChangeInfo): a generation counter, whether the
/// doc is blank, and the caret's textblock-prefix), all O(1) or O(block) to
/// produce — so a consumer that must react to every keystroke (a placeholder,
/// an `@`-mention picker) pays no whole-document serialization. Subscribe via
/// [`view::Editor::on_change`](super::Editor::on_change); an editor that also
/// sets `suppress_doc_changed` then reads content on demand via
/// [`Editor::get`](super::Editor::get) types nothing per keystroke at all.
pub const CHANGE_EVENT: &str = "pine:richtext:change";

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
    /// Dispatch an extension-contributed command by registered name.
    /// `args` is forwarded to the [`crate::extension::NamedCommand`]
    /// factory; an unknown name or malformed args resolves to a silent
    /// no-op (same as a non-applicable command).
    Custom {
        name: String,
        #[serde(default)]
        args: serde_json::Value,
    },
}

impl CommandRequest {
    fn into_command(self, runtime: &EditorRuntime) -> Option<BoxedCommand> {
        Some(match self {
            CommandRequest::ToggleMark { mark } => {
                let mark = runtime.schema().mark(&mark, Attrs::new()).ok()?;
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
            CommandRequest::Custom { name, args } => {
                let factory = runtime.named_command(&name)?;
                factory(args)?
            }
        })
    }
}

/// Plugin set every state-materialization path uses, scoped to the
/// runtime this surface mounts against. Each surface gets its own
/// plugin set so two editors with different runtimes don't share
/// history (or any other) plugin state.
fn runtime_plugins(runtime: &EditorRuntime) -> Vec<Plugin> {
    runtime.plugins().to_vec()
}

/// Cache slot shared between `state_provider`, `dispatch`, and the
/// `root.doc` watcher so the typing fast path skips re-parsing the
/// full doc JSON on every keystroke.
///
/// Dispatch writes both the cache (cheap [`EditorState::clone`] — the
/// internal `Node`/`Fragment` uses Arc-shared children) and the
/// reactive `root.doc` JSON (still needed so external observers can
/// subscribe). The cache's `generation` is bumped on every commit;
/// the watcher compares its last-seen generation against the slot's
/// current one to decide between the fast path (use the cached
/// `state.doc()`) and the slow path (`materialize_doc` for an
/// external write like `ReplaceState` or `pp-bind:initial-doc`
/// rebind).
struct CommitSlot {
    state: Option<EditorState>,
    generation: u64,
}

/// Build an empty doc that's valid against this runtime's
/// schema. Used as the reconciler's sentinel `old_doc` at first
/// mount and as the default seed when no `initial_doc` is bound.
///
/// Delegates to [`Schema::default_node`], which walks the doc's
/// content match and fills required content automatically — so
/// schemas that require a leading node (e.g. a `title` before
/// `block*`) get the correct minimum-valid shape without this
/// helper knowing about specific node names.
fn empty_doc_for_runtime(runtime: &EditorRuntime) -> Option<Node> {
    runtime.schema().default_node("doc").ok()
}

/// JSON for the empty seed state of `runtime`, suitable for stashing
/// in `PineRichTextRoot::doc` when no `initial_doc` is bound.
fn empty_seed_state_json(runtime: &EditorRuntime) -> Option<Value> {
    let document = empty_doc_for_runtime(runtime)?;
    let state = EditorState::create(
        EditorStateConfig::new(runtime.schema().clone(), document)
            .plugins(runtime_plugins(runtime)),
    )
    .ok()?;
    state.to_json().ok()
}

/// Wrap a doc [`Node`] in a fresh [`EditorState`] valid against
/// `runtime`, then serialize to state JSON suitable for
/// stashing in `PineRichTextRoot::doc` or dispatching via
/// [`CommandRequest::ReplaceState`]. Used by the typed
/// [`super::Editor::set`] helper to land any
/// [`super::ContentFormat`] into the surface through the same
/// pipeline keystrokes use.
pub(crate) fn state_json_from_doc(doc: Node, runtime: &EditorRuntime) -> Option<Value> {
    let state = EditorState::create(
        EditorStateConfig::new(runtime.schema().clone(), doc).plugins(runtime_plugins(runtime)),
    )
    .ok()?;
    state.to_json().ok()
}

/// Expose the runtime's plugin set so the typed
/// [`super::Editor`] helpers can reconstruct an
/// [`EditorState`] from the same state JSON the surface
/// dispatches over [`DOC_CHANGED_EVENT`] /
/// [`EXPORT_STATE_RESULT_EVENT`].
pub(crate) fn runtime_plugins_view(runtime: &EditorRuntime) -> Vec<Plugin> {
    runtime_plugins(runtime)
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineRichTextRoot.poco", role = "scope", display = "block")]
pub struct PineRichTextRoot {
    /// Emit copyable JSON snapshots to the browser console while
    /// applying state transitions. Intended for debugging view/state
    /// selection issues.
    #[prop]
    pub debug_json: bool,
    /// Emit compact browser-console timing records for state materialization,
    /// transaction commit, DOM reconciliation, node-view mounting, and cursor
    /// sync. Intended for app-integration performance diagnosis.
    #[prop]
    pub debug_perf: bool,
    /// Suppress per-transaction `pine:richtext:doc-changed` events.
    /// Use this for surfaces that pull content explicitly via
    /// [`super::Editor::get`] instead of subscribing to live updates.
    #[prop]
    pub suppress_doc_changed: bool,
    /// Seed document the surface initializes with. Read once at
    /// `on_setup`; subsequent updates to `initial_doc` are ignored.
    /// Pass through `pp-bind:initial-doc="…"` to seed without making
    /// the parent the source of truth.
    #[prop]
    pub initial_doc: Value,
    /// Name of the [`crate::runtime::EditorRuntime`] this surface
    /// resolves against. `<pine-rich-text-root runtime="comment">`
    /// looks up the runtime registered with
    /// `pine_richtext::runtime::register("comment", …)`. Empty or
    /// absent falls back to [`crate::runtime::registry::default`] —
    /// the kitchen-sink default configuration.
    #[prop]
    pub runtime: String,
    /// External-seed editor state JSON. Owned by this component
    /// but written only on **state replacements** (initial mount,
    /// `ReplaceState`, `pp-bind:initial-doc` rebinds) — NOT on
    /// every keystroke commit. The keystroke fast path bumps
    /// [`PineRichTextRoot::doc_generation`] instead, so the
    /// reactive watcher fires off a tiny `u64` write rather than
    /// a tree-sized JSON write per commit. Reactive consumers
    /// that need the live state should subscribe to
    /// [`super::Editor::on_update`] or read it imperatively via
    /// [`super::Editor::get`]; treating `root.doc` as the
    /// canonical live value would see stale data between
    /// replacements.
    pub doc: Value,
    /// Monotonically-increasing commit counter the dispatch path
    /// bumps after every applied transaction. The `root.doc`
    /// watcher used to fire off the (potentially multi-MB) state
    /// JSON every keystroke; watching this `u64` instead means
    /// the reactive scheduler queues a single tiny diff. The
    /// reconciler reads the latest doc out of the in-memory
    /// state cache, so it's unaffected.
    pub doc_generation: u64,
}

impl PineRichTextRoot {
    /// Resolve the [`EditorRuntime`] this surface mounts against. The
    /// `runtime` prop names a registered runtime; an empty string
    /// (the `Default::default()` of the prop) falls back to the
    /// kitchen-sink default runtime — same configuration today's
    /// global `schema_basic::schema()` produces.
    fn resolve_runtime(&self) -> Arc<EditorRuntime> {
        let name = self.runtime.as_str();
        runtime::registry::resolve(if name.is_empty() { None } else { Some(name) })
    }
}

#[handlers]
impl PineRichTextRoot {
    fn on_setup(&mut self) {
        // Resolve the runtime first — every other seed/serialize path
        // below needs it to materialize an EditorState against the
        // correct schema.
        let runtime = self.resolve_runtime();

        // Seed `doc`. Order: static `initial-doc` attribute first
        // (the "I already have a state JSON" path), then the
        // empty seed. Dynamic `pp-bind:initial-doc` updates that
        // haven't landed yet at on_setup are picked up by the
        // watcher in `on_ready`. After mount, external content
        // changes go through the imperative
        // [`crate::view::Editor`] API.
        if self.doc.is_null() {
            if !self.initial_doc.is_null() {
                self.doc = self.initial_doc.clone();
            } else if let Some(seed) = empty_seed_state_json(&runtime) {
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
        let runtime = self.resolve_runtime();
        let node_view_manager = Rc::new(RefCell::new(NodeViewManager::new(runtime.clone())));

        // Materialize the seed state once up front. The cache is
        // the canonical source-of-truth for the live doc; the
        // watcher reads it on every `doc_generation` bump, the
        // initial-doc rebind watcher refreshes it, and the
        // ReplaceState handler rebuilds it. Built before the
        // initial paint and the initial-doc watcher because both
        // need it.
        let initial_input = self.doc.clone();
        let initial_state = EditorState::from_json(
            runtime.schema().clone(),
            runtime_plugins(&runtime),
            initial_input.clone(),
        );
        let initial_error = initial_state.as_ref().err().map(ToString::to_string);
        let commit_slot: Rc<RefCell<CommitSlot>> = Rc::new(RefCell::new(CommitSlot {
            state: initial_state.ok(),
            generation: 0,
        }));
        let last_watch_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));

        // Pick up the dynamic `pp-bind:initial-doc` seed if it landed
        // after `on_setup` ran (the common case — pp-bind effects fire
        // on a microtask following the synchronous mount). If the
        // attribute was static, `on_setup` already absorbed it and
        // this is a no-op. The watcher is one-shot: it copies the
        // bound value into `doc` only while `doc` is still null /
        // empty, so subsequent parent re-binds don't clobber edits.
        {
            let handle_for_seed = handle.clone();
            let runtime_for_seed = runtime.clone();
            let commit_slot_for_seed = commit_slot.clone();
            let surface_for_seed = surface_el.clone();
            watch_scope_field_scoped::<Value, _>(scope, "initial_doc", move |seed, _| {
                if seed.is_null() {
                    return;
                }
                let seed = seed.clone();
                let eligible = handle_for_seed.with(|root| {
                    root.doc.is_null()
                        || root.doc == empty_seed_state_json(&runtime_for_seed).unwrap_or_default()
                });
                if !eligible {
                    return;
                }
                let rebuilt = match EditorState::from_json(
                    runtime_for_seed.schema().clone(),
                    runtime_plugins(&runtime_for_seed),
                    seed.clone(),
                ) {
                    Ok(state) => state,
                    Err(error) => {
                        report_load_error(&surface_for_seed, &runtime_for_seed, &seed, &error);
                        return;
                    }
                };
                clear_load_error(&surface_for_seed);
                let commit_slot_for_seed = commit_slot_for_seed.clone();
                handle_for_seed.update(move |root: &mut PineRichTextRoot| {
                    root.doc = seed.clone();
                    // The reactive doc watcher fires on
                    // `doc_generation` now, so the bump must
                    // happen here for the surface to repaint.
                    // Refresh the cache too so the watcher's
                    // generation-gated read finds the new
                    // state. Initial-doc rebinds are rare
                    // (parent-side `pp-bind` updates) so the
                    // `from_json` cost is fine.
                    let mut slot = commit_slot_for_seed.borrow_mut();
                    slot.state = Some(rebuilt);
                    slot.generation = slot.generation.wrapping_add(1);
                    root.doc_generation = root.doc_generation.wrapping_add(1);
                });
            });
        }

        // Initial paint + cursor sync. Use a sentinel empty doc as
        // `old_doc` so the reconciler's full-render branch
        // fires once at mount time. Cursor sync reads the cached
        // state so we don't re-parse `self.doc` here.
        let empty_doc = empty_doc_for_runtime(&runtime).expect("runtime schema supports empty doc");
        paint_initial(&surface_el, &empty_doc, &self.doc, &runtime);
        if let Some(error) = initial_error {
            report_load_error_message(&surface_el, &runtime, &initial_input, &error);
        } else {
            clear_load_error(&surface_el);
        }
        if let Some(state) = commit_slot.borrow().state.as_ref() {
            sync_cursor_from_state(&surface_el, state);
        }
        log_debug_json(self.debug_json, "mount", || json!({ "state": self.doc }));

        // Track the last-rendered doc so the diff path knows what to
        // compare against on each change.
        let last_doc = Rc::new(RefCell::new(
            materialize_doc(&self.doc, &runtime).unwrap_or(empty_doc),
        ));

        // Install keymap + beforeinput + selectionchange listeners.
        let keymap = Rc::new(default_keymap(&runtime));
        let debug_json = self.debug_json;
        let debug_perf = self.debug_perf;

        // `state_provider` resolves the current EditorState whenever an
        // event listener needs one. The model state is reconstructed
        // from the latest JSON; the model's selection is then OVERWRITTEN
        // with whatever `window.getSelection()` currently has so commands
        // operate on the live cursor — no separate selectionchange
        // listener required (which would break drag-select).
        let handle_for_provider = handle.clone();
        let surface_for_provider = surface_el.clone();
        let runtime_for_provider = runtime.clone();
        let commit_slot_for_provider = commit_slot.clone();
        let state_provider: Rc<dyn Fn(bool) -> Option<EditorState>> =
            Rc::new(move |live_selection| {
                let started_at = perf_now_ms();
                // Fast path: typing keeps the cache hot. Avoid the
                // O(doc-size) `from_json` parse entirely.
                let mut cache_hit = true;
                let state = match commit_slot_for_provider.borrow().state.clone() {
                    Some(state) => state,
                    None => {
                        cache_hit = false;
                        let materialized =
                            handle_for_provider.with(|root: &PineRichTextRoot| {
                                if root.doc.is_null() {
                                    return None;
                                }
                                EditorState::from_json(
                                    runtime_for_provider.schema().clone(),
                                    runtime_plugins(&runtime_for_provider),
                                    root.doc.clone(),
                                )
                                .ok()
                            })?;
                        commit_slot_for_provider.borrow_mut().state = Some(materialized.clone());
                        materialized
                    }
                };
                let from_json_ms = perf_now_ms() - started_at;
                // Try to inject the live DOM selection. If the cursor isn't
                // inside the surface (e.g., the user clicked outside), keep
                // the model's stored selection.
                let selection_started_at = perf_now_ms();
                // Cell selections deliberately clear the browser range because
                // one DOM Range cannot represent a rectangle. Once the user
                // creates a real DOM caret/range again (for example by clicking
                // a paragraph outside the table), that live selection must win
                // just like it does for every other cached selection kind.
                if live_selection && let Some(live) = read_dom_selection(&surface_for_provider) {
                    let live = normalize_live_selection(&state, live);
                    // `with_selection` substitutes the selection without
                    // running the plugin state-field loop or the
                    // transaction filter/append hooks. For a no-op
                    // selection-only update that's what we want — the
                    // bigger cost in `state.apply` was the
                    // `BTreeMap::clone` of plugin_state + the per-plugin
                    // remove/reinsert dance even when no plugin actually
                    // mutates. Arc-wrapped values mean the clone is now
                    // O(plugins); skipping the loop saves the rest.
                    if let Ok(next) = state.with_selection(live) {
                        // Keep the hot in-memory state aligned with a genuine
                        // browser caret/range. This is not a document commit:
                        // it adds no history entry, emits no change event, and
                        // does not bump the document generation. It does ensure
                        // typed chrome that intentionally reads cached state
                        // cannot mistake the previous Cells selection for the
                        // current selection after a click-out.
                        if next.selection() != state.selection() {
                            commit_slot_for_provider.borrow_mut().state = Some(next.clone());
                        }
                        log_debug_json(
                            debug_json,
                            "state_provider.live_selection",
                            || json!({ "state": state_debug_json(&next) }),
                        );
                        log_perf(debug_perf, "state_provider", || {
                            json!({
                                "runtime": runtime_for_provider.name(),
                                "live_selection": true,
                                "live_selection_requested": live_selection,
                                "cache_hit": cache_hit,
                                "doc_size": next.doc().content_size(),
                                "top_level_children": next.doc().child_count(),
                                "selection_from": next.selection().from(next.doc()),
                                "selection_to": next.selection().to(next.doc()),
                                "from_json_ms": round_ms(from_json_ms),
                                "selection_ms": round_ms(perf_now_ms() - selection_started_at),
                                "total_ms": round_ms(perf_now_ms() - started_at),
                            })
                        });
                        return Some(next);
                    }
                }
                log_perf(debug_perf, "state_provider", || {
                    json!({
                        "runtime": runtime_for_provider.name(),
                        "live_selection": false,
                        "live_selection_requested": live_selection,
                        "cache_hit": cache_hit,
                        "doc_size": state.doc().content_size(),
                        "top_level_children": state.doc().child_count(),
                        "selection_from": state.selection().from(state.doc()),
                        "selection_to": state.selection().to(state.doc()),
                        "from_json_ms": round_ms(from_json_ms),
                        "selection_ms": round_ms(perf_now_ms() - selection_started_at),
                        "total_ms": round_ms(perf_now_ms() - started_at),
                    })
                });
                Some(state)
            });

        // dispatch closure: apply the transaction against the same
        // state snapshot that produced it. That snapshot has the live
        // DOM selection injected; rebuilding from JSON here would
        // apply the edit with a stale model selection and pollute
        // history bookmarks.
        let handle_for_dispatch = handle.clone();
        let runtime_for_dispatch = runtime.clone();
        let surface_for_dispatch = surface_el.clone();
        let commit_slot_for_dispatch = commit_slot.clone();
        let dispatch: Rc<TransactionDispatch> = Rc::new(
            move |state: EditorState, tr: Transaction, sync_flush: bool| {
                let dispatch_started_at = perf_now_ms();
                let runtime_for_inner = runtime_for_dispatch.clone();
                let surface_for_inner = surface_for_dispatch.clone();
                let runtime_name = runtime_for_dispatch.name().map(str::to_string);
                let commit_slot_inner = commit_slot_for_dispatch.clone();
                // Tag the transaction with a wall-clock timestamp
                // so the history plugin can merge close-in-time
                // typing events into the same undo step. Callers
                // that set their own `commit_ms` (e.g. tests) keep
                // theirs.
                let mut tr = tr;
                if tr.meta(crate::history::HISTORY_COMMIT_MS_META).is_none() {
                    tr.set_meta(
                        crate::history::HISTORY_COMMIT_MS_META,
                        json!(perf_now_ms() as u64),
                    );
                }
                handle_for_dispatch.update(move |root: &mut PineRichTextRoot| {
                    let update_started_at = perf_now_ms();
                    let from_json_started_at = perf_now_ms();
                    // Fast path: read the current state out of the cache
                    // instead of re-parsing root.doc every keystroke. The
                    // cache was populated by the previous commit (or by
                    // `on_ready`'s seed step); it can only miss if some
                    // external writer cleared it. Fall back to from_json
                    // in that case.
                    let mut current_cache_hit = true;
                    let current = match commit_slot_inner.borrow().state.clone() {
                        Some(state) => state,
                        None => {
                            current_cache_hit = false;
                            let Ok(state) = EditorState::from_json(
                                runtime_for_inner.schema().clone(),
                                runtime_plugins(&runtime_for_inner),
                                root.doc.clone(),
                            ) else {
                                return;
                            };
                            state
                        }
                    };
                    let from_json_ms = perf_now_ms() - from_json_started_at;
                    // Capture the transaction's steps (serialized) BEFORE `apply` moves
                    // `tr`, so the doc-changed event can carry them — a collab binding
                    // turns them into a fine-grained CRDT write (see
                    // `Editor::on_update_steps`).
                    let steps_json: Vec<Value> =
                        tr.transform().steps().iter().map(|s| s.to_json()).collect();
                    let step_count = steps_json.len();
                    if current.doc() != state.doc() {
                        log_debug_json(debug_json, "dispatch.stale_doc", || {
                            json!({
                                "current": state_debug_json(&current),
                                "transaction": transaction_debug_json(&tr),
                                "transaction_state": state_debug_json(&state),
                            })
                        });
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
                    let apply_started_at = perf_now_ms();
                    let Ok(next) = state.apply(tr) else { return };
                    let apply_ms = perf_now_ms() - apply_started_at;
                    // Populate the cache BEFORE writing root.doc so the
                    // reactive watcher (which fires after the JSON write)
                    // sees a fresh cache and can skip materialize_doc.
                    {
                        let mut slot = commit_slot_inner.borrow_mut();
                        slot.state = Some(next.clone());
                        slot.generation = slot.generation.wrapping_add(1);
                    }
                    log_debug_json(debug_json, "dispatch.apply", || {
                        json!({
                            "before": before,
                            "transaction": transaction,
                            "after": state_debug_json(&next),
                        })
                    });
                    // Hot-path generation bump. The reactive watcher
                    // used to fire on `root.doc` — a full state JSON
                    // value — so every keystroke queued a multi-KB
                    // value mutation and the reactive scheduler paid
                    // the propagation cost for the whole subtree.
                    // Now we bump a `u64` that everyone subscribes
                    // to instead, and the reconciler pulls the doc
                    // out of the in-memory cache via the same
                    // `commit_slot.generation` it already tracks.
                    let assign_started_at = perf_now_ms();
                    root.doc_generation = root.doc_generation.wrapping_add(1);
                    let assign_ms = perf_now_ms() - assign_started_at;
                    // Notify external subscribers that the doc
                    // changed. The slim payload carries `doc +
                    // selection + stored_marks` — no
                    // `plugin_state` so per-keystroke
                    // serde_wasm_bindgen stays bounded by doc size,
                    // not history length. Subscribers reconstruct
                    // an EditorState via
                    // [`view::Editor::on_update`].
                    let event_started_at = perf_now_ms();
                    let doc_changed_event = !root.suppress_doc_changed;
                    if doc_changed_event {
                        let slim = doc_changed_event_payload(&next, steps_json);
                        dispatch_doc_changed_event(&surface_for_inner, &slim);
                    }
                    // Lightweight change signal — dispatched on EVERY commit, even
                    // when the heavy doc-changed event above is suppressed. Cheap
                    // fields only (generation / empty / caret prefix), so a
                    // placeholder or `@`-mention picker can react per keystroke at
                    // O(block) cost instead of re-serializing the whole document.
                    let change = change_event_payload(&next, root.doc_generation);
                    dispatch_change_event(&surface_for_inner, &change);
                    let event_ms = perf_now_ms() - event_started_at;
                    log_perf(debug_perf, "dispatch.commit", || {
                        json!({
                            "runtime": runtime_for_inner.name(),
                            "steps": step_count,
                            "cache_hit": current_cache_hit,
                            "doc_size_before": state.doc().content_size(),
                            "doc_size_after": next.doc().content_size(),
                            "top_level_children": next.doc().child_count(),
                            "from_json_ms": round_ms(from_json_ms),
                            "apply_ms": round_ms(apply_ms),
                            // `to_json_ms` is now structurally zero —
                            // the hot path no longer serializes the
                            // full state JSON every commit. Kept in
                            // the schema so old perf dashboards
                            // don't break.
                            "to_json_ms": 0.0_f64,
                            "assign_ms": round_ms(assign_ms),
                            "doc_changed_event": doc_changed_event,
                            "event_ms": round_ms(event_ms),
                            "total_ms": round_ms(perf_now_ms() - update_started_at),
                            "update_ms": round_ms(perf_now_ms() - update_started_at),
                        })
                    });
                });
                if sync_flush {
                    // Force the reactive queue to drain before command dispatch returns.
                    // This is required for external commands triggered from inside a
                    // parent `pp-on:click` handler: the inner `dispatch_event` call
                    // returns synchronously into a still-active outer `scope.invoke`.
                    let flush_started_at = perf_now_ms();
                    pocopine_core::flush_sync();
                    log_perf(debug_perf, "dispatch.flush_sync", || {
                        json!({
                            "runtime": runtime_name,
                            "flush_ms": round_ms(perf_now_ms() - flush_started_at),
                            "total_ms": round_ms(perf_now_ms() - dispatch_started_at),
                        })
                    });
                } else {
                    log_perf(debug_perf, "dispatch.flush_deferred", || {
                        json!({
                            "runtime": runtime_name,
                            "total_ms": round_ms(perf_now_ms() - dispatch_started_at),
                        })
                    });
                }
            },
        );

        let node_view_binding = NodeViewEditorBinding::new(
            runtime.clone(),
            surface_el.clone(),
            state_provider.clone(),
            dispatch.clone(),
        );
        node_view_manager
            .borrow_mut()
            .set_binding(node_view_binding.clone());
        if let Some(state) = commit_slot.borrow().state.as_ref() {
            node_view_manager.borrow_mut().sync_tree(
                &surface_el,
                state.doc(),
                state.selection(),
                surface_is_editable(&surface_el),
                surface_is_focused(&surface_el),
            );
        }

        let dispatch_for_listeners = dispatch.clone();
        let closures = install_listeners(
            surface_el.clone(),
            runtime.clone(),
            state_provider.clone(),
            node_view_manager.clone(),
            keymap,
            debug_perf,
            move |state, transaction, sync_flush| {
                dispatch_for_listeners(state, transaction, sync_flush);
            },
        );
        // Keep the listener Closures alive for the component's lifetime;
        // drop them on unmount.
        type ClosureSlot = Rc<RefCell<Vec<Closure<dyn FnMut(Event)>>>>;
        let slot: ClosureSlot = Rc::new(RefCell::new(closures));

        // External-bridge listeners (toolbar commands, state
        // export, doc-changed broadcast). Install on the
        // custom-element host instead of the inner
        // `.pine-rich-text` so the typed [`super::Editor::find`]
        // helper can target a single, well-known element by
        // tag selector. Dispatches at the inner
        // `.pine-rich-text` still reach the listener via
        // bubbling (existing playwright tests dispatch at the
        // inner — that's preserved).
        let host_el = surface_el
            .parent_element()
            .unwrap_or_else(|| surface_el.clone());

        // External-toolbar bridge: anything outside the surface scope
        // dispatches a `pine:richtext:command` CustomEvent onto the
        // surface; we run it through the same state_provider + dispatch
        // the keymap uses.
        {
            let state_provider = state_provider.clone();
            let dispatch = dispatch.clone();
            let handle_for_replace = handle.clone();
            let runtime_for_listener = runtime.clone();
            let commit_slot_for_replace = commit_slot.clone();
            let surface_for_replace = surface_el.clone();
            let cb = Closure::wrap(Box::new(move |event: Event| {
                let Ok(custom) = event.dyn_into::<CustomEvent>() else {
                    return;
                };
                let Ok(request) = serde_wasm_bindgen::from_value::<CommandRequest>(custom.detail())
                else {
                    return;
                };
                if let CommandRequest::ReplaceState { doc } = request {
                    // Rebuild the cache eagerly so the watcher's
                    // generation-gated read finds the new state,
                    // and bump root.doc_generation so the
                    // watcher actually fires.
                    let rebuilt = match EditorState::from_json(
                        runtime_for_listener.schema().clone(),
                        runtime_plugins(&runtime_for_listener),
                        doc.clone(),
                    ) {
                        Ok(state) => state,
                        Err(error) => {
                            report_load_error(
                                &surface_for_replace,
                                &runtime_for_listener,
                                &doc,
                                &error,
                            );
                            return;
                        }
                    };
                    clear_load_error(&surface_for_replace);
                    let mut slot = commit_slot_for_replace.borrow_mut();
                    slot.state = Some(rebuilt);
                    slot.generation = slot.generation.wrapping_add(1);
                    drop(slot);
                    handle_for_replace.update(|root: &mut PineRichTextRoot| {
                        root.doc = doc;
                        root.doc_generation = root.doc_generation.wrapping_add(1);
                    });
                    pocopine_core::flush_sync();
                    return;
                }
                let cmd: BoxedCommand = match request.into_command(&runtime_for_listener) {
                    Some(cmd) => cmd,
                    None => return,
                };
                let Some(state) = state_provider(true) else {
                    return;
                };
                let Some(tr) = commands::Command::apply(cmd.as_ref(), &state) else {
                    return;
                };
                dispatch(state, tr, true);
            }) as Box<dyn FnMut(Event)>);
            let _ = host_el
                .add_event_listener_with_callback(COMMAND_EVENT, cb.as_ref().unchecked_ref());
            slot.borrow_mut().push(cb);
        }

        // Export-state bridge: external code dispatches
        // `pine:richtext:export-state` at the surface; the
        // surface resolves the current state via state_provider,
        // serializes it to JSON, and dispatches a
        // `pine:richtext:export-state-result` CustomEvent back.
        // `detail` is a JS object carrying the state JSON (or
        // `null` when no state is available). The parent
        // typically consumes this via [`super::Editor::get`]
        // which additionally runs the requested
        // [`super::ContentFormat`] over the result.
        {
            let state_provider = state_provider.clone();
            let host_for_response = host_el.clone();
            let cb = Closure::wrap(Box::new(move |_event: Event| {
                let state_json = state_provider(true).and_then(|state| state.to_json().ok());
                let detail_js = match state_json
                    .as_ref()
                    .and_then(|v| serde_wasm_bindgen::to_value(v).ok())
                {
                    Some(v) => v,
                    None => wasm_bindgen::JsValue::NULL,
                };
                let init = web_sys::CustomEventInit::new();
                init.set_bubbles(true);
                init.set_detail(&detail_js);
                let Ok(response) =
                    CustomEvent::new_with_event_init_dict(EXPORT_STATE_RESULT_EVENT, &init)
                else {
                    return;
                };
                let _ = host_for_response.dispatch_event(&response);
            }) as Box<dyn FnMut(Event)>);
            let _ = host_el.add_event_listener_with_callback(
                EXPORT_STATE_REQUEST_EVENT,
                cb.as_ref().unchecked_ref(),
            );
            slot.borrow_mut().push(cb);
        }

        // Lightweight selection-context bridge. Unlike the full state export
        // above, this does not serialize or reconstruct the document. It reads
        // the cached state with the live DOM selection injected and returns the
        // compact snapshot used by external bubble menus and toolbars.
        {
            let state_provider = state_provider.clone();
            let surface_for_snapshot = surface_el.clone();
            let host_for_response = host_el.clone();
            let cb = Closure::wrap(Box::new(move |_event: Event| {
                let snapshot = state_provider(true)
                    .map(|state| super::selection_observer::capture(&state, &surface_for_snapshot));
                let detail = snapshot
                    .as_ref()
                    .and_then(|value| serde_wasm_bindgen::to_value(value).ok())
                    .unwrap_or(wasm_bindgen::JsValue::NULL);
                let init = web_sys::CustomEventInit::new();
                init.set_bubbles(true);
                init.set_detail(&detail);
                let Ok(response) =
                    CustomEvent::new_with_event_init_dict(SELECTION_SNAPSHOT_RESULT_EVENT, &init)
                else {
                    return;
                };
                let _ = host_for_response.dispatch_event(&response);
            }) as Box<dyn FnMut(Event)>);
            let _ = host_el.add_event_listener_with_callback(
                SELECTION_SNAPSHOT_REQUEST_EVENT,
                cb.as_ref().unchecked_ref(),
            );
            slot.borrow_mut().push(cb);
        }

        // Keep typed node-view selection/focus snapshots current even when the
        // browser changes its live selection without an editor transaction
        // (drag-select, focus entering/leaving component chrome, or a native
        // selectionchange). The shared observer coalesces those noisy DOM
        // events to one animation-frame read. `sync_selection` then resolves
        // only the old/new affected positions through the manager's index;
        // focus/editability transitions intentionally update every view.
        let selection_subscription = {
            let state_provider = state_provider.clone();
            let surface_for_read = surface_el.clone();
            let node_view_manager = node_view_manager.clone();
            let commit_slot = commit_slot.clone();
            SelectionChangeSubscription::subscribe(
                host_el.clone(),
                Some(surface_el.clone()),
                move || {
                    state_provider(true)
                        .map(|state| super::selection_observer::capture(&state, &surface_for_read))
                },
                move |snapshot| {
                    let state = commit_slot.borrow().state.clone();
                    let Some(state) = state else {
                        return;
                    };
                    node_view_manager.borrow_mut().sync_selection(
                        state.doc(),
                        &snapshot.selection,
                        snapshot.editable,
                        snapshot.focused,
                    );
                },
            )
        };
        pocopine::on_scope_unmount(move || drop(selection_subscription));

        let node_view_manager_for_release = node_view_manager.clone();
        let node_view_binding_for_release = node_view_binding;
        pocopine::on_before_subtree_release(&surface_el, move || {
            node_view_binding_for_release.invalidate();
            node_view_manager_for_release.borrow_mut().drain();
        });

        let slot_for_drop = slot.clone();
        pocopine::on_scope_unmount(move || {
            slot_for_drop.borrow_mut().clear();
        });

        // Mark the surface as ready. `Editor::is_ready` reads
        // this attribute to decide whether `set` / `clear` /
        // `dispatch` can fire immediately or need to retry on
        // the next animation frame.
        //
        // Stamped on this component's own `pp-ref="surface"`
        // element — the inner `<root class="pine-rich-text">`
        // div. Editor handles look up either way: an Editor
        // wrapping the custom-element host walks one
        // descendant via `.pine-rich-text` to find this
        // attribute, and an Editor that already targets the
        // inner reads it off `self.surface` directly. Neither
        // side has to know about the other's element layout.
        let ready_el = surface_el.clone();
        let _ = ready_el.set_attribute(crate::view::interop::SURFACE_READY_ATTR, "true");
        pocopine::on_scope_unmount(move || {
            // Surface is going away — drop the ready signal so
            // any in-flight rAF-retry stops early instead of
            // dispatching into a detached node.
            let _ = ready_el.remove_attribute(crate::view::interop::SURFACE_READY_ATTR);
        });

        // Repaint when the doc changes. The reconciler walks the model
        // and DOM trees together so unchanged siblings, node-view
        // chrome, focus, IME state, scroll position, and unrelated
        // event listeners survive.
        //
        // Cursor sync rules:
        // - Structural DOM patch (Text / Reconciled / Full) → sync.
        // - Attr-only patch → skip (node-view chrome clicks must not
        //   move the cursor).
        // - Unchanged doc → sync when the selection changed since the
        //   previous watcher fire. Cells also always sync: reselecting the
        //   same rectangle after a transient browser caret must clear that
        //   DOM range even though the semantic endpoints match the previous
        //   watcher value. Selection-only transactions (`select_all`,
        //   programmatic `set_selection`, etc.) land here; without this we'd
        //   leave visible browser selection out of step with the model.
        let surface_for_watch = surface_el;
        let last_doc_for_watch = last_doc;
        let last_selection_for_watch: Rc<RefCell<Option<crate::state::Selection>>> =
            Rc::new(RefCell::new(None));
        let runtime_for_watch = runtime;
        let node_view_manager_for_watch = node_view_manager;
        let commit_slot_for_watch = commit_slot.clone();
        let last_watch_gen_for_watch = last_watch_gen.clone();
        let reconcile_pending = Rc::new(Cell::new(false));
        // Watch the `u64` generation counter instead of the
        // full state JSON. Every dispatch / replacement path
        // refreshes the cache first and bumps
        // `root.doc_generation` second, so this handler can
        // unconditionally read the latest state out of the
        // cache without re-materialising from a wire JSON
        // value. The previous shape (`watch(&"doc", …)`)
        // forced the reactive scheduler to propagate a
        // multi-KB `Value` mutation on every keystroke; this
        // shape propagates a 8-byte integer.
        watch_scope_field_scoped::<u64, _>(scope, "doc_generation", move |_gen, _| {
            if reconcile_pending.replace(true) {
                return;
            }
            let reconcile_pending = reconcile_pending.clone();
            let surface_for_watch = surface_for_watch.clone();
            let last_doc_for_watch = last_doc_for_watch.clone();
            let last_selection_for_watch = last_selection_for_watch.clone();
            let runtime_for_watch = runtime_for_watch.clone();
            let node_view_manager_for_watch = node_view_manager_for_watch.clone();
            let commit_slot_for_watch = commit_slot_for_watch.clone();
            let last_watch_gen_for_watch = last_watch_gen_for_watch.clone();
            pocopine::defer_component_callback_for(scope, move || {
                reconcile_pending.set(false);
                let watch_started_at = perf_now_ms();
                let materialize_started_at = perf_now_ms();
                let (new_doc, cached_state) = {
                    let slot = commit_slot_for_watch.borrow();
                    let Some(state) = slot.state.as_ref() else {
                        return;
                    };
                    last_watch_gen_for_watch.set(slot.generation);
                    (state.doc().clone(), state.clone())
                };
                let materialize_ms = perf_now_ms() - materialize_started_at;
                let reconcile_started_at = perf_now_ms();
                let (reconcile_outcome, document_changed) = {
                    let old = last_doc_for_watch.borrow();
                    (
                        reconcile_surface_with_manager(
                            &runtime_for_watch,
                            &node_view_manager_for_watch,
                            &surface_for_watch,
                            &old,
                            &new_doc,
                        ),
                        *old != new_doc,
                    )
                };
                let reconcile_ms = perf_now_ms() - reconcile_started_at;
                let new_top_level_children = new_doc.child_count();
                let new_doc_size = new_doc.content_size();
                log_debug_json(debug_json, "watch.doc", || {
                    json!({
                        "dom_changed": reconcile_outcome.dom_changed(),
                        "patch": reconcile_outcome.as_str(),
                        "state": state_debug_json(&cached_state),
                    })
                });
                let mount_started_at = perf_now_ms();
                let editable = surface_is_editable(&surface_for_watch);
                let focused = surface_is_focused(&surface_for_watch);
                if document_changed {
                    node_view_manager_for_watch.borrow_mut().sync_tree(
                        &surface_for_watch,
                        &new_doc,
                        cached_state.selection(),
                        editable,
                        focused,
                    );
                } else {
                    node_view_manager_for_watch.borrow_mut().sync_selection(
                        &new_doc,
                        cached_state.selection(),
                        editable,
                        focused,
                    );
                }
                *last_doc_for_watch.borrow_mut() = new_doc;
                let mount_ms = perf_now_ms() - mount_started_at;
                let cursor_started_at = perf_now_ms();
                let selection_changed = {
                    let mut slot = last_selection_for_watch.borrow_mut();
                    let new_sel = cached_state.selection().clone();
                    let changed = slot.as_ref() != Some(&new_sel);
                    *slot = Some(new_sel);
                    changed
                };
                let should_sync_cursor = reconcile_outcome.should_sync_cursor()
                    || (matches!(reconcile_outcome, ReconcileOutcome::Unchanged)
                        && (selection_changed || cached_state.selection().is_cells()));
                if should_sync_cursor {
                    // Use the cached state directly — saves the
                    // `from_json` re-parse that
                    // `sync_cursor_from_doc` would do.
                    sync_cursor_from_state(&surface_for_watch, &cached_state);
                }
                let cursor_ms = perf_now_ms() - cursor_started_at;
                log_perf(debug_perf, "watch.doc", || {
                    json!({
                        "runtime": runtime_for_watch.name(),
                        "patch": reconcile_outcome.as_str(),
                        "dom_changed": reconcile_outcome.dom_changed(),
                        // `cache_hit` is now structurally always
                        // true — the watcher only triggers on
                        // generation bumps, and every bumper
                        // refreshes the cache first.
                        "cache_hit": true,
                        "doc_size": new_doc_size,
                        "top_level_children": new_top_level_children,
                        "materialize_ms": round_ms(materialize_ms),
                        "reconcile_ms": round_ms(reconcile_ms),
                        "mount_ms": round_ms(mount_ms),
                        "cursor_ms": round_ms(cursor_ms),
                        "total_ms": round_ms(perf_now_ms() - watch_started_at),
                    })
                });
            });
        });
    }
}

/// Initial mount paint. Always replaces the surface's `innerHTML` with
/// the rendered seed doc so any template-literal content (e.g. the
/// `<!-- TODO -->` placeholder in `PineRichTextRoot.poco`) is cleared
/// even when the seed doc is empty.
///
/// We can't go through the reconciler here because the reconciler's
/// diff returns `Unchanged` when the seed doc is structurally
/// identical to the sentinel `old_doc` (both empty paragraphs), and
/// it would leave the template literal in place. Subsequent
/// updates do go through the reconciler — by that point the surface
/// has real model-owned DOM to diff against.
fn paint_initial(surface: &Element, _old_doc: &Node, doc_json: &Value, runtime: &EditorRuntime) {
    let Some(new_doc) = materialize_doc(doc_json, runtime) else {
        return;
    };
    let html = crate::render::render_doc_to_html(runtime, &new_doc);
    if let Ok(html_el) = surface.clone().dyn_into::<web_sys::HtmlElement>() {
        html_el.set_inner_html(&html);
    }
}

fn materialize_doc(doc_json: &Value, runtime: &EditorRuntime) -> Option<Node> {
    EditorState::from_json(
        runtime.schema().clone(),
        runtime_plugins(runtime),
        doc_json.clone(),
    )
    .ok()
    .map(|s| s.doc().clone())
}

fn report_load_error(
    surface: &Element,
    runtime: &EditorRuntime,
    input: &Value,
    error: &impl std::fmt::Display,
) {
    report_load_error_message(surface, runtime, input, &error.to_string());
}

fn report_load_error_message(
    surface: &Element,
    runtime: &EditorRuntime,
    input: &Value,
    message: &str,
) {
    clear_load_error(surface);
    let _ = surface.set_attribute("data-pine-richtext-load-error", "true");
    let _ = surface.set_attribute("aria-invalid", "true");

    if let Some(document) = surface.owner_document()
        && let Ok(plan) = crate::render::dom_output::DomElementSpec::element("div")
            .and_then(|plan| plan.attr("class", "pine-richtext-load-error"))
            .and_then(|plan| plan.attr("data-pine-richtext-load-error-message", "true"))
            .and_then(|plan| plan.attr("role", "alert"))
            .and_then(|plan| {
                plan.text(format!(
                    "This document could not be loaded with runtime `{}`: {message}",
                    runtime.name().unwrap_or("<default>")
                ))
            })
        && let Ok(alert) = plan.materialize(&document)
        && let Some(parent) = surface.parent_node()
    {
        let _ = parent.insert_before(alert.as_ref(), surface.next_sibling().as_ref());
    }

    let detail = json!({
        "runtime": runtime.name(),
        "wire_fingerprint": runtime.wire_fingerprint(),
        "error": message,
        "input": input,
    });
    if let Ok(detail) = serde_wasm_bindgen::to_value(&detail) {
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&detail);
        if let Ok(event) = CustomEvent::new_with_event_init_dict(LOAD_ERROR_EVENT, &init) {
            let _ = surface.dispatch_event(&event);
        }
    }
}

fn clear_load_error(surface: &Element) {
    let _ = surface.remove_attribute("data-pine-richtext-load-error");
    let _ = surface.remove_attribute("aria-invalid");
    if let Some(next) = surface.next_element_sibling()
        && next.has_attribute("data-pine-richtext-load-error-message")
    {
        next.remove();
    }
}

fn normalize_live_selection(state: &EditorState, selection: Selection) -> Selection {
    match selection {
        Selection::Text { anchor, head } => {
            // A DOM Range that covers exactly one selectable atom is Pine's
            // browser representation of a node selection. Recover the typed
            // selection here so the next ArrowLeft/ArrowRight command sees
            // `Selection::Node` rather than overwriting it with a text range.
            let (from, to) = if anchor <= head {
                (anchor, head)
            } else {
                (head, anchor)
            };
            if from != to
                && let Some(node) = state
                    .doc()
                    .resolve(from)
                    .ok()
                    .and_then(|pos| pos.node_after())
                && from.saturating_add(node.node_size()) == to
                && !node.is_text()
                && state
                    .schema()
                    .node_type(node.type_name())
                    .is_ok_and(|node_type| node_type.is_atom() && node_type.is_selectable())
            {
                return Selection::node(from);
            }
            Selection::text_between_near(state.doc(), state.schema(), anchor, head, None)
                .unwrap_or(Selection::Text { anchor, head })
        }
        other => other,
    }
}

fn surface_is_editable(surface: &Element) -> bool {
    surface.get_attribute("contenteditable").as_deref() != Some("false")
}

fn surface_is_focused(surface: &Element) -> bool {
    let Some(document) = surface.owner_document() else {
        return false;
    };
    let Some(active) = document.active_element() else {
        return false;
    };
    active == *surface || surface.contains(Some(active.as_ref()))
}

/// Push the model selection into `window.getSelection()` so the visible
/// caret matches what the model thinks the cursor is. Operates directly
/// on a typed [`EditorState`] held in the cache — saves the
/// `from_json` re-parse the legacy `sync_cursor_from_doc` path
/// did on every keystroke.
fn sync_cursor_from_state(surface: &Element, state: &EditorState) {
    let (anchor, head) = match state.selection() {
        crate::state::Selection::Text { anchor, head } => (*anchor, *head),
        crate::state::Selection::Node { anchor } => {
            let head = state
                .doc()
                .resolve(*anchor)
                .ok()
                .and_then(|pos| pos.node_after())
                .map(|node| anchor.saturating_add(node.node_size()))
                .unwrap_or(*anchor);
            (*anchor, head)
        }
        crate::state::Selection::Cells { .. } => {
            // A rectangular table selection has no faithful single DOM Range.
            // Clear the browser caret; table view code paints selected cells
            // from the semantic model selection.
            if let Some(selection) =
                web_sys::window().and_then(|window| window.get_selection().ok().flatten())
            {
                let _ = selection.remove_all_ranges();
            }
            return;
        }
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

/// Slim state payload for [`DOC_CHANGED_EVENT`]: `{ doc, selection,
/// stored_marks, steps }`. `plugin_state` is intentionally omitted —
/// it grows with the user's history and isn't read by any of the
/// in-tree subscribers (see the call site for the rationale).
/// Consumers that need a full state snapshot can dispatch
/// [`EXPORT_STATE_REQUEST_EVENT`] instead. `steps` is the
/// transaction's serialized steps, for a collab binding that maps
/// them to an incremental CRDT write ([`super::Editor::on_update_steps`]).
fn doc_changed_event_payload(state: &EditorState, steps: Vec<Value>) -> Value {
    json!({
        "doc": state.doc(),
        "selection": state.selection(),
        "stored_marks": state.stored_marks(),
        "steps": steps,
    })
}

/// Dispatch the [`DOC_CHANGED_EVENT`] CustomEvent at `surface`
/// with the current state JSON as `detail`. Consumers
/// register via [`super::Editor::on_update`].
fn dispatch_doc_changed_event(surface: &Element, state_json: &Value) {
    let Ok(detail_js) = serde_wasm_bindgen::to_value(state_json) else {
        return;
    };
    let init = web_sys::CustomEventInit::new();
    init.set_bubbles(true);
    init.set_detail(&detail_js);
    let Ok(event) = CustomEvent::new_with_event_init_dict(DOC_CHANGED_EVENT, &init) else {
        return;
    };
    let _ = surface.dispatch_event(&event);
}

/// Cheap payload for [`CHANGE_EVENT`] ([`super::ChangeInfo`]): a generation
/// counter, whether the doc is blank, and the caret's textblock prefix. Unlike
/// [`doc_changed_event_payload`] it never serializes the document — `empty` is
/// O(1) and `caret_prefix` is O(block) — so per-keystroke consumers stay flat
/// regardless of document size.
fn change_event_payload(state: &EditorState, generation: u64) -> super::ChangeInfo {
    /// Cap on `caret_prefix` length: a mention token is short, and an unbounded
    /// block prefix would be O(block) for a huge single paragraph — the point of
    /// this event is to stay flat. Positions map 1:1 to chars inside a textblock.
    const CARET_PREFIX_WINDOW: usize = 256;

    let doc = state.doc();
    // Resting empty state = a single empty textblock. O(1).
    let empty = doc.child_count() == 1
        && doc
            .child(0)
            .map(|child| child.content_size() == 0)
            .unwrap_or(true);
    // Text in the caret's textblock, up to the caret (bounded). Empty for a
    // ranged (non-collapsed) selection. O(window), never touches the rest of the
    // doc — so a mention picker stays flat regardless of paragraph size.
    let selection = state.selection();
    let caret_prefix = if selection.is_empty(doc) {
        let caret = selection.from(doc);
        doc.resolve(caret)
            .ok()
            .and_then(|resolved| {
                let block_start = resolved.start(resolved.depth())?;
                let start = block_start.max(caret.saturating_sub(CARET_PREFIX_WINDOW));
                doc.text_between(start, caret, "").ok()
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    super::ChangeInfo {
        generation,
        empty,
        caret_prefix,
    }
}

/// Dispatch the [`CHANGE_EVENT`] CustomEvent at `surface`. Consumers register
/// via [`super::Editor::on_change`]. `ChangeInfo` is a struct, so it serializes
/// to a plain JS object (not a `Map`), which `from_value::<ChangeInfo>` reads.
fn dispatch_change_event(surface: &Element, change: &super::ChangeInfo) {
    let Ok(detail_js) = serde_wasm_bindgen::to_value(change) else {
        return;
    };
    let init = web_sys::CustomEventInit::new();
    init.set_bubbles(true);
    init.set_detail(&detail_js);
    let Ok(event) = CustomEvent::new_with_event_init_dict(CHANGE_EVENT, &init) else {
        return;
    };
    let _ = surface.dispatch_event(&event);
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

/// Log a debug-JSON event. `payload` is a **closure** so the (potentially
/// whole-document) JSON is built only when `enabled` — passing `json!(…)`
/// directly would serialize eagerly on every commit even with debug off, which
/// was the dominant per-keystroke cost (`state_debug_json` → `EditorState::to_json`).
fn log_debug_json(enabled: bool, event: &str, payload: impl FnOnce() -> Value) {
    if !enabled {
        return;
    }
    let value = json!({
        "debug_version": DEBUG_LOG_VERSION,
        "event": event,
        "payload": payload(),
    });
    let Ok(message) = serde_json::to_string_pretty(&value) else {
        return;
    };
    log_to_console(&message);
}

fn log_perf(enabled: bool, event: &str, payload: impl FnOnce() -> Value) {
    if !enabled {
        return;
    }
    let value = json!({
        "debug_version": DEBUG_LOG_VERSION,
        "event": event,
        "payload": payload(),
    });
    let Ok(message) = serde_json::to_string(&value) else {
        return;
    };
    log_to_console_with_label("pine-richtext:perf", &message);
}

fn round_ms(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(target_arch = "wasm32")]
fn perf_now_ms() -> f64 {
    // `performance.now()` gives sub-millisecond resolution. `Date.now()`
    // resolves to whole milliseconds and hides sub-ms hot paths in the
    // dispatch fan-out. Fallback to `Date.now()` if `window.performance`
    // is unavailable (no DOM — happens in some test embeddings).
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
fn log_to_console(message: &str) {
    log_to_console_with_label("pine-richtext:json", message);
}

#[cfg(not(target_arch = "wasm32"))]
fn log_to_console(message: &str) {
    let _ = message;
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
    use super::CommandRequest;
    use serde_json::json;

    #[test]
    fn custom_variant_deserializes_from_kind_tagged_payload() {
        let value = json!({
            "kind": "custom",
            "name": "wrap_in_bullet_list",
            "args": {}
        });
        let req: CommandRequest = serde_json::from_value(value).expect("deserialize");
        match req {
            CommandRequest::Custom { name, args } => {
                assert_eq!(name, "wrap_in_bullet_list");
                assert!(args.is_object());
            }
            other => panic!("expected Custom, got {:?}", other),
        }
    }

    #[test]
    fn custom_variant_defaults_args_when_omitted() {
        let value = json!({ "kind": "custom", "name": "undo" });
        let req: CommandRequest = serde_json::from_value(value).expect("deserialize");
        match req {
            CommandRequest::Custom { name, args } => {
                assert_eq!(name, "undo");
                assert!(args.is_null());
            }
            other => panic!("expected Custom, got {:?}", other),
        }
    }
}
