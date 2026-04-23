//! Small interaction motion presets.
//!
//! These are convenience wrappers around [`crate::hover`] +
//! [`crate::animate`]. They intentionally stay tiny: use them for
//! common one-element affordances, and drop down to `animate()` for
//! anything bespoke.
//!
//! Caveat: these presets own `transform` while active. If an element
//! is already using transform for drag, FLIP, tilt, or layout
//! projection, apply the effect to a wrapper/child instead.

use web_sys::Element;

use crate::animate::animate;
use crate::gesture::{hover, GestureHandle};
use crate::spring::Spring;

const REST_TRANSFORM: &str = "translateY(0px) scale(1)";
const REST_SHADOW: &str = "0 4px 10px rgba(0, 0, 0, 0.10)";
const RAISE_SHADOW: &str = "0 16px 32px rgba(0, 0, 0, 0.18)";

/// Built-in hover motion presets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoverMotion {
    /// Lift upward and deepen the shadow.
    Raise,
    /// Scale to the configured factor.
    Scale,
}

/// Tuning for [`hover_motion`].
#[derive(Clone, Copy, Debug)]
pub struct HoverMotionConfig {
    pub spring: Spring,
    pub raise_px: f64,
    pub scale: f64,
}

impl Default for HoverMotionConfig {
    fn default() -> Self {
        Self {
            spring: Spring::gentle(),
            raise_px: 8.0,
            scale: 1.04,
        }
    }
}

/// Attach one of the built-in hover motion presets to `el`.
pub fn hover_motion(el: &Element, motion: HoverMotion, cfg: HoverMotionConfig) -> GestureHandle {
    let target = el.clone();
    let animated = el.clone();
    hover(&target, move |entered, _| {
        let active_transform = motion.active_transform(&cfg);
        let (from_transform, to_transform) = if entered {
            (REST_TRANSFORM, active_transform.as_str())
        } else {
            (active_transform.as_str(), REST_TRANSFORM)
        };

        let mut channels = vec![("transform", from_transform, to_transform)];
        if motion == HoverMotion::Raise {
            let (from_shadow, to_shadow) = if entered {
                (REST_SHADOW, RAISE_SHADOW)
            } else {
                (RAISE_SHADOW, REST_SHADOW)
            };
            channels.push(("boxShadow", from_shadow, to_shadow));
        }

        animate(&animated, &channels, cfg.spring);
    })
}

/// Lift on hover.
pub fn raise(el: &Element) -> GestureHandle {
    hover_motion(el, HoverMotion::Raise, HoverMotionConfig::default())
}

/// Scale on hover. Values above `1.0` grow; below `1.0` shrink.
pub fn scale(el: &Element, factor: f64) -> GestureHandle {
    hover_motion(
        el,
        HoverMotion::Scale,
        HoverMotionConfig {
            scale: factor,
            ..Default::default()
        },
    )
}

impl HoverMotion {
    fn active_transform(self, cfg: &HoverMotionConfig) -> String {
        match self {
            HoverMotion::Raise => format!("translateY(-{}px) scale(1)", cfg.raise_px),
            HoverMotion::Scale => format!("translateY(0px) scale({})", cfg.scale),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_build_expected_transforms() {
        let cfg = HoverMotionConfig::default();
        assert_eq!(
            HoverMotion::Raise.active_transform(&cfg),
            "translateY(-8px) scale(1)"
        );
        assert_eq!(
            HoverMotion::Scale.active_transform(&cfg),
            "translateY(0px) scale(1.04)"
        );
        let shrink = HoverMotionConfig {
            scale: 0.96,
            ..Default::default()
        };
        assert_eq!(HoverMotion::Scale.active_transform(&shrink), "translateY(0px) scale(0.96)");
    }
}
