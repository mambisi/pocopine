//! `pp-ref="<name>"` — pin the element under its enclosing scope so
//! Rust handlers can reach it via [`crate::refs`]. Setup-only: no
//! reactive effect is registered.

use web_sys::console;
use wasm_bindgen::JsValue;

use super::DirectiveCall;
use crate::refs;

pub fn run(call: &DirectiveCall) {
    let name = call.value.trim();
    if name.is_empty() {
        console::error_1(&JsValue::from_str(
            "pp-ref: expected a non-empty name (e.g. pp-ref=\"input\")",
        ));
        return;
    }
    refs::register(call.scope_id, name, call.el);
}
