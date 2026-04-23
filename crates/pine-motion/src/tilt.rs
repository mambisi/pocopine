//! 3D hover tilt — Motion-style pointer-driven card perspective.
//!
//! Attach [`tilt`] to a card/container element. The helper rotates
//! the element around its center as the pointer moves and stamps
//! `data-pm-tilt="active"` while hovered so child "depth" layers can
//! respond in CSS.

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, PointerEvent};

use crate::gesture::GestureHandle;

/// Options for [`tilt`].
#[derive(Clone, Copy, Debug)]
pub struct TiltConfig {
    /// Pointer delta divisor. Lower values produce a stronger tilt.
    /// The common React example uses `25.0`.
    pub divisor: f64,
    /// Transition used when entering and resetting.
    pub transition_ms: f64,
}

impl Default for TiltConfig {
    fn default() -> Self {
        Self {
            divisor: 25.0,
            transition_ms: 200.0,
        }
    }
}

/// Attach pointer-driven 3D tilt to `el`.
///
/// CSS still owns perspective and child layer depth. Typical setup:
///
/// ```css
/// .scene { perspective: 1000px; }
/// .card { transform-style: preserve-3d; }
/// .card[data-pm-tilt="active"] .title {
///   transform: translateZ(48px);
/// }
/// ```
pub fn tilt(el: &Element, cfg: TiltConfig) -> GestureHandle {
    let mut handle = GestureHandle::new();
    let html = el.clone().dyn_into::<HtmlElement>().ok();

    {
        let target = el.clone();
        let el = el.clone();
        let html = html.clone();
        handle.listen::<PointerEvent, _>(target.as_ref(), "pointerenter", move |_| {
            let _ = el.set_attribute("data-pm-tilt", "active");
            if let Some(h) = &html {
                let style = h.style();
                let _ = style.set_property("transform-style", "preserve-3d");
                let _ = style.set_property(
                    "transition",
                    &format!("transform {}ms linear", cfg.transition_ms),
                );
            }
        });
    }

    {
        let html = html.clone();
        handle.listen::<PointerEvent, _>(el.as_ref(), "pointermove", move |ev| {
            let Some(h) = &html else { return };
            let rect = h.get_bounding_client_rect();
            let (rotate_y, rotate_x) = tilt_degrees(
                ev.client_x() as f64,
                ev.client_y() as f64,
                rect.left(),
                rect.top(),
                rect.width(),
                rect.height(),
                cfg.divisor,
            );
            let style = h.style();
            let _ = style.set_property("transition", "transform 40ms linear");
            let _ = style.set_property(
                "transform",
                &format!("rotateY({rotate_y}deg) rotateX({rotate_x}deg)"),
            );
        });
    }

    {
        let target = el.clone();
        let el = el.clone();
        handle.listen::<PointerEvent, _>(target.as_ref(), "pointerleave", move |_| {
            let _ = el.remove_attribute("data-pm-tilt");
            if let Some(h) = &html {
                let style = h.style();
                let _ = style.set_property(
                    "transition",
                    &format!("transform {}ms linear", cfg.transition_ms),
                );
                let _ = style.set_property("transform", "rotateY(0deg) rotateX(0deg)");
            }
        });
    }

    handle
}

fn tilt_degrees(
    client_x: f64,
    client_y: f64,
    left: f64,
    top: f64,
    width: f64,
    height: f64,
    divisor: f64,
) -> (f64, f64) {
    let divisor = divisor.max(1.0);
    let rotate_y = (client_x - left - width / 2.0) / divisor;
    let rotate_x = (client_y - top - height / 2.0) / divisor;
    (rotate_y, rotate_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_has_no_tilt() {
        assert_eq!(
            tilt_degrees(150.0, 100.0, 50.0, 50.0, 200.0, 100.0, 25.0),
            (0.0, 0.0)
        );
    }

    #[test]
    fn pointer_offset_maps_to_degrees() {
        assert_eq!(
            tilt_degrees(200.0, 125.0, 50.0, 50.0, 200.0, 100.0, 25.0),
            (2.0, 1.0)
        );
    }
}
