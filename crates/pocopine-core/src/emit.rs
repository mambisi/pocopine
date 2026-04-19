//! `emit` / `emit_from` — RFC-028.
//!
//! Vue-style one-line event emission from Rust handlers. Both
//! helpers fire a bubbling `CustomEvent` on the next microtask
//! so the caller's `&mut self` borrow releases before any
//! `pp-model` mirror listener re-enters the scope.
//!
//! Use `emit(name, detail)` inside a handler to dispatch from
//! the current directive element. Use `emit_from(&el, name,
//! detail)` when you need to dispatch from a specific element —
//! the teleport-backed overlays (Dialog, Popover, DropdownMenu)
//! must dispatch from their host tag because the teleported
//! content sits outside the host's bubbling path.
//!
//! Separate from [`crate::magics::dispatch_event`], which backs
//! the synchronous template-expression magic `$dispatch`. Same
//! underlying primitive; different ergonomic shape.

use serde::Serialize;
use wasm_bindgen::JsValue;
use web_sys::{CustomEvent, CustomEventInit, Element};

use crate::scope::current_el;
use crate::tick;

/// Serialize `detail` and fire a bubbling `CustomEvent(name)`
/// from the current directive element, deferred one microtask.
/// No-op when no element is current (called outside a handler /
/// lifecycle context) or when serialization fails.
pub fn emit<T: Serialize>(name: &str, detail: T) {
    let Some(el) = current_el() else { return };
    emit_from(&el, name, detail);
}

/// Emit from the host tag of the teleported subtree the current
/// element lives in. Walks `current_el` up through
/// `__pp_teleport_origin` pointers to find the original host,
/// then dispatches via [`emit_from`]. No-op outside a handler
/// context or when the current element isn't teleported.
///
/// This is the one-liner for overlay components (Dialog, Popover,
/// DropdownMenu) whose content has been moved to `<body>` but
/// whose `pp-model` listener still lives on the host tag —
/// bubbling from the teleport target wouldn't reach it.
pub fn emit_from_host<T: Serialize>(name: &str, detail: T) {
    let Some(el) = current_el() else { return };
    let Some(host) = crate::directives::teleport::host_of(&el) else {
        return;
    };
    emit_from(&host, name, detail);
}

/// Variant of [`emit`] that dispatches from an explicit element.
/// Needed by overlays whose emitting handlers run inside a
/// teleported subtree: bubbling from the teleport target would
/// miss the original host tag where `pp-model` listens.
pub fn emit_from<T: Serialize>(el: &Element, name: &str, detail: T) {
    let detail_js: JsValue = match serde_wasm_bindgen::to_value(&detail) {
        Ok(v) => v,
        Err(_) => return,
    };
    let el = el.clone();
    let name = name.to_string();
    tick::next(move || {
        let init = CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&detail_js);
        if let Ok(ev) = CustomEvent::new_with_event_init_dict(&name, &init) {
            let _ = el.dispatch_event(&ev);
        }
    });
}
