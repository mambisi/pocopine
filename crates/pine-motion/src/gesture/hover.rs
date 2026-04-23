//! Hover observer — fires `true` on pointerenter, `false` on pointerleave.
//!
//! Uses `pointerenter` / `pointerleave` rather than `mouseenter`
//! because they bubble in a useful way for custom elements and fire
//! on touch where a stylus / pen is in hover state. Matches Motion's
//! `hover()` behaviour.

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::{Element, PointerEvent};

use super::GestureHandle;

/// Attach a hover observer to `el`. The callback fires `true` when a
/// primary pointer enters the element, `false` when it leaves.
/// Returns a [`GestureHandle`] — drop it to stop observing.
pub fn hover<F>(el: &Element, cb: F) -> GestureHandle
where
    F: FnMut(bool, PointerEvent) + 'static,
{
    let mut handle = GestureHandle::new();
    let target = el.as_ref();
    let cb = Rc::new(RefCell::new(cb));

    let cb_enter = cb.clone();
    handle.listen::<PointerEvent, _>(target, "pointerenter", move |ev| {
        if let Ok(mut f) = cb_enter.try_borrow_mut() {
            f(true, ev);
        }
    });

    let cb_leave = cb.clone();
    handle.listen::<PointerEvent, _>(target, "pointerleave", move |ev| {
        if let Ok(mut f) = cb_leave.try_borrow_mut() {
            f(false, ev);
        }
    });

    handle
}
