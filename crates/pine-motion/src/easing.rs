//! Named easings + the linear-easing sampler.
//!
//! The sampler is the trick that makes Motion's springs feel buttery:
//! any `fn(progress) -> value` gets sampled at N points and emitted
//! as a CSS `linear(v0, v1, …)` timing function string. Browsers that
//! support the `linear()` timing function (Chrome 113+, Safari 17.4+,
//! Firefox 112+) then run the animation on the GPU compositor with
//! zero per-frame JS. Ported from
//! `motion-dom/src/animation/waapi/utils/linear.ts`.
//!
//! For older browsers the caller should fall back to a named easing
//! like `ease-out` — see [`Easing::as_css`].

use crate::spring::Spring;

/// An animation's timing curve. Either a named browser-native easing
/// (a single string lands on `Element.animate`'s `easing` option), or
/// a spring/function that gets sampled into `linear(...)`.
#[derive(Clone, Copy, Debug)]
pub enum Easing {
    /// `linear`
    Linear,
    /// `ease`
    Ease,
    /// `ease-in`
    EaseIn,
    /// `ease-out` — the default for most UI motion.
    EaseOut,
    /// `ease-in-out`
    EaseInOut,
    /// Custom cubic-bezier curve. Matches `cubic-bezier(a, b, c, d)`.
    CubicBezier([f64; 4]),
    /// Spring physics. Gets compiled to `linear(...)` via sampling.
    Spring(Spring),
}

impl Easing {
    /// Named presets popular in the motion library world.
    pub const EASE_OUT_QUAD: Easing = Easing::CubicBezier([0.25, 0.46, 0.45, 0.94]);
    pub const EASE_OUT_EXPO: Easing = Easing::CubicBezier([0.22, 1.0, 0.36, 1.0]);
    pub const EASE_OUT_BACK: Easing = Easing::CubicBezier([0.34, 1.56, 0.64, 1.0]);
    pub const EASE_OUT_CIRC: Easing = Easing::CubicBezier([0.55, 0.0, 1.0, 0.45]);
    /// Apple-style "sharp deceleration" curve — matches the global
    /// `--pp-tx-easing` in pocopine-core's preset atoms.
    pub const APPLE: Easing = Easing::CubicBezier([0.16, 1.0, 0.3, 1.0]);

    /// Render as the CSS string passed to `Element.animate`. Spring
    /// easings are sampled at 30ms intervals over the provided
    /// `duration_ms` and emitted as `linear(...)`.
    pub fn as_css(&self, duration_ms: f64) -> String {
        match self {
            Easing::Linear => "linear".into(),
            Easing::Ease => "ease".into(),
            Easing::EaseIn => "ease-in".into(),
            Easing::EaseOut => "ease-out".into(),
            Easing::EaseInOut => "ease-in-out".into(),
            Easing::CubicBezier([a, b, c, d]) => {
                format!("cubic-bezier({a}, {b}, {c}, {d})")
            }
            Easing::Spring(s) => sample_spring_to_linear(*s, duration_ms),
        }
    }

    /// Resolved duration in ms. Springs self-determine via their
    /// settle time; named easings return `None` so the caller's
    /// configured `duration_ms` wins.
    pub fn intrinsic_duration_ms(&self) -> Option<f64> {
        match self {
            Easing::Spring(s) => Some(s.settle_duration_ms()),
            _ => None,
        }
    }
}

impl Default for Easing {
    fn default() -> Self {
        Easing::EaseOut
    }
}

impl From<Spring> for Easing {
    fn from(s: Spring) -> Self {
        Easing::Spring(s)
    }
}

/// Sample an arbitrary `f(progress) -> value` over `duration_ms` and
/// emit it as a CSS `linear(v0, v1, …)` timing function. `resolution`
/// is the sampling step in ms; 10ms ≈ one frame at 100fps and matches
/// Motion's default.
pub fn sample_to_linear_easing(
    mut f: impl FnMut(f64) -> f64,
    duration_ms: f64,
    resolution_ms: f64,
) -> String {
    let resolution = resolution_ms.max(1.0);
    let num_points = ((duration_ms / resolution).round() as usize).max(2);
    let mut out = String::from("linear(");
    for i in 0..num_points {
        let progress = i as f64 / (num_points - 1) as f64;
        let v = (f(progress) * 10_000.0).round() / 10_000.0;
        if i > 0 {
            out.push_str(", ");
        }
        // %.4 precision is enough for imperceptible difference — any
        // extra digits just bloat the CSS string.
        let s = format_f64_short(v);
        out.push_str(&s);
    }
    out.push(')');
    out
}

fn sample_spring_to_linear(spring: Spring, _duration_ms: f64) -> String {
    // Spring resolves its own duration; ignore the caller's and use
    // the settle time for the sampling window. Without this, a
    // too-short window would clip the spring before it reaches rest.
    let settle = spring.settle_duration_ms();
    sample_to_linear_easing(|p| spring.at(p * settle), settle, 30.0)
}

fn format_f64_short(v: f64) -> String {
    // Trim trailing zeros. `{:.4}` always prints four decimals, so
    // `1.0` becomes `1.0000`, which is wasteful across N sample
    // points. Strip the ones after the decimal for CSS compactness.
    let s = format!("{v:.4}");
    if !s.contains('.') {
        return s;
    }
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".into()
    } else {
        trimmed.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_easings_stringify() {
        assert_eq!(Easing::Linear.as_css(300.0), "linear");
        assert_eq!(Easing::EaseOut.as_css(300.0), "ease-out");
    }

    #[test]
    fn cubic_bezier_stringifies() {
        let css = Easing::CubicBezier([0.16, 1.0, 0.3, 1.0]).as_css(300.0);
        assert_eq!(css, "cubic-bezier(0.16, 1, 0.3, 1)");
    }

    #[test]
    fn linear_sampler_emits_valid_string() {
        let css = sample_to_linear_easing(|p| p, 100.0, 10.0);
        assert!(css.starts_with("linear("), "got: {css}");
        assert!(css.ends_with(')'));
        // Identity should start at 0 and end at 1.
        assert!(css.contains("0,"));
        assert!(css.ends_with("1)"));
    }

    #[test]
    fn spring_easing_produces_linear_string() {
        let e = Easing::Spring(Spring::wobbly());
        let css = e.as_css(0.0); // duration ignored for springs
        assert!(css.starts_with("linear("), "got: {css}");
        // A wobbly spring should produce at least one sample > 1.0
        // (overshoot) somewhere in the stream.
        let has_overshoot = css
            .trim_start_matches("linear(")
            .trim_end_matches(')')
            .split(", ")
            .filter_map(|s| s.parse::<f64>().ok())
            .any(|v| v > 1.0);
        assert!(has_overshoot, "wobbly spring's linear() samples should overshoot: {css}");
    }

    #[test]
    fn no_trailing_zeros_in_samples() {
        // Trimming should collapse `0.5000` to `0.5`, `1.0000` to
        // `1`, `0.0000` to `0`.
        assert_eq!(format_f64_short(0.5), "0.5");
        assert_eq!(format_f64_short(1.0), "1");
        assert_eq!(format_f64_short(0.0), "0");
        assert_eq!(format_f64_short(0.1234), "0.1234");
    }
}
