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

use pocopine_core::animate::AnimationHandle;

use crate::animate::animate;
use crate::easing::Easing;
use crate::gesture::GestureHandle;
use crate::gesture::pan::{PanConfig, PanEvent, pan};
use crate::spring::Spring;

const MOMENTUM_POWER_SECONDS: f64 = 0.35;

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
        Self {
            left,
            right,
            top,
            bottom,
        }
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
    let current_delta: Rc<RefCell<(f64, f64)>> = Rc::new(RefCell::new((0.0, 0.0)));
    let drag_origin_delta: Rc<RefCell<(f64, f64)>> = Rc::new(RefCell::new((0.0, 0.0)));
    let active_release: Rc<RefCell<Option<AnimationHandle>>> = Rc::new(RefCell::new(None));
    let html = el.clone().dyn_into::<HtmlElement>().ok();

    let cfg_cb = cfg.clone();
    let current_cb = current_delta.clone();
    let origin_cb = drag_origin_delta.clone();
    let active_cb = active_release.clone();
    let el_cb = el.clone();
    let html_cb = html.clone();

    pan(el, PanConfig::default(), move |event| {
        match event {
            PanEvent::Start { .. } => {
                if let Some(handle) = active_cb.borrow_mut().take() {
                    handle.cancel();
                }
                *origin_cb.borrow_mut() = *current_cb.borrow();
            }
            PanEvent::Move { delta, .. } => {
                let origin = *origin_cb.borrow();
                let next = (origin.0 + delta.0, origin.1 + delta.1);
                let clamped = clamp_delta(next, &cfg_cb);
                *current_cb.borrow_mut() = clamped;
                if let Some(h) = &html_cb {
                    let _ = h.style().set_property(
                        "transform",
                        &format!("translate({}px, {}px)", clamped.0, clamped.1),
                    );
                }
            }
            PanEvent::End { velocity } => {
                let final_delta = *current_cb.borrow();
                if cfg_cb.snap_to_origin {
                    let target = (0.0, 0.0);
                    *current_cb.borrow_mut() = target;
                    spring_to(
                        &el_cb,
                        html_cb.clone(),
                        active_cb.clone(),
                        final_delta,
                        target,
                        cfg_cb.release_spring,
                    );
                } else if cfg_cb.momentum {
                    // Motion's drag momentum is inertia-first: project a
                    // likely rest point from release velocity, then animate
                    // there. We use a small fixed power window because this
                    // API exposes a spring-only release primitive for now.
                    let estimated_rest = extrapolate_rest(final_delta, velocity);
                    let clamped_rest = clamp_delta(estimated_rest, &cfg_cb);
                    *current_cb.borrow_mut() = clamped_rest;
                    let spring = spring_with_projected_velocity(
                        cfg_cb.release_spring,
                        final_delta,
                        clamped_rest,
                        velocity,
                    );
                    spring_to(
                        &el_cb,
                        html_cb.clone(),
                        active_cb.clone(),
                        final_delta,
                        clamped_rest,
                        spring,
                    );
                }
            }
            PanEvent::Cancel => {
                let from = *current_cb.borrow();
                let target = (0.0, 0.0);
                *current_cb.borrow_mut() = target;
                spring_to(
                    &el_cb,
                    html_cb.clone(),
                    active_cb.clone(),
                    from,
                    target,
                    cfg_cb.release_spring,
                );
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

fn spring_to(
    el: &Element,
    html: Option<HtmlElement>,
    active_release: Rc<RefCell<Option<AnimationHandle>>>,
    from: (f64, f64),
    to: (f64, f64),
    spring: Spring,
) {
    let from_css = translate_css(from);
    let to_css = translate_css(to);
    let handle = animate(
        el,
        &[("transform", &from_css, &to_css)],
        Easing::Spring(spring),
    );
    handle.on_finish({
        let active_release = active_release.clone();
        move || {
            if let Some(h) = html {
                let _ = h.style().set_property("transform", &to_css);
            }
            // Commit the final transform into inline style, then remove
            // the filled WAAPI effect so the next drag owns transform again.
            if let Some(handle) = active_release.borrow_mut().take() {
                handle.cancel();
            }
        }
    });
    *active_release.borrow_mut() = Some(handle);
}

/// Estimate a rest position for a release fling. Motion's inertia
/// projects from velocity before applying constraints; this is the
/// same idea with a conservative fixed power window.
fn extrapolate_rest(current: (f64, f64), velocity: (f64, f64)) -> (f64, f64) {
    let factor = MOMENTUM_POWER_SECONDS;
    (
        current.0 + velocity.0 * factor,
        current.1 + velocity.1 * factor,
    )
}

fn spring_with_projected_velocity(
    spring: Spring,
    from: (f64, f64),
    to: (f64, f64),
    velocity: (f64, f64),
) -> Spring {
    let travel = (to.0 - from.0, to.1 - from.1);
    let distance_sq = travel.0 * travel.0 + travel.1 * travel.1;
    if distance_sq <= 1e-6 {
        return spring;
    }
    let projected_velocity = (velocity.0 * travel.0 + velocity.1 * travel.1) / distance_sq;
    spring.with_velocity(projected_velocity)
}

fn translate_css(delta: (f64, f64)) -> String {
    format!("translate({}px, {}px)", delta.0, delta.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn momentum_rest_projects_far_enough_to_read_as_a_fling() {
        let rest = extrapolate_rest((10.0, -5.0), (1000.0, -400.0));
        assert_eq!(rest, (360.0, -145.0));
    }

    #[test]
    fn drag_delta_is_clamped_after_accumulating_from_current_position() {
        let cfg = DragConfig {
            constraints: Some(DragConstraints::rect(-20.0, 120.0, -10.0, 80.0)),
            ..Default::default()
        };
        assert_eq!(clamp_delta((130.0, 90.0), &cfg), (120.0, 80.0));
    }

    #[test]
    fn projected_release_velocity_is_normalized_to_travel() {
        let spring = spring_with_projected_velocity(
            Spring::gentle(),
            (0.0, 0.0),
            (100.0, 0.0),
            (500.0, 250.0),
        );
        assert_eq!(spring.velocity, 5.0);
    }
}
