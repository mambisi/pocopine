//! Scheduling utilities — RFC-014 §3.2.
//!
//! `next(f)` defers `f` to the next microtask via `queueMicrotask` —
//! fires after the current synchronous frame commits (so reactive
//! DOM updates have landed), but before the browser paints. Use for
//! "focus the input after the dialog mounts", "measure right after
//! `pp-show` toggles on", etc.
//!
//! `next_frame(f)` defers `f` to `requestAnimationFrame` — one paint
//! tick later. Use when layout / computed style must already be
//! resolved (e.g. `getBoundingClientRect` against freshly applied
//! transition classes).

use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// Schedule `f` on the next microtask.
pub fn next<F: FnOnce() + 'static>(f: F) {
    let Some(window) = web_sys::window() else { return };
    let js = Closure::once_into_js(f);
    window.queue_microtask(js.unchecked_ref());
}

/// Schedule `f` on the next animation frame.
pub fn next_frame<F: FnOnce() + 'static>(f: F) {
    let Some(window) = web_sys::window() else { return };
    let js = Closure::once_into_js(move |_: JsValue| f());
    let _ = window.request_animation_frame(js.unchecked_ref());
}
