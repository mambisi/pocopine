//! Drag — turn a pan into "element follows the pointer", with
//! optional bounds, axis lock, and release momentum.
//!
//! Layered on top of [`crate::gesture::pan`]: the pan session fires
//! the raw events, drag applies the cumulative delta as a CSS
//! `transform: translate(x, y)` on the element, and on release
//! either snaps back or runs a spring with the fling velocity.
//!
//! ```ignore
//! use pine_motion::drag::{drag, DragConfig, DragAxis, DragConstraints};
//! let _handle = drag(&el, DragConfig {
//!     axis: DragAxis::X,
//!     constraints: Some(DragConstraints::rect(-100.0, 100.0, 0.0, 0.0)),
//!     momentum: true,
//!     snap_to_origin: false,
//!     ..Default::default()
//! });
//! ```
//!
//! **v0 caveat**: drag sets `style.transform` wholesale. If the
//! element already has an author transform, it gets replaced. Apply
//! drag to a wrapper element if you need to preserve other
//! transforms.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement};

use crate::animate::animate;
use crate::easing::Easing;
use crate::gesture::pan::{pan, PanConfig, PanEvent};
use crate::gesture::GestureHandle;
use crate::spring::Spring;

/// Which axis the element is allowed to move along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragAxis {
    X,
    Y,
    Both,
}

/// Rectangular bounds on the drag delta (not the element's final
/// client position). All values in px, relative to the drag origin.
/// Use large values for "unbounded on this side".
#[derive(Clone, Copy, Debug)]
pub struct DragConstraints {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

impl DragConstraints {
    pub fn rect(left: f64, right: f64, top: f64, bottom: f64) -> Self {
        Self { left, right, top, bottom }
    }
}

#[derive(Clone, Debug)]
pub struct DragConfig {
    pub axis: DragAxis,
    pub constraints: Option<DragConstraints>,
    /// On release, run a spring with the fling velocity so the
    /// element continues its momentum. Default `true`.
    pub momentum: bool,
    /// On release, animate back to origin regardless of velocity.
    /// Wins over `momentum`. Default `false`.
    pub snap_to_origin: bool,
    /// Spring used for momentum + snap-back. Defaults to `gentle`.
    pub release_spring: Spring,
}

impl Default for DragConfig {
    fn default() -> Self {
        Self {
            axis: DragAxis::Both,
            constraints: None,
            momentum: true,
            snap_to_origin: false,
            release_spring: Spring::gentle(),
        }
    }
}

/// Attach a drag observer to `el`. Returns a [`GestureHandle`] whose
/// drop removes the underlying pan listeners.
pub fn drag(el: &Element, cfg: DragConfig) -> GestureHandle {
    let cfg = Rc::new(cfg);
    let last_delta: Rc<RefCell<(f64, f64)>> = Rc::new(RefCell::new((0.0, 0.0)));
    let html = el.clone().dyn_into::<HtmlElement>().ok();

    let cfg_cb = cfg.clone();
    let last_cb = last_delta.clone();
    let el_cb = el.clone();
    let html_cb = html.clone();

    pan(el, PanConfig::default(), move |event| {
        match event {
            PanEvent::Start { .. } => {
                // Nothing to do at start — transform is whatever the
                // element already had.
            }
            PanEvent::Move { delta, .. } => {
                let clamped = clamp_delta(delta, &cfg_cb);
                *last_cb.borrow_mut() = clamped;
                if let Some(h) = &html_cb {
                    let _ = h.style().set_property(
                        "transform",
                        &format!("translate({}px, {}px)", clamped.0, clamped.1),
                    );
                }
            }
            PanEvent::End { velocity } => {
                let final_delta = *last_cb.borrow();
                if cfg_cb.snap_to_origin {
                    spring_to_origin(&el_cb, final_delta, &cfg_cb.release_spring);
                } else if cfg_cb.momentum {
                    // Continue the fling. For v0 we don't run a true
                    // inertia solver — instead we spring from the
                    // current position to an estimated rest position
                    // based on the velocity and the spring's settle
                    // time. This matches Motion's behaviour for a
                    // spring-backed drag (vs inertia-based).
                    let estimated_rest =
                        extrapolate_rest(final_delta, velocity, &cfg_cb.release_spring);
                    let clamped_rest = clamp_delta(estimated_rest, &cfg_cb);
                    *last_cb.borrow_mut() = clamped_rest;
                    let from = translate_css(final_delta);
                    let to = translate_css(clamped_rest);
                    animate(
                        &el_cb,
                        &[("transform", &from, &to)],
                        Easing::Spring(cfg_cb.release_spring),
                    );
                }
            }
            PanEvent::Cancel => {
                spring_to_origin(&el_cb, *last_cb.borrow(), &cfg_cb.release_spring);
                *last_cb.borrow_mut() = (0.0, 0.0);
            }
        }
    })
}

fn clamp_delta(delta: (f64, f64), cfg: &DragConfig) -> (f64, f64) {
    let axis_masked = match cfg.axis {
        DragAxis::X => (delta.0, 0.0),
        DragAxis::Y => (0.0, delta.1),
        DragAxis::Both => delta,
    };
    match cfg.constraints {
        None => axis_masked,
        Some(c) => (
            axis_masked.0.clamp(c.left, c.right),
            axis_masked.1.clamp(c.top, c.bottom),
        ),
    }
}

fn spring_to_origin(el: &Element, from: (f64, f64), spring: &Spring) {
    let from_css = translate_css(from);
    animate(
        el,
        &[("transform", &from_css, "translate(0px, 0px)")],
        Easing::Spring(*spring),
    );
}

/// Estimate a rest position for a release fling. The spring's
/// critically-damped settle distance under an initial velocity `v`
/// is roughly `v / (stiffness/mass)`; we take the vector form and
/// add to the current delta.
fn extrapolate_rest(
    current: (f64, f64),
    velocity: (f64, f64),
    spring: &Spring,
) -> (f64, f64) {
    let k_over_m = spring.stiffness / spring.mass;
    let factor = 1.0 / k_over_m.max(1e-3);
    (current.0 + velocity.0 * factor, current.1 + velocity.1 * factor)
}

fn translate_css(delta: (f64, f64)) -> String {
    format!("translate({}px, {}px)", delta.0, delta.1)
}
