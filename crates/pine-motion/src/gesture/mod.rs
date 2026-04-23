//! Gesture observers — `hover`, `press`, `focus`.
//!
//! Each entry point attaches event listeners to an element and
//! returns a [`GestureHandle`] that removes them when dropped. Hold
//! the handle for as long as you want the gesture active:
//!
//! ```ignore
//! use pine_motion::gesture::{hover, press};
//!
//! // Hover — fires true on pointerenter, false on pointerleave.
//! let _hover = hover(&el, |entered| {
//!     // ...
//! });
//!
//! // Press — returns Option<end handler>; end fires with success=
//! // true when the pointerup lands on the original target.
//! let _press = press(&el, |_start_event| {
//!     Some(Box::new(|_end_event, success| {
//!         if success {
//!             // activate
//!         }
//!     }))
//! });
//! ```
//!
//! The handle is cheap to hold (one allocation). Dropping it before
//! the corresponding lifecycle event just means you miss the event —
//! no cleanup bookkeeping to worry about.

pub mod focus;
pub mod hover;
pub mod pan;
pub mod press;

pub use focus::focus;
pub use hover::hover;
pub use pan::{pan, PanConfig, PanEvent};
pub use press::{press, PressEndHandler};

use wasm_bindgen::JsCast;
use wasm_bindgen::{closure::Closure, JsValue};
use web_sys::EventTarget;

/// Holds a set of event-listener closures alive and removes them on
/// drop. Created by the gesture entry points in this module; authors
/// just keep the handle for as long as they want the gesture active.
pub struct GestureHandle {
    cleanup: Vec<Box<dyn FnOnce()>>,
}

impl GestureHandle {
    pub(crate) fn new() -> Self {
        Self {
            cleanup: Vec::new(),
        }
    }

    /// Register a listener and record how to remove it. The
    /// `Closure` is moved into the handle so it lives as long as the
    /// handle does — without this, the closure would drop
    /// immediately after `addEventListener` returned and the
    /// callback would be a dangling pointer.
    pub(crate) fn listen<T, F>(
        &mut self,
        target: &EventTarget,
        event: &'static str,
        handler: F,
    ) where
        T: wasm_bindgen::convert::FromWasmAbi + 'static,
        F: FnMut(T) + 'static,
    {
        let closure = Closure::<dyn FnMut(T)>::new(handler);
        let func = closure.as_ref().unchecked_ref::<js_sys::Function>().clone();
        let _ = target.add_event_listener_with_callback(event, &func);

        let target_cloned = target.clone();
        let event_name = event;
        self.cleanup.push(Box::new(move || {
            let _ = target_cloned.remove_event_listener_with_callback(event_name, &func);
            // Drop the closure explicitly here so the JS-side
            // function reference becomes dead at the same moment the
            // listener is removed.
            drop(closure);
        }));
    }

    /// Attach a window-level listener. Symmetric cleanup.
    pub(crate) fn listen_on_window<T, F>(&mut self, event: &'static str, handler: F)
    where
        T: wasm_bindgen::convert::FromWasmAbi + 'static,
        F: FnMut(T) + 'static,
    {
        let Some(window) = web_sys::window() else { return };
        let target: &EventTarget = window.as_ref();
        self.listen(target, event, handler);
    }
}

impl Drop for GestureHandle {
    fn drop(&mut self) {
        for cb in self.cleanup.drain(..) {
            cb();
        }
    }
}

/// Unused helper — exported for future internal extensions.
#[allow(dead_code)]
pub(crate) fn is_primary_pointer(event: &web_sys::PointerEvent) -> bool {
    // Only left mouse button (button == 0) or primary touch. Right-
    // click and middle-click filter out here so a `press` gesture
    // doesn't fire on menu-open.
    let is_primary = event.is_primary();
    let btn = event.button();
    let _ = JsValue::NULL;
    is_primary && btn == 0
}
