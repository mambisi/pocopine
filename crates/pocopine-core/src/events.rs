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
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{CustomEvent, EventTarget};

use crate::emit::Emit;
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

/// Opaque handle to one or more installed listeners. Drop the value
/// or call [`ListenerHandle::cancel`] to remove all of them; otherwise
/// they stay alive for as long as the handle does.
///
/// Most call sites end up with a single underlying listener, but
/// [`on_emit`] stores one entry per [`Emit`] variant name, so the
/// handle is built around a `Vec` rather than a single closure.
type Listener = (&'static str, Closure<dyn FnMut(JsValue)>);

pub struct ListenerHandle {
    target: EventTarget,
    listeners: Vec<Listener>,
}

impl ListenerHandle {
    /// Remove every listener this handle owns. Equivalent to dropping
    /// the handle.
    pub fn cancel(mut self) {
        self.cancel_in_place();
    }

    fn cancel_in_place(&mut self) {
        for (name, closure) in self.listeners.drain(..) {
            let _ = self
                .target
                .remove_event_listener_with_callback(name, closure.as_ref().unchecked_ref());
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
    on_scope_unmount_for(scope, f);
}

/// Run `f` when `scope` unmounts. If the scope has already gone away,
/// the cleanup runs immediately so callers cannot accidentally leak a
/// resource by registering too late.
pub fn on_scope_unmount_for(scope: ScopeId, f: impl FnOnce() + 'static) {
    if crate::scope::Scope::find(scope).is_none() {
        f();
        return;
    }
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn on_scope_unmount_for_runs_immediately_when_scope_is_gone() {
        let ran = Rc::new(Cell::new(false));
        let ran_for_cleanup = ran.clone();

        on_scope_unmount_for(ScopeId(u64::MAX), move || ran_for_cleanup.set(true));

        assert!(ran.get());
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
        listeners: vec![(event, closure)],
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

/// Listen for every variant of a typed [`Emit`] enum on `target`.
/// One DOM listener is registered per variant name; every fire is
/// reconstructed back into the original enum via [`Emit::from_event`]
/// before the handler runs.
///
/// The receiving counterpart to `emit_event(MyEvent::…)`. Pairs the
/// emit-side typed surface with a receive-side typed surface so a
/// component author can write:
///
/// ```ignore
/// #[derive(Emit)]
/// enum DialogEvent {
///     Close,
///     Confirm { value: String },
/// }
///
/// events::on_emit_scoped::<DialogEvent, _, _>(&host, move |e| match e {
///     DialogEvent::Close            => { /* … */ }
///     DialogEvent::Confirm { value } => { /* … */ }
/// });
/// ```
///
/// All variant listeners share a single `Rc<RefCell<F>>` so the
/// closure runs against consistent state across variants.
pub fn on_emit<E, T, F>(target: &T, handler: F) -> ListenerHandle
where
    E: Emit + 'static,
    T: AsRef<EventTarget>,
    F: FnMut(E) + 'static,
{
    let target = target.as_ref();
    let handler = Rc::new(RefCell::new(handler));
    let mut listeners = Vec::with_capacity(E::event_names().len());
    for &name in E::event_names() {
        let handler = handler.clone();
        let closure: Closure<dyn FnMut(JsValue)> = Closure::wrap(Box::new(move |raw: JsValue| {
            // Custom events fire as CustomEvent; if the browser
            // delivered something else (the bare base `Event`), the
            // detail field is undefined and from_event will fail
            // gracefully.
            let detail = raw
                .dyn_ref::<CustomEvent>()
                .map(|ce| ce.detail())
                .unwrap_or(JsValue::UNDEFINED);
            if let Some(typed) = E::from_event(name, detail) {
                (handler.borrow_mut())(typed);
            }
        }));
        let _ = target.add_event_listener_with_callback(name, closure.as_ref().unchecked_ref());
        listeners.push((name, closure));
    }
    ListenerHandle {
        target: target.clone(),
        listeners,
    }
}

/// Scope-bound counterpart to [`on_emit`] — the listeners are
/// removed automatically when the current scope unmounts.
pub fn on_emit_scoped<E, T, F>(target: &T, handler: F)
where
    E: Emit + 'static,
    T: AsRef<EventTarget>,
    F: FnMut(E) + 'static,
{
    let handle = on_emit::<E, T, F>(target, handler);
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
    use web_sys::{
        AnimationEvent, ClipboardEvent, CompositionEvent, DragEvent, ErrorEvent, Event, FocusEvent,
        HashChangeEvent, InputEvent, KeyboardEvent, MessageEvent, MouseEvent, PageTransitionEvent,
        PointerEvent, PopStateEvent, ProgressEvent, StorageEvent, SubmitEvent, TouchEvent,
        TransitionEvent, UiEvent, WheelEvent,
    };

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
        click             => ("click", MouseEvent),
        dblclick          => ("dblclick", MouseEvent),
        contextmenu       => ("contextmenu", MouseEvent),
        auxclick          => ("auxclick", MouseEvent),
        mousedown         => ("mousedown", MouseEvent),
        mouseup           => ("mouseup", MouseEvent),
        mousemove         => ("mousemove", MouseEvent),
        mouseenter        => ("mouseenter", MouseEvent),
        mouseleave        => ("mouseleave", MouseEvent),
        mouseover         => ("mouseover", MouseEvent),
        mouseout          => ("mouseout", MouseEvent),
        wheel             => ("wheel", WheelEvent),
        // Pointer (modern unified mouse + touch + pen)
        pointerdown       => ("pointerdown", PointerEvent),
        pointerup         => ("pointerup", PointerEvent),
        pointermove       => ("pointermove", PointerEvent),
        pointerenter      => ("pointerenter", PointerEvent),
        pointerleave      => ("pointerleave", PointerEvent),
        pointerover       => ("pointerover", PointerEvent),
        pointerout        => ("pointerout", PointerEvent),
        pointercancel     => ("pointercancel", PointerEvent),
        gotpointercapture => ("gotpointercapture", PointerEvent),
        lostpointercapture => ("lostpointercapture", PointerEvent),
        // Touch
        touchstart        => ("touchstart", TouchEvent),
        touchend          => ("touchend", TouchEvent),
        touchmove         => ("touchmove", TouchEvent),
        touchcancel       => ("touchcancel", TouchEvent),
        // Drag and drop
        drag              => ("drag", DragEvent),
        dragstart         => ("dragstart", DragEvent),
        dragend           => ("dragend", DragEvent),
        dragenter         => ("dragenter", DragEvent),
        dragleave         => ("dragleave", DragEvent),
        dragover          => ("dragover", DragEvent),
        drop              => ("drop", DragEvent),
        // Keyboard
        keydown           => ("keydown", KeyboardEvent),
        keyup             => ("keyup", KeyboardEvent),
        keypress          => ("keypress", KeyboardEvent),
        // IME composition
        compositionstart  => ("compositionstart", CompositionEvent),
        compositionupdate => ("compositionupdate", CompositionEvent),
        compositionend    => ("compositionend", CompositionEvent),
        // Focus
        focus             => ("focus", FocusEvent),
        blur              => ("blur", FocusEvent),
        focusin           => ("focusin", FocusEvent),
        focusout          => ("focusout", FocusEvent),
        // Form / input
        input             => ("input", InputEvent),
        change            => ("change", Event),
        submit            => ("submit", SubmitEvent),
        reset             => ("reset", Event),
        invalid           => ("invalid", Event),
        search            => ("search", Event),
        // Selection / clipboard
        select            => ("select", Event),
        copy              => ("copy", ClipboardEvent),
        cut               => ("cut", ClipboardEvent),
        paste             => ("paste", ClipboardEvent),
        // Animation / transition
        animationstart    => ("animationstart", AnimationEvent),
        animationend      => ("animationend", AnimationEvent),
        animationiteration => ("animationiteration", AnimationEvent),
        animationcancel   => ("animationcancel", AnimationEvent),
        transitionstart   => ("transitionstart", TransitionEvent),
        transitionrun     => ("transitionrun", TransitionEvent),
        transitionend     => ("transitionend", TransitionEvent),
        transitioncancel  => ("transitioncancel", TransitionEvent),
        // Document / window lifecycle
        load              => ("load", Event),
        unload            => ("unload", Event),
        beforeunload      => ("beforeunload", Event),
        scroll            => ("scroll", Event),
        scrollend         => ("scrollend", Event),
        resize            => ("resize", UiEvent),
        visibilitychange  => ("visibilitychange", Event),
        fullscreenchange  => ("fullscreenchange", Event),
        fullscreenerror   => ("fullscreenerror", Event),
        // Media (load, error, progress – the typed ones)
        error             => ("error", ErrorEvent),
        progress          => ("progress", ProgressEvent),
        loadstart         => ("loadstart", ProgressEvent),
        loadend           => ("loadend", ProgressEvent),
        // Routing / storage / connectivity
        hashchange        => ("hashchange", HashChangeEvent),
        popstate          => ("popstate", PopStateEvent),
        pageshow          => ("pageshow", PageTransitionEvent),
        pagehide          => ("pagehide", PageTransitionEvent),
        storage           => ("storage", StorageEvent),
        online            => ("online", Event),
        offline           => ("offline", Event),
        // postMessage. MediaQueryList fires its own "change" event
        // with a different payload — reach for `on_named` if you
        // want it typed as MediaQueryListEvent.
        message           => ("message", MessageEvent),
        // HTML5 details / dialog
        toggle            => ("toggle", Event),
        cancel            => ("cancel", Event),
        close             => ("close", Event),
        // Slot / slot change
        slotchange        => ("slotchange", Event),
    }
}
