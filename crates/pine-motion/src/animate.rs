//! Public `animate()` entry point.
//!
//! Motion-style:
//!
//! ```ignore
//! use pine_motion::{animate, Spring, Tween};
//!
//! // Tween
//! animate(&el, &[("opacity", "0", "1")], Tween::new().duration(300).ease_out());
//!
//! // Spring — sampled to linear() and run on GPU compositor
//! animate(&el, &[("transform", "scale(0.8)", "scale(1)")], Spring::default());
//!
//! // Named preset
//! animate(&el, &[("transform", "translateX(0)", "translateX(100px)")], Spring::wobbly());
//! ```
//!
//! Under the hood this delegates to [`pocopine_core::animate::animate`]
//! so we share the reduced-motion handling, fallback Animation
//! object, and future WAAPI additions for free.

use std::borrow::Cow;

pub use pocopine_core::animate::AnimationHandle;

use pocopine_core::animate::{AnimateOptions, Keyframe};
use web_sys::Element;

use crate::easing::Easing;
use crate::spring::Spring;

/// A single animated property with its from/to values. The CSS
/// property name is `&'static str` (CSS properties are always
/// literals); the from/to values are `&str` so callers can pass
/// either literals or formatted runtime strings without leaking.
pub type Channel<'a> = (&'static str, &'a str, &'a str);

/// Author-facing animation options. Build with [`Tween::new`] or
/// pass a [`Spring`] directly via `Into`.
#[derive(Clone, Debug)]
pub struct Tween {
    pub duration_ms: f64,
    pub easing: Easing,
    pub delay_ms: f64,
    pub respect_motion_preference: bool,
}

impl Default for Tween {
    fn default() -> Self {
        Self {
            duration_ms: 300.0,
            easing: Easing::EaseOut,
            delay_ms: 0.0,
            respect_motion_preference: true,
        }
    }
}

impl Tween {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn duration(mut self, ms: f64) -> Self {
        self.duration_ms = ms;
        self
    }

    pub fn delay(mut self, ms: f64) -> Self {
        self.delay_ms = ms;
        self
    }

    pub fn easing(mut self, e: Easing) -> Self {
        self.easing = e;
        self
    }

    pub fn ease_out(mut self) -> Self {
        self.easing = Easing::EaseOut;
        self
    }

    pub fn ease_in(mut self) -> Self {
        self.easing = Easing::EaseIn;
        self
    }

    pub fn ease_in_out(mut self) -> Self {
        self.easing = Easing::EaseInOut;
        self
    }

    /// Ignore the system reduced-motion preference for this tween.
    /// Use sparingly — only for motion that conveys data (progress
    /// indicators, drag feedback).
    pub fn always_animate(mut self) -> Self {
        self.respect_motion_preference = false;
        self
    }
}

/// Trait for anything that can produce the WAAPI timing bundle. Lets
/// `animate()` accept either a [`Tween`] or a [`Spring`] directly.
pub trait IntoTiming {
    fn into_timing(self) -> Tween;
}

impl IntoTiming for Tween {
    fn into_timing(self) -> Tween {
        self
    }
}

impl IntoTiming for Spring {
    /// A bare spring becomes a Tween whose duration and easing are
    /// derived from the spring's settle time and sampled profile.
    fn into_timing(self) -> Tween {
        Tween {
            duration_ms: self.settle_duration_ms(),
            easing: Easing::Spring(self),
            delay_ms: 0.0,
            respect_motion_preference: true,
        }
    }
}

impl IntoTiming for Easing {
    fn into_timing(self) -> Tween {
        let duration_ms = self.intrinsic_duration_ms().unwrap_or(300.0);
        Tween {
            duration_ms,
            easing: self,
            delay_ms: 0.0,
            respect_motion_preference: true,
        }
    }
}

/// Run a Motion-style animation on `el`.
///
/// `channels` is a list of `(css_property, from, to)` triples. A
/// spring or tween runs exactly one underlying WAAPI animation
/// (two-keyframe) on all channels together, so shared timing stays
/// in sync.
pub fn animate<T: IntoTiming>(
    el: &Element,
    channels: &[Channel<'_>],
    timing: T,
) -> AnimationHandle {
    let tween = timing.into_timing();

    // Build `from` and `to` keyframes from the channel list.
    let from_props: Vec<(&'static str, String)> = channels
        .iter()
        .map(|(p, from, _)| (*p, (*from).to_string()))
        .collect();
    let to_props: Vec<(&'static str, String)> = channels
        .iter()
        .map(|(p, _, to)| (*p, (*to).to_string()))
        .collect();
    let keyframes = [Keyframe { props: from_props }, Keyframe { props: to_props }];

    // Stringify easing at the chosen duration. Springs compile to
    // `linear(...)`; cubic-beziers/named easings land as their native
    // CSS string. `AnimateOptions.easing` is a `Cow<'static, str>`
    // so we hand over the owned String without leaking.
    let easing_css = tween.easing.as_css(tween.duration_ms);

    let opts = AnimateOptions {
        duration_ms: tween.duration_ms,
        easing: Cow::Owned(easing_css),
        delay_ms: tween.delay_ms,
        fill: "forwards",
        respect_motion_preference: tween.respect_motion_preference,
    };

    pocopine_core::animate::animate(el, &keyframes, opts)
}
