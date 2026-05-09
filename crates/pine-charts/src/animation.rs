use crate::svg::format_tick;

pub(crate) const DEFAULT_ANIMATION_DURATION_MS: f64 = 160.0;
pub(crate) const DEFAULT_ANIMATION_EASING: &str = "ease";

pub(crate) fn animation_style(duration_ms: f64, easing: &str) -> String {
    let duration_ms = if duration_ms.is_finite() && duration_ms >= 0.0 {
        duration_ms
    } else {
        DEFAULT_ANIMATION_DURATION_MS
    };
    let easing = easing.trim();
    let easing = if easing.is_empty() {
        DEFAULT_ANIMATION_EASING
    } else {
        easing
    };

    format!(
        "--pine-chart-animation-duration: {}ms; --pine-chart-animation-easing: {easing};",
        format_tick(duration_ms)
    )
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
}
