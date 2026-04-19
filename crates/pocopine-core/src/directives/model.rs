//! `pp-model="field"` — two-way input binding.
//!
//! Two code paths:
//!
//! * **Native input** (`<input>`, `<textarea>`, `<select>`). Effect
//!   writes element value from `proxy[key]`; `input`/`change`
//!   listener writes element value back through `write_path`.
//! * **Registered component tag** (`<pine-input pp-model="name">`).
//!   Per [RFC-009](../../../../rfcs/rfc-009-pp-model-components.md):
//!   effect mirrors parent's `proxy[key]` into the child's `model`
//!   prop; listener on `pp:update:model` writes `event.detail` back
//!   to `proxy[key]`.

use js_sys::Reflect;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{CustomEvent, Event, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};

use super::DirectiveCall;
use crate::path::{resolve_path, write_path};
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::{child_component_proxy, track_effect_on};

pub fn run(call: &DirectiveCall) {
    if let Some(child_proxy) = child_component_proxy(call.el) {
        run_component(call, child_proxy);
    } else {
        run_native(call);
    }
}

// ─── component-boundary path (RFC-009) ────────────────────────────

fn run_component(call: &DirectiveCall, child_proxy: JsValue) {
    let parent_proxy = call.proxy.clone();
    let key = call.value.clone();
    let el = call.el.clone();

    // Parent → child: mirror proxy[key] into the child's `model`.
    // Same shape as pp-bind's child-prop path.
    let parent_r = parent_proxy.clone();
    let key_r = key.clone();
    let child_r = child_proxy.clone();
    let el_for_track = el.clone();
    let id = effect(move || {
        with_current_el(&el_for_track.clone(), || {
            let v = resolve_path(&parent_r, &key_r);
            let _ = Reflect::set(&child_r, &JsValue::from_str("model"), &v);
        });
    });
    track_effect_on(call.el, id);

    // Child → parent: `pp:update:model` custom event. `event.detail`
    // is the new value. Write it back through `write_path` so dotted
    // keys (`$store.foo.bar`) continue to work.
    let parent_w = parent_proxy;
    let key_w = key;
    let listener = Closure::wrap(Box::new(move |ev: Event| {
        let Ok(ce) = ev.dyn_into::<CustomEvent>() else { return };
        let detail = ce.detail();
        let _ = write_path(&parent_w, &key_w, &detail);
    }) as Box<dyn FnMut(Event)>);
    let _ = el.add_event_listener_with_callback(
        "pp:update:model",
        listener.as_ref().unchecked_ref(),
    );
    listener.forget();
}

// ─── native input path ────────────────────────────────────────────

fn run_native(call: &DirectiveCall) {
    let proxy = call.proxy.clone();
    let key = call.value.clone();
    let el = call.el.clone();
    let number = call.modifiers.iter().any(|m| m == "number");
    let lazy = call.modifiers.iter().any(|m| m == "lazy");

    // Read side: proxy[key] -> element value.
    let proxy_r = proxy.clone();
    let key_r = key.clone();
    let el_r = el.clone();
    let id = effect(move || {
        with_current_el(&el_r.clone(), || {
            let v = resolve_path(&proxy_r, &key_r);
            write_to_element(&el_r, &v);
        });
    });
    track_effect_on(call.el, id);

    // Write side: input event -> proxy[key] = element value.
    let proxy_w = proxy.clone();
    let key_w = key.clone();
    let el_w = el.clone();
    let handler = Closure::wrap(Box::new(move |_ev: Event| {
        let v = read_from_element(&el_w, number);
        let _ = write_path(&proxy_w, &key_w, &v);
    }) as Box<dyn FnMut(Event)>);

    let event_name = if lazy { "change" } else { "input" };
    let _ = el.add_event_listener_with_callback(event_name, handler.as_ref().unchecked_ref());
    handler.forget();
}

fn write_to_element(el: &web_sys::Element, v: &JsValue) {
    if let Some(inp) = el.dyn_ref::<HtmlInputElement>() {
        match inp.type_().as_str() {
            "checkbox" => inp.set_checked(!v.is_falsy()),
            "radio" => {
                let want = v.as_string().unwrap_or_default();
                inp.set_checked(inp.value() == want);
            }
            _ => inp.set_value(&as_string(v)),
        }
        return;
    }
    if let Some(s) = el.dyn_ref::<HtmlSelectElement>() {
        s.set_value(&as_string(v));
        return;
    }
    if let Some(t) = el.dyn_ref::<HtmlTextAreaElement>() {
        t.set_value(&as_string(v));
    }
}

fn read_from_element(el: &web_sys::Element, number: bool) -> JsValue {
    if let Some(inp) = el.dyn_ref::<HtmlInputElement>() {
        return match inp.type_().as_str() {
            "checkbox" => JsValue::from_bool(inp.checked()),
            _ => coerce(inp.value(), number),
        };
    }
    if let Some(s) = el.dyn_ref::<HtmlSelectElement>() {
        return coerce(s.value(), number);
    }
    if let Some(t) = el.dyn_ref::<HtmlTextAreaElement>() {
        return coerce(t.value(), number);
    }
    JsValue::UNDEFINED
}

fn coerce(s: String, number: bool) -> JsValue {
    if number {
        if let Ok(n) = s.parse::<f64>() {
            return JsValue::from_f64(n);
        }
    }
    JsValue::from_str(&s)
}

fn as_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(|n| n.to_string()))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_default()
}
