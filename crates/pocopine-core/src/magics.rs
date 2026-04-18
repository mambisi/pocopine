//! Magics — `$el`, `$refs`, `$dispatch`. Resolved by the proxy's `get` trap
//! whenever a key starting with `$` is read.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{CustomEvent, CustomEventInit, Element};

use crate::reactive::ScopeId;
use crate::router::route_proxy;
use crate::scope::current_el;
use crate::store::stores_object;

pub fn resolve(key: &str, _scope_id: ScopeId) -> JsValue {
    match key {
        "$el" => current_el().map(JsValue::from).unwrap_or(JsValue::UNDEFINED),
        "$refs" => build_refs(),
        "$dispatch" => build_dispatch(),
        "$store" => stores_object(),
        "$route" => route_proxy(),
        _ => JsValue::UNDEFINED,
    }
}

fn build_refs() -> JsValue {
    let obj = Object::new();
    let Some(el) = current_el() else { return obj.into() };
    // Walk descendants and collect pp-ref="name".
    let nodes = match el.query_selector_all("[pp-ref]") {
        Ok(n) => n,
        Err(_) => return obj.into(),
    };
    for i in 0..nodes.length() {
        let Some(n) = nodes.get(i) else { continue };
        let Ok(e) = n.dyn_into::<Element>() else { continue };
        if let Some(name) = e.get_attribute("pp-ref") {
            let _ = Reflect::set(&obj, &name.into(), &e.into());
        }
    }
    obj.into()
}

fn build_dispatch() -> JsValue {
    // $dispatch(name, detail?) -> void; dispatches a bubbling CustomEvent
    // from the current element.
    let closure = Closure::wrap(Box::new(move |name: JsValue, detail: JsValue| {
        let Some(el) = current_el() else { return };
        let Some(name) = name.as_string() else { return };
        let init = CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&detail);
        if let Ok(ev) = CustomEvent::new_with_event_init_dict(&name, &init) {
            let _ = el.dispatch_event(&ev);
        }
    }) as Box<dyn Fn(JsValue, JsValue)>);
    let f: Function = closure.as_ref().unchecked_ref::<Function>().clone();
    // Leaked intentionally — the scope holds the reference implicitly.
    closure.forget();
    f.into()
}

// Silence unused-import warning if the `Array` import drifts; the magics
// module historically grows to expose `$watch`/`$store`, which will take
// `Array` args.
#[allow(dead_code)]
fn _unused(_: Array) {}
