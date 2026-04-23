//! Stagger — compute per-index delay offsets for group animations.
//!
//! Ported from Motion's `calcChildStagger` and flip-toolkit's
//! `staggerConfig`. Handles the common cases Motion authors expect:
//!
//! ```ignore
//! use pine_motion::{animate, stagger::{Stagger, Origin}, Spring};
//!
//! // Each child 50ms after the previous, from the first element.
//! let s = Stagger::new(50.0);
//! for (i, el) in elements.iter().enumerate() {
//!     animate(
//!         el,
//!         &[("opacity", "0", "1")],
//!         Spring::gentle().into_timing_with_delay(s.delay_for(i, elements.len())),
//!     );
//! }
//!
//! // Emanate from center.
//! let s = Stagger::new(40.0).from(Origin::Center);
//!
//! // Reverse direction (last-first).
//! let s = Stagger::new(50.0).from(Origin::Last);
//!
//! // Non-linear distribution — later items slower (drop-in ease-out).
//! let s = Stagger::new(50.0).ease(Easing::EASE_OUT_EXPO);
//! ```

use crate::easing::Easing;

/// Where the stagger sequence originates. `First` gives the familiar
/// "top-down" cascade; `Center` is the Motion-signature "emanate
/// outward" feel.
#[derive(Clone, Copy, Debug)]
pub enum Origin {
    /// Index 0 starts at t=0; index N ends at t = N · each_ms.
    First,
    /// Last index starts at t=0; index 0 ends at t = N · each_ms.
    Last,
    /// Middle index starts at t=0; ends emanate outward to both
    /// sides. Distances use absolute `|i - mid|`.
    Center,
    /// Sequence starts at a specific index.
    At(usize),
    /// Start position as a 0..=1 ratio across the list.
    Ratio(f64),
}

#[derive(Clone, Copy, Debug)]
pub struct Stagger {
    pub each_ms: f64,
    pub from: Origin,
    /// Optional easing for the distribution. `None` = linear spacing.
    /// With an easing, the *relative* position along the stagger
    /// curve passes through the easing before multiplying by the
    /// total stagger duration — so an `EASE_OUT_EXPO` distribution
    /// bunches later items and spaces earlier ones wider.
    pub ease: Option<Easing>,
}

impl Stagger {
    /// Linear stagger — each child `each_ms` after the previous,
    /// starting at index 0.
    pub const fn new(each_ms: f64) -> Self {
        Self {
            each_ms,
            from: Origin::First,
            ease: None,
        }
    }

    pub fn from(mut self, origin: Origin) -> Self {
        self.from = origin;
        self
    }

    pub fn ease(mut self, e: Easing) -> Self {
        self.ease = Some(e);
        self
    }

    /// Compute the delay in ms for `index` out of `total`.
    /// Indices outside `0..total` return 0.
    pub fn delay_for(&self, index: usize, total: usize) -> f64 {
        if total == 0 || index >= total {
            return 0.0;
        }
        let (mid_idx, distance) = match self.from {
            Origin::First => (0_f64, index as f64),
            Origin::Last => {
                let last = (total - 1) as f64;
                (last, last - index as f64)
            }
            Origin::Center => {
                let mid = (total - 1) as f64 / 2.0;
                (mid, (index as f64 - mid).abs())
            }
            Origin::At(at) => (at as f64, (index as f64 - at as f64).abs()),
            Origin::Ratio(r) => {
                let r = r.clamp(0.0, 1.0);
                let at = r * (total - 1) as f64;
                (at, (index as f64 - at).abs())
            }
        };
        let _ = mid_idx; // `mid_idx` is computed for clarity; only distance is used downstream

        // Apply optional easing as a distribution curve. We need a
        // max distance to normalize `distance` to 0..1 first. The
        // worst case for each origin:
        //   First/Last: (total - 1)
        //   Center:     (total - 1) / 2
        //   At(k):      max(k, total - 1 - k)
        //   Ratio(r):   same as At after resolution
        let max_dist = match self.from {
            Origin::First | Origin::Last => (total - 1) as f64,
            Origin::Center => (total - 1) as f64 / 2.0,
            Origin::At(at) => {
                let at = at.min(total.saturating_sub(1)) as f64;
                at.max((total - 1) as f64 - at)
            }
            Origin::Ratio(r) => {
                let r = r.clamp(0.0, 1.0);
                let at = r * (total - 1) as f64;
                at.max((total - 1) as f64 - at)
            }
        };

        if max_dist < f64::EPSILON {
            return 0.0;
        }

        let normalized = distance / max_dist;
        let curved = match self.ease {
            None => normalized,
            Some(e) => e.sample(normalized),
        };
        // Multiply by total stagger span (each_ms * max_dist) so the
        // per-step feel matches a plain linear cascade at the
        // endpoints. A Ratio-positioned origin therefore still ends
        // the latest child at `each_ms * max_dist`, regardless of
        // which end it is.
        curved * self.each_ms * max_dist
    }
}

/// Shortcut — equivalent to `Stagger::new(each_ms)`.
pub const fn stagger(each_ms: f64) -> Stagger {
    Stagger::new(each_ms)
}

impl Easing {
    /// Evaluate this easing at `progress ∈ 0..=1`. Used by stagger
    /// distribution curves. Note: sampling a spring here would be
    /// wasteful (rebuilds the solver each call), so a spring easing
    /// just returns its position at `progress * settle_time`.
    pub(crate) fn sample(&self, progress: f64) -> f64 {
        let p = progress.clamp(0.0, 1.0);
        match self {
            Easing::Linear => p,
            Easing::Ease => cubic_bezier_at([0.25, 0.1, 0.25, 1.0], p),
            Easing::EaseIn => cubic_bezier_at([0.42, 0.0, 1.0, 1.0], p),
            Easing::EaseOut => cubic_bezier_at([0.0, 0.0, 0.58, 1.0], p),
            Easing::EaseInOut => cubic_bezier_at([0.42, 0.0, 0.58, 1.0], p),
            Easing::CubicBezier(pts) => cubic_bezier_at(*pts, p),
            Easing::Spring(s) => s.at(p * s.settle_duration_ms()),
        }
    }
}

/// Evaluate a cubic-bezier (with handles `(x1,y1,x2,y2)`, endpoints
/// fixed at (0,0) and (1,1)) at a given progress `p ∈ 0..=1`. This is
/// the same two-stage approach CSS uses: binary-search for the t
/// that maps to x=p, then evaluate y(t).
fn cubic_bezier_at(handles: [f64; 4], p: f64) -> f64 {
    let [x1, y1, x2, y2] = handles;
    // Find t such that X(t) = p, where X(t) = 3(1-t)²t·x1 + 3(1-t)t²·x2 + t³.
    let mut lo = 0.0_f64;
    let mut hi = 1.0_f64;
    let mut t = p;
    for _ in 0..24 {
        let x = cubic_at(x1, x2, t);
        if (x - p).abs() < 1e-4 {
            break;
        }
        if x < p {
            lo = t;
        } else {
            hi = t;
        }
        t = (lo + hi) / 2.0;
    }
    cubic_at(y1, y2, t)
}

fn cubic_at(c1: f64, c2: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    3.0 * u * u * t * c1 + 3.0 * u * t * t * c2 + t * t * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_from_first() {
        let s = Stagger::new(50.0);
        assert_eq!(s.delay_for(0, 5), 0.0);
        assert!((s.delay_for(2, 5) - 100.0).abs() < 1e-6);
        assert!((s.delay_for(4, 5) - 200.0).abs() < 1e-6);
    }

    #[test]
    fn reversed_from_last() {
        let s = Stagger::new(50.0).from(Origin::Last);
        assert!((s.delay_for(4, 5) - 0.0).abs() < 1e-6);
        assert!((s.delay_for(0, 5) - 200.0).abs() < 1e-6);
    }

    #[test]
    fn center_emanates() {
        // 5 items, mid = index 2. Indices 2, 1/3, 0/4 at distances
        // 0, 1, 2 respectively. each_ms * max_dist = 50 * 2 = 100
        // total stagger; linear distribution gives 0, 50, 100.
        let s = Stagger::new(50.0).from(Origin::Center);
        assert!((s.delay_for(2, 5) - 0.0).abs() < 1e-6);
        assert!((s.delay_for(1, 5) - 50.0).abs() < 1e-6);
        assert!((s.delay_for(3, 5) - 50.0).abs() < 1e-6);
        assert!((s.delay_for(0, 5) - 100.0).abs() < 1e-6);
        assert!((s.delay_for(4, 5) - 100.0).abs() < 1e-6);
    }

    #[test]
    fn at_specific_index() {
        let s = Stagger::new(50.0).from(Origin::At(1));
        assert!((s.delay_for(1, 5) - 0.0).abs() < 1e-6);
        // At(1) with 5 items: max distance = max(1, 3) = 3,
        // total stagger = 50 * 3 = 150. Index 0 is distance 1 → 50.
        assert!((s.delay_for(0, 5) - 50.0).abs() < 1e-6);
        assert!((s.delay_for(4, 5) - 150.0).abs() < 1e-6);
    }

    #[test]
    fn out_of_range_is_zero() {
        let s = Stagger::new(50.0);
        assert_eq!(s.delay_for(10, 5), 0.0);
        assert_eq!(s.delay_for(0, 0), 0.0);
    }

    #[test]
    fn single_item_is_zero() {
        let s = Stagger::new(50.0);
        assert_eq!(s.delay_for(0, 1), 0.0);
    }

    #[test]
    fn ease_out_distribution_bunches_later_items() {
        // With ease-out, the early indices should get a larger share
        // of the stagger span (they are further from the accelerating
        // tail), so later-index delays grow faster than linear.
        let linear = Stagger::new(100.0);
        let curved = Stagger::new(100.0).ease(Easing::EaseOut);

        let delta_linear = linear.delay_for(3, 10) - linear.delay_for(2, 10);
        let delta_curved = curved.delay_for(3, 10) - curved.delay_for(2, 10);
        // ease-out starts fast (big early deltas) and decelerates
        // toward the end. Our index→delay function applies the
        // easing to a normalized distance; the "2→3" step is still
        // early, so curved should still be bigger than linear there.
        assert!(
            delta_curved > delta_linear,
            "ease-out should expand early deltas vs linear ({} vs {})",
            delta_curved,
            delta_linear
        );
    }

    #[test]
    fn cubic_bezier_endpoints() {
        // (0,0) → (1,1) at endpoints regardless of handles.
        let v0 = cubic_bezier_at([0.42, 0.0, 0.58, 1.0], 0.0);
        let v1 = cubic_bezier_at([0.42, 0.0, 0.58, 1.0], 1.0);
        assert!(v0.abs() < 1e-3, "ease-in-out at 0 should be 0, got {v0}");
        assert!(
            (v1 - 1.0).abs() < 1e-3,
            "ease-in-out at 1 should be 1, got {v1}"
        );
    }
}
