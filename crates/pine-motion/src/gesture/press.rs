//! Press gesture — pointerdown on target, pointerup on window.
//!
//! Different from a plain click: fires on press *start* (pointerdown
//! of the primary pointer) with an optional end handler that
//! receives a `success` flag indicating whether the release landed
//! on the original target. That's the key Motion primitive for
//! `whileTap`-style styling and for tap-to-confirm flows — you can
//! revert the "pressed" state on pointerup regardless of where the
//! finger/mouse ends up, and then only apply the tap action when
//! `success == true`.
//!
//! Mirrors `motion-dom/src/gestures/press/index.ts` but without the
//! keyboard-accessibility layer (keyboard press lives on Pine's
//! Button primitive via its own handlers — layering it in here
//! would duplicate the dance).

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use web_sys::{Element, Node, PointerEvent};

use super::{GestureHandle, is_primary_pointer};

/// Callback fired on the matching pointerup / pointercancel. `success`
/// is `true` when the release lands on the original press target or
/// one of its descendants; `false` when it lands elsewhere (the user
/// dragged off) or the press was cancelled.
pub type PressEndHandler = dyn FnOnce(PointerEvent, bool);

/// Attach a press observer to `el`. The callback fires on each
/// qualifying `pointerdown` and may return a boxed end handler that
/// fires on the matching `pointerup` / `pointercancel`.
///
/// Returns a [`GestureHandle`] — drop it to stop observing.
pub fn press<F>(el: &Element, mut on_press_start: F) -> GestureHandle
where
    F: FnMut(PointerEvent) -> Option<Box<PressEndHandler>> + 'static,
{
    let mut handle = GestureHandle::new();
    let target_el = el.clone();
    let target = el.as_ref();

    // Shared slot for the pending end-handler. Only one press is
    // active at a time per target; a new pointerdown while a prior
    // press is still pending cancels the prior end handler.
    let pending: Rc<RefCell<Option<Box<PressEndHandler>>>> = Rc::new(RefCell::new(None));

    // Window listeners for pointerup / pointercancel — they stay
    // registered for the handle's lifetime so we don't pay the
    // add/remove cost on every press. They no-op when there's no
    // pending handler.
    {
        let pending_cb = pending.clone();
        let target_for_up = target_el.clone();
        handle.listen_on_window::<PointerEvent, _>("pointerup", move |ev| {
            if let Some(end) = pending_cb.borrow_mut().take() {
                let success = landed_on_target(&target_for_up, &ev);
                end(ev, success);
            }
        });
    }
    {
        let pending_cb = pending.clone();
        handle.listen_on_window::<PointerEvent, _>("pointercancel", move |ev| {
            if let Some(end) = pending_cb.borrow_mut().take() {
                end(ev, false);
            }
        });
    }

    // pointerdown on target.
    let pending_for_down = pending.clone();
    handle.listen::<PointerEvent, _>(target, "pointerdown", move |ev| {
        if !is_primary_pointer(&ev) {
            return;
        }
        // Cancel any prior pending handler — a second press before
        // the first resolves means the caller wants to treat the
        // first as abandoned.
        if let Some(prior) = pending_for_down.borrow_mut().take() {
            prior(ev.clone(), false);
        }
        let next = on_press_start(ev);
        *pending_for_down.borrow_mut() = next;
    });

    handle
}

/// Walk the pointerup event's target chain; return `true` if any
/// ancestor is the original press target. Matches Motion's
/// `isNodeOrChild` logic.
fn landed_on_target(press_target: &Element, event: &PointerEvent) -> bool {
    let Ok(up_target) = event
        .target()
        .ok_or(())
        .and_then(|t| t.dyn_into::<Node>().map_err(|_| ()))
    else {
        return false;
    };
    let press_node: &Node = press_target.as_ref();
    press_node.contains(Some(&up_target))
}
