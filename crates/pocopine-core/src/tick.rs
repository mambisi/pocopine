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

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;

/// Schedule `f` on the next microtask.
pub fn next<F: FnOnce() + 'static>(f: F) {
    next_dyn(Box::new(f));
}

/// Schedule `f` on the next animation frame.
pub fn next_frame<F: FnOnce() + 'static>(f: F) {
    next_frame_dyn(Box::new(f));
}

/// Schedule `f` after the current reactive flush yields — a
/// `setTimeout(_, 0)` macrotask. Use for post-`Handle::update`
/// DOM mutations like re-focusing an element that the reactive
/// walk just blurred. Strictly later than [`next`] (microtask).
pub fn after_flush<F: FnOnce() + 'static>(f: F) {
    after_flush_dyn(Box::new(f));
}

// RFC-058 Phase 6.5 — type-erased helpers. The public generic
// shims monomorphise once per call site (cheap — they just forward
// to the boxed variant). The boxed variants are the only versions
// that get instantiated for `Closure::once_into_js`, collapsing
// N call sites' worth of `Closure` boilerplate into a single copy
// in the wasm. Twiggy showed `tick::next_frame::<F>` totalling
// ~5 KB of bloat across pp-for / pp-if / transition closure
// types before this consolidation.

fn next_dyn(f: Box<dyn FnOnce() + 'static>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let js = Closure::once_into_js(f);
    window.queue_microtask(js.unchecked_ref());
}

fn next_frame_dyn(f: Box<dyn FnOnce() + 'static>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let js = Closure::once_into_js(move |_: JsValue| f());
    let _ = window.request_animation_frame(js.unchecked_ref());
}

fn after_flush_dyn(f: Box<dyn FnOnce() + 'static>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let js = Closure::once_into_js(f);
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(js.unchecked_ref(), 0);
}
