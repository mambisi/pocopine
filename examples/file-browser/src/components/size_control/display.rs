//! Pure size-control projection and input normalization (values are in MiB).

use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SizeDisplay {
    pub value: f64,
    pub unit: String,
    pub amount: f64,
    pub amount_min: f64,
    pub amount_max: f64,
    pub amount_step: String,
    pub range_min: f64,
    pub range_max: f64,
    pub range_style: String,
    pub value_label: String,
    pub min_label: String,
    pub max_label: String,
    pub gb_disabled: bool,
}

impl SizeDisplay {
    pub fn new(value: f64, min: f64, max: f64, unit: &str) -> Self {
        let min = clamp_mib(min, 1.0, 1024.0);
        let max = clamp_mib(max, min, 1024.0);
        let value = clamp_mib(value, min, max);
        let gb_disabled = max < 1024.0;
        let unit = if unit == "GB" && !gb_disabled {
            "GB"
        } else {
            "MB"
        };
        let factor = unit_factor(unit);
        let amount = |value: f64| {
            let value = value / factor;
            if unit == "GB" {
                (value * 1000.0).round() / 1000.0
            } else {
                value.round()
            }
        };
        let fraction = (value - min) / (max - min).max(f64::EPSILON);
        Self {
            value,
            unit: unit.to_string(),
            amount: amount(value),
            amount_min: amount(min),
            amount_max: amount(max),
            amount_step: if unit == "GB" { "0.01" } else { "1" }.to_string(),
            range_min: min,
            range_max: max,
            range_style: format!("--cfe-size-fraction: {fraction:.6};"),
            value_label: format_mib(value),
            min_label: format_mib(min),
            max_label: format!("{} max", format_mib(max)),
            gb_disabled,
        }
    }

    pub fn accept_range(&self, value: f64) -> f64 {
        clamp_mib(value, self.range_min, self.range_max)
    }

    pub fn accept_amount(&self, amount: f64) -> f64 {
        self.accept_range(amount * unit_factor(&self.unit))
    }

    /// Changing units interprets the displayed amount in the selected unit.
    pub fn accept_unit(&self, unit: &str) -> (f64, String) {
        let unit = if unit == "GB" && !self.gb_disabled {
            "GB"
        } else {
            "MB"
        };
        (
            self.accept_range(self.amount * unit_factor(unit)),
            unit.to_string(),
        )
    }
}

fn unit_factor(unit: &str) -> f64 {
    if unit == "GB" { 1024.0 } else { 1.0 }
}

fn clamp_mib(value: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() {
        value.round().clamp(min, max)
    } else {
        min
    }
}

fn format_mib(value: f64) -> String {
    if value == 1024.0 {
        "1.0 GB".to_string()
    } else if value >= 10.0 {
        format!("{value:.0} MB")
    } else {
        format!("{value:.1} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_gb_input_commits_mib() {
        let display = SizeDisplay::new(1024.0, 1.0, 1024.0, "GB");
        assert_eq!(display.accept_amount(0.5), 512.0);
        assert_eq!(
            SizeDisplay::new(512.0, 1.0, 1024.0, "GB").value_label,
            "512 MB"
        );
        assert_eq!(display.accept_amount(2.0), 1024.0);
    }

    #[test]
    fn smaller_bounds_derive_a_clamped_mb_display_without_an_input_commit() {
        let display = SizeDisplay::new(128.0, 1.0, 64.0, "GB");
        assert_eq!(display.value, 64.0);
        assert_eq!(display.amount, 64.0);
        assert_eq!(display.unit, "MB");
        assert!(display.gb_disabled);
        assert_eq!(display.range_style, "--cfe-size-fraction: 1.000000;");
        assert_eq!(display.accept_unit("GB"), (64.0, "MB".into()));
    }

    #[test]
    fn changing_units_preserves_numeric_input_semantics() {
        let display = SizeDisplay::new(1.0, 1.0, 1024.0, "MB");
        assert_eq!(display.accept_unit("GB"), (1024.0, "GB".into()));
        let display = SizeDisplay::new(512.0, 1.0, 1024.0, "GB");
        assert_eq!(display.amount, 0.5);
        assert_eq!(display.accept_unit("MB"), (1.0, "MB".into()));
    }

    #[test]
    fn invalid_inputs_and_degenerate_bounds_stay_finite() {
        let display = SizeDisplay::new(f64::NAN, 64.0, 2.0, "GB");
        assert_eq!(
            (display.range_min, display.range_max, display.value),
            (64.0, 64.0, 64.0)
        );
        assert_eq!(display.range_style, "--cfe-size-fraction: 0.000000;");
        assert_eq!(display.accept_amount(f64::NAN), 64.0);
        assert_eq!(display.accept_range(f64::INFINITY), 64.0);
        let display = SizeDisplay::new(2.4, f64::NAN, f64::NAN, "invalid");
        assert_eq!(
            (display.range_min, display.range_max, display.value),
            (1.0, 1.0, 1.0)
        );
    }
}
