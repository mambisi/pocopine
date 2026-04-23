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
        m.borrow_mut()
            .entry(scope_id)
            .or_default()
            .emit_el = Some(el.clone());
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
    MODEL_RUNTIME.with(|m| {
        m.borrow()
            .get(&scope_id)
            .and_then(|rt| rt.emit_el.clone())
    })
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

pub fn with_scope_write<R>(
    scope_id: ScopeId,
    origin: WriteOrigin,
    f: impl FnOnce() -> R,
) -> R {
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
    let state = scope.state.borrow();
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
            let pending = runtime.pending.drain().map(|(_, item)| item).collect::<Vec<_>>();
            (emit_el, pending)
        });
        let Some(el) = emit_el else { continue };
        for item in pending {
            if matches!(item.origin, WriteOrigin::ParentModelIn | WriteOrigin::SetupSeed) {
                continue;
            }
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
