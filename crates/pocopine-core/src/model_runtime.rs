use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use js_sys::JSON;
use wasm_bindgen::JsValue;
use web_sys::{CustomEvent, CustomEventInit, Element};

use crate::reactive::ScopeId;
use crate::scope::Scope;
use crate::tick;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteOrigin {
    ParentModelIn,
    LocalHandler,
    SetupSeed,
    ObserveMirror,
}

#[derive(Clone)]
struct PendingModel {
    wire_name: String,
    detail: JsValue,
    origin: WriteOrigin,
}

#[derive(Default)]
struct ScopeModelRuntime {
    emit_el: Option<Element>,
    pending: HashMap<String, PendingModel>,
}

thread_local! {
    static WRITE_ORIGIN: Cell<WriteOrigin> = const { Cell::new(WriteOrigin::LocalHandler) };
    static MODEL_RUNTIME: RefCell<HashMap<ScopeId, ScopeModelRuntime>> =
        RefCell::new(HashMap::new());
    static FLUSH_PENDING: RefCell<Vec<ScopeId>> = const { RefCell::new(Vec::new()) };
    static FLUSH_SCHEDULED: Cell<bool> = const { Cell::new(false) };
}

pub fn with_write_origin<R>(origin: WriteOrigin, f: impl FnOnce() -> R) -> R {
    let prev = WRITE_ORIGIN.with(|c| c.replace(origin));
    let out = f();
    WRITE_ORIGIN.with(|c| c.set(prev));
    out
}

pub fn current_write_origin() -> WriteOrigin {
    WRITE_ORIGIN.with(|c| c.get())
}

pub fn capture_emit_el(scope_id: ScopeId, el: &Element) {
    MODEL_RUNTIME.with(|m| {
        m.borrow_mut().entry(scope_id).or_default().emit_el = Some(el.clone());
    });
}

pub fn clear_scope(scope_id: ScopeId) {
    MODEL_RUNTIME.with(|m| {
        m.borrow_mut().remove(&scope_id);
    });
    FLUSH_PENDING.with(|q| {
        q.borrow_mut().retain(|id| *id != scope_id);
    });
}

pub fn emit_target(scope_id: ScopeId) -> Option<Element> {
    MODEL_RUNTIME.with(|m| m.borrow().get(&scope_id).and_then(|rt| rt.emit_el.clone()))
}

pub fn resolve_model_key(scope_id: ScopeId, wire_name: &str) -> Option<String> {
    let scope = Scope::find(scope_id)?;
    let state = scope.state.borrow();
    for key in state.keys() {
        if state.is_model(key) && state.model_name(key) == Some(wire_name) {
            return Some((*key).to_string());
        }
    }
    if state.keys().contains(&wire_name) {
        return Some(wire_name.to_string());
    }
    None
}

pub fn with_scope_write<R>(scope_id: ScopeId, origin: WriteOrigin, f: impl FnOnce() -> R) -> R {
    let before = snapshot_models(scope_id);
    let out = with_write_origin(origin, f);
    let after = snapshot_models(scope_id);
    queue_changed_models(scope_id, origin, before, after);
    out
}

fn snapshot_models(scope_id: ScopeId) -> HashMap<String, (String, JsValue, String)> {
    let Some(scope) = Scope::find(scope_id) else {
        return HashMap::new();
    };
    // Re-entrant `Scope::invoke` (handler → DOM mutation → synchronous
    // event dispatch → another `Scope::invoke` for the same scope)
    // arrives here with the outer's `borrow_mut` still held. We
    // cannot snapshot under a live mutable borrow, but panicking on
    // the inner invoke is the wrong call — the outer write is still
    // valid; we just lose the inner write's diff. Skip silently:
    // model `pp:update:*` events for the inner invoke degrade to
    // best-effort rather than aborting the whole event chain.
    let Ok(state) = scope.state.try_borrow() else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for key in state.keys() {
        if !state.is_model(key) {
            continue;
        }
        let value = state.get_model_value(key);
        let Some(fingerprint) = fingerprint(&value) else {
            continue;
        };
        let wire_name = state.model_name(key).unwrap_or(key).to_string();
        out.insert((*key).to_string(), (wire_name, value, fingerprint));
    }
    out
}

fn fingerprint(value: &JsValue) -> Option<String> {
    if value.is_undefined() {
        return Some("undefined".into());
    }
    JSON::stringify(value).ok()?.as_string()
}

fn queue_changed_models(
    scope_id: ScopeId,
    origin: WriteOrigin,
    before: HashMap<String, (String, JsValue, String)>,
    after: HashMap<String, (String, JsValue, String)>,
) {
    MODEL_RUNTIME.with(|m| {
        let mut map = m.borrow_mut();
        let runtime = map.entry(scope_id).or_default();
        for (key, (wire_name, detail, after_fp)) in after {
            let changed = before
                .get(&key)
                .map(|(_, _, before_fp)| before_fp != &after_fp)
                .unwrap_or(true);
            if !changed {
                continue;
            }
            runtime.pending.insert(
                key,
                PendingModel {
                    wire_name,
                    detail,
                    origin,
                },
            );
        }
    });
    schedule_flush(scope_id);
}

fn schedule_flush(scope_id: ScopeId) {
    FLUSH_PENDING.with(|q| {
        let mut q = q.borrow_mut();
        if !q.contains(&scope_id) {
            q.push(scope_id);
        }
    });
    if FLUSH_SCHEDULED.with(|f| f.replace(true)) {
        return;
    }
    tick::next(|| {
        FLUSH_SCHEDULED.with(|f| f.set(false));
        flush_pending();
    });
}

fn flush_pending() {
    let scope_ids = FLUSH_PENDING.with(|q| q.borrow_mut().drain(..).collect::<Vec<_>>());
    for scope_id in scope_ids {
        let (emit_el, pending) = MODEL_RUNTIME.with(|m| {
            let mut map = m.borrow_mut();
            let Some(runtime) = map.get_mut(&scope_id) else {
                return (None, Vec::new());
            };
            let emit_el = runtime.emit_el.clone();
            let pending = runtime
                .pending
                .drain()
                .map(|(_, item)| item)
                .collect::<Vec<_>>();
            (emit_el, pending)
        });
        let Some(el) = emit_el else { continue };
        for item in pending {
            // RFC-044 §5.5 origin suppression — none of these should
            // advance the public two-way contract outward:
            //
            //   ParentModelIn — a parent's `pp-model:<field>` write.
            //     Echoing it back out would immediately rewrite the
            //     same value on the parent; hard loop.
            //   SetupSeed — initial hydration. Not a user-visible
            //     contract advance; parent already drove the value.
            //   ObserveMirror — a `#[observe(KEY)]` mirror from a
            //     provided root Handle. Re-publishing on every
            //     observed change would ping-pong: Root write →
            //     observe fires → child emit → outer parent's
            //     pp-model listener → Root write back. Authors who
            //     want observed changes to re-publish must do so
            //     from an explicit handler (which runs under
            //     LocalHandler origin and thus emits).
            if matches!(
                item.origin,
                WriteOrigin::ParentModelIn | WriteOrigin::SetupSeed | WriteOrigin::ObserveMirror
            ) {
                #[cfg(any(debug_assertions, feature = "devtools"))]
                testing::record_suppressed(scope_id, &item);
                continue;
            }
            #[cfg(any(debug_assertions, feature = "devtools"))]
            testing::record_emitted(scope_id, &item);
            let init = CustomEventInit::new();
            init.set_bubbles(true);
            init.set_detail(&item.detail);
            let name = format!("pp:update:{}", item.wire_name);
            if let Ok(ev) = CustomEvent::new_with_event_init_dict(&name, &init) {
                let _ = el.dispatch_event(&ev);
            }
        }
    }
}

/// Cfg-gated model-emission log for unit tests.
///
/// Mirrors the `stats()` / `listener_count()` pattern used elsewhere
/// in the crate — compiled under `debug_assertions` and when the
/// `devtools` feature is on, absent from release binaries built
/// `--no-default-features --release`.
///
/// Usage from an author test:
///
/// ```ignore
/// use pocopine_core::model_runtime::testing::{drain_emitted, reset};
///
/// reset();
/// scope.invoke("select_date", js_sys::Array::of1(&"2024-06-15".into()).as_ref());
/// pocopine_core::tick::flush_sync();
/// let events = drain_emitted();
/// assert_eq!(events.len(), 1);
/// assert_eq!(events[0].wire_name, "value");
/// ```
///
/// Semantics:
///   - `drain_emitted` / `drain_suppressed` snapshot + clear the
///     corresponding log. Subsequent calls return `Vec::new()`
///     until new events land.
///   - Entries are recorded at flush time, AFTER origin-based
///     suppression has run — so `drain_emitted` contains only
///     writes the runtime chose to publish, and `drain_suppressed`
///     contains writes it silenced (ParentModelIn / SetupSeed /
///     ObserveMirror). Inspecting the latter is how a test verifies
///     "parent mirror-in did NOT echo back out" or "observe-driven
///     write did NOT re-publish."
///   - `reset` clears both logs + the pending-emit queue; use
///     between tests that share a thread-local runtime.
#[cfg(any(debug_assertions, feature = "devtools"))]
pub mod testing {
    use super::{PendingModel, WriteOrigin};
    use crate::reactive::ScopeId;
    use std::cell::RefCell;
    use wasm_bindgen::JsValue;

    /// One observed model emission / suppression, snapshotted at
    /// flush time.
    #[derive(Clone, Debug)]
    pub struct ModelEvent {
        pub scope_id: ScopeId,
        pub wire_name: String,
        pub detail: JsValue,
        pub origin: WriteOrigin,
    }

    thread_local! {
        static EMITTED_LOG: RefCell<Vec<ModelEvent>> = const { RefCell::new(Vec::new()) };
        static SUPPRESSED_LOG: RefCell<Vec<ModelEvent>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record_emitted(scope_id: ScopeId, item: &PendingModel) {
        EMITTED_LOG.with(|l| {
            l.borrow_mut().push(ModelEvent {
                scope_id,
                wire_name: item.wire_name.clone(),
                detail: item.detail.clone(),
                origin: item.origin,
            });
        });
    }

    pub(super) fn record_suppressed(scope_id: ScopeId, item: &PendingModel) {
        SUPPRESSED_LOG.with(|l| {
            l.borrow_mut().push(ModelEvent {
                scope_id,
                wire_name: item.wire_name.clone(),
                detail: item.detail.clone(),
                origin: item.origin,
            });
        });
    }

    /// Snapshot + clear every model event emitted to the DOM since
    /// the last call. Ordered by flush arrival.
    pub fn drain_emitted() -> Vec<ModelEvent> {
        EMITTED_LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    /// Snapshot + clear every model write the runtime suppressed
    /// via origin rules. Use this to assert that e.g. a parent
    /// `pp-model` write didn't echo back out.
    pub fn drain_suppressed() -> Vec<ModelEvent> {
        SUPPRESSED_LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    /// Clear both logs AND the pending-emit queue. Call between
    /// tests that share the thread-local model runtime.
    pub fn reset() {
        EMITTED_LOG.with(|l| l.borrow_mut().clear());
        SUPPRESSED_LOG.with(|l| l.borrow_mut().clear());
        super::MODEL_RUNTIME.with(|m| m.borrow_mut().clear());
        super::FLUSH_PENDING.with(|q| q.borrow_mut().clear());
        super::FLUSH_SCHEDULED.with(|f| f.set(false));
    }
}
