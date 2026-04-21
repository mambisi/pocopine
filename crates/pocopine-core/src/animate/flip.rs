//! FLIP — _First, Last, Invert, Play_. Layout-animation helper for
//! reordered / moved elements.
//!
//! Use case: a keyed `pp-for` list mutates its order in place (or
//! the author's code moves an element to a new position). The user
//! sees an instant jump; FLIP makes it animate smoothly to the new
//! spot.
//!
//! ## How it works
//!
//! 1. **First** — snapshot the element's bounding rect before the
//!    layout change.
//! 2. **Last** — DOM mutation happens (caller's responsibility, or
//!    it has already happened by the time we're here).
//! 3. **Invert** — compute delta `(old.x - new.x, old.y - new.y)`
//!    and apply an `inverse` transform so the element LOOKS like it
//!    hasn't moved. Browser paints the inverted frame.
//! 4. **Play** — on the next frame, transition the transform back
//!    to `none`; the user sees the element glide from its old spot
//!    to its new one.
//!
//! The two public shapes here:
//! - [`flip_from_snapshot`] — you already have the old rect (the
//!   pp-for keyed-diff path does), hand it in.
//! - [`flip`] — you don't; pass a `mutate` closure and a snapshot
//!   phase runs before it.

use wasm_bindgen::JsCast;
use web_sys::{DomRect, Element, HtmlElement};

use super::waapi::{animate, AnimateOptions, Keyframe};

/// Options for a FLIP animation.
#[derive(Clone, Debug)]
pub struct FlipOptions {
    /// Duration of the "play" phase in ms. Default 260.
    pub duration_ms: f64,
    /// CSS easing. Default a gentle ease-out curve that feels
    /// natural for layout shifts.
    pub easing: &'static str,
    /// If the total movement is under this threshold in pixels, skip
    /// the animation entirely. Default 2 (sub-pixel / no-op moves).
    pub min_delta_px: f64,
}

impl Default for FlipOptions {
    fn default() -> Self {
        Self {
            duration_ms: 260.0,
            easing: "cubic-bezier(0.2, 0.8, 0.2, 1)",
            min_delta_px: 2.0,
        }
    }
}

/// Run a FLIP animation on `el` given its rect **before** the
/// mutation. The current rect is measured now, the delta computed,
/// and a Web Animation plays `transform: translate(old-new)` →
/// `transform: translate(0)`.
///
/// Safe to call on every frame of a keyed reconcile — if the
/// element hasn't moved past `min_delta_px`, it returns without
/// scheduling an animation.
pub fn flip_from_snapshot(el: &Element, old_rect: DomRect, opts: FlipOptions) {
    let new_rect = el.get_bounding_client_rect();
    let dx = old_rect.left() - new_rect.left();
    let dy = old_rect.top() - new_rect.top();
    if dx.abs() < opts.min_delta_px && dy.abs() < opts.min_delta_px {
        return;
    }

    // Cancel any in-flight transform animation on this element —
    // rapid re-orders shouldn't stack.
    if let Ok(html) = el.clone().dyn_into::<HtmlElement>() {
        // Clear any previously-applied transform inline style so
        // the new keyframe starts from a known state. The
        // `fill: "forwards"` on the prior animation might still
        // hold the final transform; a fresh animate() overrides.
        let _ = html.style().remove_property("transform");
    }

    let from_transform = format!("translate({}px, {}px)", dx, dy);
    animate(
        el,
        &[
            Keyframe::from_iter([("transform", from_transform.as_str())]),
            Keyframe::from_iter([("transform", "translate(0, 0)")]),
        ],
        AnimateOptions {
            duration_ms: opts.duration_ms,
            easing: opts.easing,
            delay_ms: 0.0,
            // `none` so the transform fully clears once the
            // animation finishes — otherwise the element would
            // keep `transform: translate(0, 0)` forever, which can
            // block stacking-context-sensitive layout.
            fill: "none",
        },
    );
}

/// Measure `el`'s rect, run `mutate`, then FLIP-animate the
/// difference. Convenience for the "I know the mutation about to
/// happen" case. For keyed `pp-for` — where the walker mutates the
/// DOM internally — use [`flip_from_snapshot`] with the rect
/// captured before the walker runs.
pub fn flip(el: &Element, mutate: impl FnOnce(), opts: FlipOptions) {
    let rect = el.get_bounding_client_rect();
    mutate();
    flip_from_snapshot(el, rect, opts);
}
