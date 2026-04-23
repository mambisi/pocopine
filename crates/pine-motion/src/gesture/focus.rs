//! Focus observer — fires `true` on focusin, `false` on focusout.
//!
//! Uses `focusin` / `focusout` rather than `focus` / `blur` so the
//! events bubble through shadow roots and custom elements. Matches
//! Motion's `focus()` behaviour.

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::{Element, FocusEvent};

use super::GestureHandle;

/// Attach a focus observer to `el`. The callback fires `true` on
/// `focusin`, `false` on `focusout`. Returns a [`GestureHandle`] —
/// drop it to stop observing.
pub fn focus<F>(el: &Element, cb: F) -> GestureHandle
where
    F: FnMut(bool, FocusEvent) + 'static,
{
    let mut handle = GestureHandle::new();
    let target = el.as_ref();
    let cb = Rc::new(RefCell::new(cb));

    let cb_in = cb.clone();
    handle.listen::<FocusEvent, _>(target, "focusin", move |ev| {
        if let Ok(mut f) = cb_in.try_borrow_mut() {
            f(true, ev);
        }
    });

    let cb_out = cb.clone();
    handle.listen::<FocusEvent, _>(target, "focusout", move |ev| {
        if let Ok(mut f) = cb_out.try_borrow_mut() {
            f(false, ev);
        }
    });

    handle
}
