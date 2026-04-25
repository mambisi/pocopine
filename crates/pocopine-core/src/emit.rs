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

use crate::refs;
use crate::scope::{current_el, current_scope_id};
use crate::tick;

/// Typed event surface — RFC 056 §6.8.
///
/// Implemented automatically by `#[derive(Emit)]`. Each variant
/// becomes one event:
///
/// * `event_name(&self)` — the on-the-wire event name (kebab-case of
///   the variant ident: `Confirm` → `"confirm"`,
///   `OpenChange` → `"open-change"`).
/// * `to_detail(&self)` — the variant's fields serialized into
///   `CustomEvent.detail` (`{}` for unit variants, the struct fields
///   for struct variants, the tuple for tuple variants).
///
/// `Emit` is what [`emit_event`] / [`emit_event_from`] dispatch on.
/// Authors should not implement this by hand — derive it instead so
/// the variant→name mapping stays consistent across the codebase.
pub trait Emit {
    /// The event name dispatched on the DOM.
    fn event_name(&self) -> &'static str;
    /// Serialize the variant's payload into the `CustomEvent.detail`
    /// slot. Unit variants serialize as `null`; struct variants
    /// serialize as their named fields; tuple variants serialize as
    /// an array of positional values.
    fn to_detail(&self) -> Result<JsValue, serde_wasm_bindgen::Error>;
}

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

/// Emit `pp:update:model` from the current scope's `pp-ref="root"`
/// element with `value` as detail. No-op outside a handler context
/// or when no `root` ref is registered. Replaces the per-Root
/// `emit_from_self` / `emit_value_update` helper every compound
/// root used to duplicate.
pub fn emit_model<T: Serialize>(value: T) {
    let Some(scope) = current_scope_id() else {
        return;
    };
    let root_el = crate::model_runtime::emit_target(scope).or_else(|| refs::get_on(scope, "root"));
    let Some(root_el) = root_el else { return };
    emit_from(&root_el, "pp:update:model", value);
}

/// Emit `pp:update:<field>` from the current scope's
/// `pp-ref="root"` element with `value` as detail. Use this for
/// named `pp-model:<field>` channels so multiple model bindings on
/// one child don't collide on the shared `pp:update:model` event.
pub fn emit_model_field<T: Serialize>(field: &str, value: T) {
    let Some(scope) = current_scope_id() else {
        return;
    };
    let root_el = crate::model_runtime::emit_target(scope).or_else(|| refs::get_on(scope, "root"));
    let Some(root_el) = root_el else { return };
    emit_from(&root_el, &format!("pp:update:{field}"), value);
}

/// Typed counterpart to [`emit`] — fires the event whose name is
/// derived from `event`'s variant via the [`Emit`] trait. RFC 056
/// §6.8.
///
/// ```ignore
/// #[derive(Emit)]
/// pub enum DialogEvent {
///     Close,
///     Confirm { value: String },
/// }
///
/// emit_event(DialogEvent::Close);
/// emit_event(DialogEvent::Confirm { value });
/// ```
pub fn emit_event<E: Emit>(event: E) {
    let Some(el) = current_el() else { return };
    emit_event_from(&el, event);
}

/// Typed variant of [`emit_event`] that dispatches from an explicit
/// element — needed for the same teleport reasons as [`emit_from`].
pub fn emit_event_from<E: Emit>(el: &Element, event: E) {
    let name = event.event_name();
    let detail_js = match event.to_detail() {
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

/// Stable alias for [`emit`]. RFC 056 §6.8 reserves the bare `emit`
/// name for the typed surface (`emit(DialogEvent::Close)`); call
/// sites that want to keep the dynamic, stringly form forward-
/// compatibly should reach for `emit_raw` instead.
pub fn emit_raw<T: Serialize>(name: &str, detail: T) {
    emit(name, detail);
}

/// Stable alias for [`emit_from`]. See [`emit_raw`].
pub fn emit_raw_from<T: Serialize>(el: &Element, name: &str, detail: T) {
    emit_from(el, name, detail);
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

/// Synchronous, cancelable counterpart to [`emit`]. Fires the
/// event immediately (no tick::next defer) so the caller can
/// read back whether a listener called `preventDefault`.
/// Returns `true` when the event was prevented.
///
/// Trade-off vs [`emit`]: because this fires synchronously, any
/// listener calling back into the caller's scope re-enters its
/// active borrow. Safe for fire-and-observe patterns (menu item
/// asks "can I close?") but not for pp-model-style mirror
/// flows — use the deferred [`emit`] / [`emit_from`] there.
pub fn emit_cancelable<T: Serialize>(name: &str, detail: T) -> bool {
    let Some(el) = current_el() else { return false };
    emit_cancelable_from(&el, name, detail)
}

/// Variant of [`emit_cancelable`] that dispatches from an
/// explicit element. Returns `true` when `preventDefault` was
/// called.
pub fn emit_cancelable_from<T: Serialize>(el: &Element, name: &str, detail: T) -> bool {
    let Ok(detail_js) = serde_wasm_bindgen::to_value(&detail) else {
        return false;
    };
    let init = CustomEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    init.set_detail(&detail_js);
    let Ok(ev) = CustomEvent::new_with_event_init_dict(name, &init) else {
        return false;
    };
    let _ = el.dispatch_event(&ev);
    ev.default_prevented()
}
