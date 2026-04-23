//! Scroll observers — scroll progress + in-view triggers.
//!
//! Two entry points:
//!
//! * [`scroll_progress`] — fires with `progress: f64` (0..=1) on each
//!   page-scroll event. Useful for scroll-linked animations where a
//!   property maps to the page's scroll position.
//! * [`on_view`] — IntersectionObserver-backed trigger; fires `true`
//!   when the element enters the viewport and `false` when it
//!   leaves. Basis for "animate on scroll into view" flows.
//!
//! `ScrollTimeline` (the native CSS API Motion uses on modern
//! browsers) isn't wrapped yet — those declarative scroll-linked
//! animations are best authored directly through CSS
//! `animation-timeline: scroll()`. This module fills the JS-triggered
//! gap where authors need a callback, not a style directive.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, IntersectionObserver, IntersectionObserverEntry, IntersectionObserverInit};

use crate::gesture::GestureHandle;

/// Attach a scroll-progress observer to the page. The callback fires
/// on every `scroll` event with a 0..=1 value reflecting
/// `scrollY / (scrollHeight - innerHeight)`. Edge cases (page isn't
/// scrollable) resolve to `0.0`.
pub fn scroll_progress<F>(cb: F) -> GestureHandle
where
    F: FnMut(f64) + 'static,
{
    let mut handle = GestureHandle::new();
    let cb = Rc::new(RefCell::new(cb));
    let cb_scroll = cb.clone();
    handle.listen_on_window::<web_sys::Event, _>("scroll", move |_| {
        if let Some(window) = web_sys::window() {
            let scroll_y = window.scroll_y().unwrap_or(0.0);
            let inner_h = window
                .inner_height()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let scroll_h = window
                .document()
                .and_then(|d| d.document_element())
                .map(|e| e.scroll_height() as f64)
                .unwrap_or(0.0);
            let scrollable = (scroll_h - inner_h).max(1.0);
            let progress = (scroll_y / scrollable).clamp(0.0, 1.0);
            if let Ok(mut f) = cb_scroll.try_borrow_mut() {
                f(progress);
            }
        }
    });
    handle
}

/// Options for [`on_view`].
#[derive(Clone, Copy, Debug)]
pub struct ViewConfig {
    /// Fraction of the element that must be visible before "in view"
    /// fires. `0.0` = any part; `0.5` = half; `1.0` = fully visible.
    /// Default `0.1` (matches Motion's default).
    pub threshold: f64,
    /// Once the element enters view, stop observing (one-shot). Good
    /// for "animate in on scroll" flows where you don't want a
    /// repeat when the user scrolls past and back.
    pub once: bool,
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            threshold: 0.1,
            once: false,
        }
    }
}

/// Attach an IntersectionObserver to `el`. The callback fires `true`
/// on enter-view, `false` on leave-view. With `once = true`, the
/// observer disconnects after the first `true`.
pub fn on_view<F>(el: &Element, cfg: ViewConfig, cb: F) -> ScrollHandle
where
    F: FnMut(bool) + 'static,
{
    let cb = Rc::new(RefCell::new(cb));

    let observer_slot: Rc<RefCell<Option<IntersectionObserver>>> = Rc::new(RefCell::new(None));
    let observer_slot_for_cb = observer_slot.clone();
    let cb_inner = cb.clone();

    let closure = Closure::<dyn FnMut(js_sys::Array, IntersectionObserver)>::new(
        move |entries: js_sys::Array, _obs: IntersectionObserver| {
            let mut any_triggered = false;
            for i in 0..entries.length() {
                if let Ok(entry) = entries.get(i).dyn_into::<IntersectionObserverEntry>() {
                    let in_view = entry.is_intersecting();
                    if let Ok(mut f) = cb_inner.try_borrow_mut() {
                        f(in_view);
                    }
                    if in_view {
                        any_triggered = true;
                    }
                }
            }
            if cfg.once && any_triggered {
                if let Some(obs) = observer_slot_for_cb.borrow_mut().take() {
                    obs.disconnect();
                }
            }
        },
    );

    let opts = IntersectionObserverInit::new();
    opts.set_threshold(&JsValue::from_f64(cfg.threshold));

    let observer =
        match IntersectionObserver::new_with_options(closure.as_ref().unchecked_ref(), &opts) {
            Ok(o) => o,
            Err(_) => {
                return ScrollHandle {
                    _closure: Some(closure),
                    observer: None,
                };
            }
        };
    observer.observe(el);
    *observer_slot.borrow_mut() = Some(observer.clone());

    ScrollHandle {
        _closure: Some(closure),
        observer: Some(observer),
    }
}

/// Handle for [`on_view`]; drop to disconnect the underlying
/// IntersectionObserver.
pub struct ScrollHandle {
    _closure: Option<Closure<dyn FnMut(js_sys::Array, IntersectionObserver)>>,
    observer: Option<IntersectionObserver>,
}

impl Drop for ScrollHandle {
    fn drop(&mut self) {
        if let Some(obs) = self.observer.take() {
            obs.disconnect();
        }
    }
}
