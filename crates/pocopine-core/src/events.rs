//! Typed DOM event listener helpers — hides `Closure::wrap` +
//! `addEventListener` + `forget` behind a small, type-safe API.
//!
//! The ergonomic problem this solves: every primitive that wires a
//! native listener used to write the same six-line dance —
//!
//! ```ignore
//! let closure = Closure::wrap(Box::new(move |ev: MouseEvent| { … })
//!     as Box<dyn FnMut(MouseEvent)>);
//! let target: &EventTarget = el.as_ref();
//! let _ = target.add_event_listener_with_callback(
//!     "contextmenu", closure.as_ref().unchecked_ref(),
//! );
//! closure.forget();
//! ```
//!
//! `closure.forget()` leaks the listener for the lifetime of the
//! page — common in primitive code today. The new surface is:
//!
//! ```ignore
//! events::on_scoped(&el, "contextmenu", move |ev: MouseEvent| { … });
//! ```
//!
//! `on_scoped` returns nothing — the listener is removed
//! automatically when the current scope unmounts. The event type is
//! inferred from the closure parameter, so no turbofish is needed.
//! The non-scoped [`on`] flavour returns a [`ListenerHandle`] for
//! cases that want manual lifetime control.

use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::EventTarget;

use crate::reactive::ScopeId;
use crate::scope::current_scope_id;

/// Opaque handle to an installed listener. Drop the value or call
/// [`ListenerHandle::cancel`] to remove the listener; otherwise it
/// stays alive for as long as the handle does.
pub struct ListenerHandle {
    target: EventTarget,
    event: &'static str,
    closure: Option<Closure<dyn FnMut(JsValue)>>,
}

impl ListenerHandle {
    /// Remove the listener now. Equivalent to dropping the handle.
    pub fn cancel(mut self) {
        self.cancel_in_place();
    }

    fn cancel_in_place(&mut self) {
        if let Some(closure) = self.closure.take() {
            let _ = self
                .target
                .remove_event_listener_with_callback(self.event, closure.as_ref().unchecked_ref());
        }
    }
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        self.cancel_in_place();
    }
}

type UnmountCb = Box<dyn FnOnce()>;
type UnmountCbs = HashMap<ScopeId, Vec<UnmountCb>>;

thread_local! {
    /// Per-scope unmount callbacks. Drained by [`clear_scope`] when
    /// the scope's owning component is torn down.
    static UNMOUNT_CBS: RefCell<UnmountCbs> = RefCell::new(HashMap::new());
}

/// Run `f` when the *current* scope unmounts. Panics outside a
/// handler / lifecycle context — without a current scope there is
/// nothing to bind the cleanup to.
///
/// Lower-level than [`on_scoped`]; reach for this when you need the
/// hook for a non-listener resource (a `setInterval`, a
/// `MutationObserver`, …).
pub fn on_scope_unmount(f: impl FnOnce() + 'static) {
    let scope =
        current_scope_id().expect("on_scope_unmount called outside a handler / lifecycle context");
    UNMOUNT_CBS.with(|m| {
        m.borrow_mut().entry(scope).or_default().push(Box::new(f));
    });
}

/// Drain and run a scope's unmount callbacks. Called from
/// `Scope::remove`.
pub fn clear_scope(scope: ScopeId) {
    let cbs = UNMOUNT_CBS.with(|m| m.borrow_mut().remove(&scope).unwrap_or_default());
    for cb in cbs {
        cb();
    }
}

/// Install a typed listener on `target`. The closure is wrapped for
/// you and a [`ListenerHandle`] is returned; drop the handle (or
/// call [`ListenerHandle::cancel`]) to remove the listener.
///
/// The event type `E` is inferred from the closure parameter:
///
/// ```ignore
/// let handle = events::on(&el, "click", move |ev: MouseEvent| {
///     web_sys::console::log_1(&format!("{}, {}", ev.client_x(), ev.client_y()).into());
/// });
/// ```
///
/// Any web-sys event type works — `MouseEvent`, `KeyboardEvent`,
/// `PointerEvent`, `Event` for the bare base, etc.
pub fn on<E, T, F>(target: &T, event: &'static str, mut handler: F) -> ListenerHandle
where
    E: JsCast + 'static,
    T: AsRef<EventTarget>,
    F: FnMut(E) + 'static,
{
    let closure: Closure<dyn FnMut(JsValue)> = Closure::wrap(Box::new(move |raw: JsValue| {
        if let Ok(ev) = raw.dyn_into::<E>() {
            handler(ev);
        }
    }));
    let target = target.as_ref();
    let _ = target.add_event_listener_with_callback(event, closure.as_ref().unchecked_ref());
    ListenerHandle {
        target: target.clone(),
        event,
        closure: Some(closure),
    }
}

/// Like [`on`], but binds the listener's lifetime to the current
/// scope — the listener is removed automatically when the component
/// unmounts. Returns nothing; storage is implicit.
///
/// Panics outside a handler / lifecycle context (same reason as
/// [`on_scope_unmount`]).
pub fn on_scoped<E, T, F>(target: &T, event: &'static str, handler: F)
where
    E: JsCast + 'static,
    T: AsRef<EventTarget>,
    F: FnMut(E) + 'static,
{
    let handle = on::<E, T, F>(target, event, handler);
    on_scope_unmount(move || drop(handle));
}
