//! `pp-html="field"` — set `innerHTML` from a scope field, reactively.
//!
//! As with Alpine's `x-html`, the consumer is trusted not to feed untrusted
//! input here; there is no sanitization.

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
        with_current_el(&el.clone(), || {
            let v = Reflect::get(&proxy, &JsValue::from_str(&key))
                .unwrap_or(JsValue::UNDEFINED);
            let s = v.as_string().unwrap_or_default();
            el.set_inner_html(&s);
        });
    });
    track_effect_on(call.el, id);
}
