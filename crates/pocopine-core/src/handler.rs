//! `HandlerDispatch` — the indirection the `#[component]` and `#[handlers]`
//! macros use to coordinate. `#[component]` emits a `ComponentState::invoke`
//! that delegates here; `#[handlers]` emits the actual match-by-name body.

use js_sys::Array;
use wasm_bindgen::JsValue;

pub trait HandlerDispatch {
    fn invoke_handler(&mut self, key: &str, args: &Array) -> JsValue;
}
