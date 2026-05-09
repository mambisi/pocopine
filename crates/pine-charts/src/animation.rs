pub(crate) const DEFAULT_ANIMATION_DURATION_MS: f64 = 160.0;
pub(crate) const DEFAULT_ANIMATION_EASING: &str = "ease";
const EXIT_ANIMATION_BUFFER_MS: u32 = 40;

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

    format!("--pine-chart-animation-duration: {duration_ms}ms; --pine-chart-animation-easing: {easing};")
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
}
