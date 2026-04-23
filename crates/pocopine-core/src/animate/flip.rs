//! FLIP — _First, Last, Invert, Play_. Layout-animation helper for
//! reordered / moved elements.
//!
//! Every FLIP runs as a single `Element.animate()` call with a
//! two-key transform keyframe. WAAPI runs on its own layer, so it
//! composes with author transforms/transitions without fighting
//! over the inline `style.transform` / `style.transition` slots.
//!
//! Rapid reorders: the returned `Animation` handle is stashed on
//! the element so a subsequent FLIP can cancel its predecessor.
//! `fill: "none"` ensures the transform contribution fully clears
//! when the animation settles — no lingering stacking context.
//!
//! Entry points:
//! - [`flip_from_snapshot`] / [`flip_with_new_rect`] — single element.
//! - [`flip_batch`] — iterator of pre-measured [`FlipTarget`]s.
//! - [`flip`] — measure + mutate + play, convenience.

use std::borrow::Cow;

use js_sys::Reflect;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Animation, DomRect, Element};

use super::waapi::{animate, AnimateOptions, Keyframe};

/// Private slot for the most recent FLIP `Animation` handle.
const FLIP_ANIM_KEY: &str = "__pp_flip_anim";

#[derive(Clone, Debug)]
pub struct FlipOptions {
    pub duration_ms: f64,
    pub easing: Cow<'static, str>,
    pub min_delta_px: f64,
}

impl Default for FlipOptions {
    fn default() -> Self {
        // Apple-style sharp deceleration — matches the global
        // `--pp-tx-easing` so FLIP reads as part of the same
        // motion language as the pp-transition presets. The long
        // settle tail is what reads as "springy" without an actual
        // spring simulation (motion.dev / react-flip-toolkit's
        // signature feel, ported to a plain WAAPI tween). pine-motion
        // callers can substitute a sampled `linear(...)` string here
        // to get a real spring on the compositor.
        Self {
            duration_ms: 320.0,
            easing: Cow::Borrowed("cubic-bezier(0.16, 1, 0.3, 1)"),
            min_delta_px: 2.0,
        }
    }
}

pub struct FlipTarget {
    pub element: Element,
    pub old_rect: DomRect,
    pub new_rect: DomRect,
}

pub fn flip_from_snapshot(el: &Element, old_rect: DomRect, opts: FlipOptions) {
    let new_rect = el.get_bounding_client_rect();
    flip_with_new_rect(el, old_rect, new_rect, opts);
}

pub fn flip_with_new_rect(el: &Element, old_rect: DomRect, new_rect: DomRect, opts: FlipOptions) {
    let dx = old_rect.left() - new_rect.left();
    let dy = old_rect.top() - new_rect.top();
    if dx.abs() < opts.min_delta_px && dy.abs() < opts.min_delta_px {
        return;
    }
    cancel_prior(el);
    let from = format!("translate({dx}px, {dy}px)");
    let handle = animate(
        el,
        &[
            Keyframe::from_iter([("transform", from.as_str())]),
            Keyframe::from_iter([("transform", "translate(0, 0)")]),
        ],
        AnimateOptions {
            duration_ms: opts.duration_ms,
            easing: opts.easing,
            delay_ms: 0.0,
            fill: "none",
            respect_motion_preference: true,
        },
    );
    stash(el, handle.raw());
}

pub fn flip_batch<I>(targets: I, opts: FlipOptions)
where
    I: IntoIterator<Item = FlipTarget>,
{
    for t in targets {
        flip_with_new_rect(&t.element, t.old_rect, t.new_rect, opts.clone());
    }
}

pub fn flip(el: &Element, mutate: impl FnOnce(), opts: FlipOptions) {
    let rect = el.get_bounding_client_rect();
    mutate();
    flip_from_snapshot(el, rect, opts);
}

/// Backwards-compatible re-export. The name survives for callers
/// that used to need a pause-and-jump dance to read a clean
/// layout rect; the WAAPI path's stashed-handle cancellation makes
/// that unnecessary, so this is now just a thin wrapper around
/// `get_bounding_client_rect`.
pub fn measure_layout_rect(el: &Element) -> DomRect {
    el.get_bounding_client_rect()
}

fn cancel_prior(el: &Element) {
    if let Some(anim) = stashed(el) {
        anim.cancel();
    }
    let _ = Reflect::set(
        el.as_ref(),
        &JsValue::from_str(FLIP_ANIM_KEY),
        &JsValue::UNDEFINED,
    );
}

fn stash(el: &Element, anim: &Animation) {
    let _ = Reflect::set(
        el.as_ref(),
        &JsValue::from_str(FLIP_ANIM_KEY),
        anim.as_ref(),
    );
}

fn stashed(el: &Element) -> Option<Animation> {
    let raw = Reflect::get(el.as_ref(), &JsValue::from_str(FLIP_ANIM_KEY)).ok()?;
    if raw.is_undefined() || raw.is_null() {
        return None;
    }
    raw.dyn_into::<Animation>().ok()
}
