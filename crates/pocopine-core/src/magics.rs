//! Magics — `$el`, `$refs`, `$dispatch`. Resolved by the proxy's `get` trap
//! whenever a key starting with `$` is read.

use js_sys::{Array, Function};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{CustomEvent, CustomEventInit};

use crate::id;
use crate::reactive::ScopeId;
use crate::refs;
use crate::router::route_proxy;
use crate::scope::current_el;
use crate::store::stores_object;

pub fn resolve(key: &str, scope_id: ScopeId) -> JsValue {
    match key {
        "$el" => current_el().map(JsValue::from).unwrap_or(JsValue::UNDEFINED),
        "$refs" => refs::as_object(scope_id),
        "$dispatch" => build_dispatch(),
        "$store" => stores_object(),
        "$route" => route_proxy(),
        "$id" => JsValue::from_str(&id::generate(scope_id)),
        _ => JsValue::UNDEFINED,
    }
}

fn build_dispatch() -> JsValue {
    // $dispatch(name, detail?) -> void; dispatches a bubbling CustomEvent
    // from the current element.
    let closure = Closure::wrap(Box::new(move |name: JsValue, detail: JsValue| {
        let Some(name) = name.as_string() else { return };
        dispatch_event(&name, &detail);
    }) as Box<dyn Fn(JsValue, JsValue)>);
    let f: Function = closure.as_ref().unchecked_ref::<Function>().clone();
    // Leaked intentionally — the scope holds the reference implicitly.
    closure.forget();
    f.into()
}

/// Rust-facing version of `$dispatch` — fires a bubbling
/// `CustomEvent(name, { detail })` from the current directive
/// element. Handlers use this to signal up through the component
/// boundary (notably for `pp-model`'s `pp:update:model` event, per
/// RFC-009).
pub fn dispatch_event(name: &str, detail: &JsValue) {
    let Some(el) = current_el() else { return };
    let init = CustomEventInit::new();
    init.set_bubbles(true);
    init.set_detail(detail);
    if let Ok(ev) = CustomEvent::new_with_event_init_dict(name, &init) {
        let _ = el.dispatch_event(&ev);
    }
}

// Silence unused-import warning if the `Array` import drifts; the magics
// module historically grows to expose `$watch`/`$store`, which will take
// `Array` args.
#[allow(dead_code)]
fn _unused(_: Array) {}
