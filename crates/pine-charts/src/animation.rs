pub(crate) const DEFAULT_ANIMATION_DURATION_MS: f64 = 160.0;
pub(crate) const DEFAULT_ANIMATION_EASING: &str = "ease";
const EXIT_ANIMATION_BUFFER_MS: u32 = 40;
const CUBIC_BEZIER_SOLVE_EPSILON: f64 = 1e-6;

pub(crate) fn animation_duration_ms(duration_ms: f64) -> u32 {
    if duration_ms.is_finite() && duration_ms >= 0.0 {
        duration_ms.round().min(u32::MAX as f64) as u32
    } else {
        DEFAULT_ANIMATION_DURATION_MS as u32
    }
}

pub(crate) fn exit_animation_delay_ms(duration_ms: f64) -> u32 {
    animation_duration_ms(duration_ms).saturating_add(EXIT_ANIMATION_BUFFER_MS)
}

pub(crate) fn animation_style(duration_ms: f64, easing: &str) -> String {
    let duration_ms = animation_duration_ms(duration_ms);
    let easing = easing.trim();
    let easing = if easing.is_empty() {
        DEFAULT_ANIMATION_EASING
    } else {
        easing
    };

    format!(
        "--pine-chart-animation-duration: {duration_ms}ms; --pine-chart-animation-easing: {easing};"
    )
}

pub(crate) fn easing_progress(progress: f64, easing: &str) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    let easing = easing.trim();
    match easing {
        "linear" => progress,
        "ease" => cubic_bezier_progress(progress, 0.25, 0.1, 0.25, 1.0),
        "ease-in" => cubic_bezier_progress(progress, 0.42, 0.0, 1.0, 1.0),
        "ease-out" => cubic_bezier_progress(progress, 0.0, 0.0, 0.58, 1.0),
        "ease-in-out" => cubic_bezier_progress(progress, 0.42, 0.0, 0.58, 1.0),
        _ => parse_cubic_bezier(easing)
            .map(|[x1, y1, x2, y2]| cubic_bezier_progress(progress, x1, y1, x2, y2))
            .unwrap_or_else(|| cubic_bezier_progress(progress, 0.25, 0.1, 0.25, 1.0)),
    }
}

fn parse_cubic_bezier(easing: &str) -> Option<[f64; 4]> {
    let inner = easing
        .strip_prefix("cubic-bezier(")?
        .strip_suffix(')')?
        .trim();
    let mut values = [0.0; 4];
    let mut count = 0;
    for part in inner.split(',') {
        if count == values.len() {
            return None;
        }
        let value = part.trim().parse::<f64>().ok()?;
        if !value.is_finite() {
            return None;
        }
        values[count] = value;
        count += 1;
    }
    if count != values.len()
        || !(0.0..=1.0).contains(&values[0])
        || !(0.0..=1.0).contains(&values[2])
    {
        return None;
    }
    Some(values)
}

fn cubic_bezier_progress(progress: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    if progress <= 0.0 {
        return 0.0;
    }
    if progress >= 1.0 {
        return 1.0;
    }

    let t = solve_cubic_bezier_t(progress, x1, x2);
    cubic_bezier_sample(t, y1, y2).clamp(0.0, 1.0)
}

fn solve_cubic_bezier_t(progress: f64, x1: f64, x2: f64) -> f64 {
    let mut t = progress;
    for _ in 0..8 {
        let x = cubic_bezier_sample(t, x1, x2) - progress;
        if x.abs() <= CUBIC_BEZIER_SOLVE_EPSILON {
            return t.clamp(0.0, 1.0);
        }
        let derivative = cubic_bezier_derivative(t, x1, x2);
        if derivative.abs() < CUBIC_BEZIER_SOLVE_EPSILON {
            break;
        }
        let next = t - x / derivative;
        if !(0.0..=1.0).contains(&next) {
            break;
        }
        t = next;
    }

    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..20 {
        t = (lo + hi) * 0.5;
        let x = cubic_bezier_sample(t, x1, x2);
        if (x - progress).abs() <= CUBIC_BEZIER_SOLVE_EPSILON {
            break;
        }
        if x < progress {
            lo = t;
        } else {
            hi = t;
        }
    }
    t
}

fn cubic_bezier_sample(t: f64, p1: f64, p2: f64) -> f64 {
    let one_minus_t = 1.0 - t;
    3.0 * one_minus_t * one_minus_t * t * p1 + 3.0 * one_minus_t * t * t * p2 + t * t * t
}

fn cubic_bezier_derivative(t: f64, p1: f64, p2: f64) -> f64 {
    let one_minus_t = 1.0 - t;
    3.0 * one_minus_t * one_minus_t * p1
        + 6.0 * one_minus_t * t * (p2 - p1)
        + 3.0 * t * t * (1.0 - p2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_style_formats_css_variables() {
        assert_eq!(
            animation_style(120.0, "ease-out"),
            "--pine-chart-animation-duration: 120ms; --pine-chart-animation-easing: ease-out;"
        );
    }

    #[test]
    fn animation_style_uses_defaults_for_invalid_values() {
        assert_eq!(
            animation_style(f64::NAN, "  "),
            "--pine-chart-animation-duration: 160ms; --pine-chart-animation-easing: ease;"
        );
    }

    #[test]
    fn exit_animation_delay_adds_cleanup_buffer() {
        assert_eq!(exit_animation_delay_ms(120.0), 160);
        assert_eq!(exit_animation_delay_ms(f64::NAN), 200);
    }

    #[test]
    fn easing_progress_matches_css_named_curves() {
        assert_eq!(easing_progress(0.5, "linear"), 0.5);
        assert!(easing_progress(0.5, "ease-in") < 0.35);
        assert!(easing_progress(0.5, "ease-out") > 0.65);
        assert!((easing_progress(0.5, "ease-in-out") - 0.5).abs() <= 0.001);
    }

    #[test]
    fn easing_progress_parses_css_cubic_bezier() {
        let progress = easing_progress(0.5, "cubic-bezier(0, 0, 0.58, 1)");

        assert!((progress - easing_progress(0.5, "ease-out")).abs() <= 0.001);
    }

    #[test]
    fn easing_progress_falls_back_to_css_ease() {
        assert_eq!(
            easing_progress(0.5, "not-an-easing"),
            easing_progress(0.5, "ease")
        );
    }
}
