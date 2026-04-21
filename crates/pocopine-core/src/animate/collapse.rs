//! Auto-height expand / collapse helper.
//!
//! CSS can't transition `height: auto` natively. This helper
//! measures `scrollHeight` at animation start, runs a Web Animation
//! from `0px` ↔ the measured value, then clears the inline height so
//! the element returns to intrinsic sizing.
//!
//! Called by the `transition = "collapse"` macro path (Collapsible,
//! Accordion, Tree child wrapper) — authors don't hit this module
//! directly unless they're hand-rolling.

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement};

use super::waapi::{animate, AnimateOptions, Keyframe};

/// Options for a collapse transition.
#[derive(Clone, Debug)]
pub struct CollapseOptions {
    /// Duration in ms. Default 200.
    pub duration_ms: f64,
    /// CSS easing string.
    pub easing: &'static str,
}

impl Default for CollapseOptions {
    fn default() -> Self {
        Self {
            duration_ms: 200.0,
            easing: "cubic-bezier(0.2, 0, 0, 1)",
        }
    }
}

/// Animate `el`'s height between 0 and its natural `scrollHeight`
/// in the direction of `open`.
///
/// - `open = true` expands: clear inline height (so we can measure
///   the real intrinsic value), snapshot that, re-clamp to `0`, then
///   animate to the measured height. On finish, clear inline height
///   so the element tracks its content normally.
/// - `open = false` collapses: measure current height, set it
///   inline, animate to 0, leave `0px` committed via `fill:
///   "forwards"` (keeps the slot closed visually).
///
/// `overflow: hidden` is applied for the duration so children don't
/// bleed through a zero-height container.
pub fn collapse_to(el: &Element, open: bool, opts: CollapseOptions) {
    let Ok(html) = el.clone().dyn_into::<HtmlElement>() else {
        return;
    };

    if open {
        // Measure natural height. `scrollHeight` works even when the
        // element is currently height-clamped to 0 — it reports
        // content size.
        let style = html.style();
        let _ = style.set_property("overflow", "hidden");
        let target = el.scroll_height() as f64;

        animate(
            el,
            &[
                Keyframe::from_iter([("height", "0px")]),
                Keyframe::from_iter([("height", format!("{}px", target))]),
            ],
            AnimateOptions {
                duration_ms: opts.duration_ms,
                easing: opts.easing,
                delay_ms: 0.0,
                // None so height clears to intrinsic when done —
                // the element can then resize freely with content.
                fill: "none",
            },
        )
        .on_finish(move || {
            // Clear overflow + inline height after the animation
            // settles. `html` was captured by move into the
            // closure.
            let style = html.style();
            let _ = style.remove_property("overflow");
            let _ = style.remove_property("height");
        });
    } else {
        // Measure current rendered height before clamping so we
        // have a well-defined "from" value even if the box has
        // padding / borders affecting client height.
        let current = el.get_bounding_client_rect().height();
        let style = html.style();
        let _ = style.set_property("overflow", "hidden");

        animate(
            el,
            &[
                Keyframe::from_iter([("height", format!("{}px", current))]),
                Keyframe::from_iter([("height", "0px")]),
            ],
            AnimateOptions {
                duration_ms: opts.duration_ms,
                easing: opts.easing,
                delay_ms: 0.0,
                // `forwards` — keep the 0px height applied once the
                // animation finishes so the slot stays closed.
                fill: "forwards",
            },
        );
    }
}
