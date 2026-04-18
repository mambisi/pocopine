//! `pp-model="field"` — two-way input binding.
//!
//! Covers `<input>`, `<textarea>`, `<select>`, and checkbox/radio inputs. The
//! read side is an effect that updates the DOM element's value when the
//! field changes; the write side listens for `input`/`change` and sets the
//! field on the proxy so a `trigger` fires.

use js_sys::{Array, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{Event, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};

use super::DirectiveCall;
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::track_effect_on;

pub fn run(call: &DirectiveCall) {
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
            let v = Reflect::get(&proxy_r, &JsValue::from_str(&key_r))
                .unwrap_or(JsValue::UNDEFINED);
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
        // Go through the proxy so the set trap fires `trigger`.
        let _ = Reflect::set(&proxy_w, &JsValue::from_str(&key_w), &v);
    }) as Box<dyn FnMut(Event)>);

    let event_name = if lazy { "change" } else { "input" };
    let _ = el.add_event_listener_with_callback(event_name, handler.as_ref().unchecked_ref());
    handler.forget();

    // Suppress unused-import warning if `Array` stops being used when the
    // branch below gets specialized.
    let _ = Array::new;
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
