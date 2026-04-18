//! `pp-text="field"` — set `textContent` from a scope field, reactively.

use js_sys::Reflect;
use wasm_bindgen::JsValue;

use super::DirectiveCall;
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::track_effect_on;

pub fn run(call: &DirectiveCall) {
    let el = call.el.clone();
    let proxy = call.proxy.clone();
    let key = call.value.clone();
    let id = effect(move || {
        let el_for_magic = el.clone();
        with_current_el(&el_for_magic, || {
            let v = Reflect::get(&proxy, &JsValue::from_str(&key)).unwrap_or(JsValue::UNDEFINED);
            el.set_text_content(Some(&js_to_string(&v)));
        });
    });
    track_effect_on(call.el, id);
}

fn js_to_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(|n| n.to_string()))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| {
            js_sys::JSON::stringify(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default()
        })
}
