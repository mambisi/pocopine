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
//! events::on_scoped(&el, ev::contextmenu, move |e| { … });
//! ```
//!
//! Three things go away in one move:
//!
//! 1. **The string event name.** `ev::contextmenu` is a unit struct
//!    that carries both the wire name and the payload type via the
//!    [`DomEventName`] trait, so a typo turns into a compile error
//!    instead of a silently-dead listener.
//! 2. **The turbofish.** Inference flows through `N::Event` so the
//!    closure parameter type rarely needs to be spelled.
//! 3. **The leak.** `on_scoped` registers a teardown against the
//!    current scope's unmount list; the listener is removed
//!    automatically when the component goes away.
//!
//! Custom / non-standard event names use [`on_named`] /
//! [`on_named_scoped`], which still take a `&'static str`.

use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::EventTarget;

use crate::reactive::ScopeId;
use crate::scope::current_scope_id;

/// Marker trait implemented by each entry in [`ev`] — pairs a
/// compile-time event name with the web-sys payload type the browser
/// delivers for it.
///
/// Implement this on your own marker types if you need a typed
/// channel for a custom event the framework doesn't ship.
pub trait DomEventName: 'static {
    /// The web-sys event payload the closure receives.
    type Event: JsCast + 'static;
    /// On-the-wire event name (the same string `addEventListener`
    /// would take).
    const NAME: &'static str;
}

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

/// Install a typed listener on `target`. The marker `name` ties the
/// wire event name to the payload type the closure receives — so
/// `events::on(&el, ev::click, |e| …)` infers `e: MouseEvent`.
///
/// Returns a [`ListenerHandle`]; drop it (or call
/// [`ListenerHandle::cancel`]) to remove the listener. Use
/// [`on_scoped`] when you want the listener tied to the current
/// scope's lifetime.
///
/// ```ignore
/// let handle = events::on(&el, ev::click, move |e| {
///     web_sys::console::log_1(&format!("{}, {}", e.client_x(), e.client_y()).into());
/// });
/// ```
pub fn on<N, T, F>(target: &T, _name: N, handler: F) -> ListenerHandle
where
    N: DomEventName,
    T: AsRef<EventTarget>,
    F: FnMut(N::Event) + 'static,
{
    on_named::<N::Event, _, _>(target, N::NAME, handler)
}

/// Like [`on`], but binds the listener's lifetime to the current
/// scope — the listener is removed automatically when the component
/// unmounts. Returns nothing; storage is implicit.
///
/// Panics outside a handler / lifecycle context (same reason as
/// [`on_scope_unmount`]).
pub fn on_scoped<N, T, F>(target: &T, _name: N, handler: F)
where
    N: DomEventName,
    T: AsRef<EventTarget>,
    F: FnMut(N::Event) + 'static,
{
    on_named_scoped::<N::Event, _, _>(target, N::NAME, handler);
}

/// Escape hatch for events the [`ev`] catalog doesn't cover —
/// custom-element events, vendor-prefixed events, dynamic event
/// names. The payload type `E` is inferred from the closure
/// parameter; the wire name stays a `&'static str`.
///
/// ```ignore
/// events::on_named(&el, "my-app:select", move |e: web_sys::CustomEvent| { … });
/// ```
pub fn on_named<E, T, F>(target: &T, event: &'static str, mut handler: F) -> ListenerHandle
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

/// Scope-bound counterpart to [`on_named`].
pub fn on_named_scoped<E, T, F>(target: &T, event: &'static str, handler: F)
where
    E: JsCast + 'static,
    T: AsRef<EventTarget>,
    F: FnMut(E) + 'static,
{
    let handle = on_named::<E, T, F>(target, event, handler);
    on_scope_unmount(move || drop(handle));
}

/// Compile-time catalogue of standard DOM event names paired with
/// their web-sys payload types. Each item is a unit struct usable
/// with [`on`] / [`on_scoped`]:
///
/// ```ignore
/// events::on_scoped(&el, ev::keydown, move |e| {
///     if e.key() == "Escape" { /* … */ }
/// });
/// ```
///
/// Names are kept lowercase to match the on-the-wire form so a
/// typo in either place is a compile error.
pub mod ev {
    use super::DomEventName;
    use web_sys::{Event, FocusEvent, InputEvent, KeyboardEvent, MouseEvent, UiEvent};

    macro_rules! event_marker {
        ($($name:ident => ($lit:literal, $ty:ty)),* $(,)?) => {$(
            #[doc = concat!("`", $lit, "` — payload `", stringify!($ty), "`.")]
            #[allow(non_camel_case_types)]
            pub struct $name;
            impl DomEventName for $name {
                type Event = $ty;
                const NAME: &'static str = $lit;
            }
        )*};
    }

    event_marker! {
        // Mouse
        click          => ("click", MouseEvent),
        dblclick       => ("dblclick", MouseEvent),
        contextmenu    => ("contextmenu", MouseEvent),
        mousedown      => ("mousedown", MouseEvent),
        mouseup        => ("mouseup", MouseEvent),
        mousemove      => ("mousemove", MouseEvent),
        mouseenter     => ("mouseenter", MouseEvent),
        mouseleave     => ("mouseleave", MouseEvent),
        mouseover      => ("mouseover", MouseEvent),
        mouseout       => ("mouseout", MouseEvent),
        // Keyboard
        keydown        => ("keydown", KeyboardEvent),
        keyup          => ("keyup", KeyboardEvent),
        keypress       => ("keypress", KeyboardEvent),
        // Focus
        focus          => ("focus", FocusEvent),
        blur           => ("blur", FocusEvent),
        focusin        => ("focusin", FocusEvent),
        focusout       => ("focusout", FocusEvent),
        // Form / input
        input          => ("input", InputEvent),
        change         => ("change", Event),
        submit         => ("submit", Event),
        reset          => ("reset", Event),
        invalid        => ("invalid", Event),
        // Document / window lifecycle
        load           => ("load", Event),
        unload         => ("unload", Event),
        beforeunload   => ("beforeunload", Event),
        scroll         => ("scroll", Event),
        resize         => ("resize", UiEvent),
        // Selection / clipboard / drag
        select         => ("select", Event),
        copy           => ("copy", Event),
        cut            => ("cut", Event),
        paste          => ("paste", Event),
    }
}
