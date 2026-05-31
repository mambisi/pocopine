//! `<file-browser-size-control>` — CFE size input with a range handle.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

const MIB: f64 = 1024.0 * 1024.0;

#[derive(Serialize, Deserialize)]
#[component(
    template = "FileBrowserSizeControl.poco",
    role = "panel",
    display = "block"
)]
pub struct FileBrowserSizeControl {
    #[model]
    pub value: f64,
    #[prop]
    pub label: String,
    #[prop]
    pub min: f64,
    #[prop]
    pub max: f64,
    #[prop]
    pub disabled: bool,
    pub amount: f64,
    pub unit: String,
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

impl Default for FileBrowserSizeControl {
    fn default() -> Self {
        Self {
            value: 1.0,
            label: String::new(),
            min: 1.0,
            max: 1024.0,
            disabled: false,
            amount: 1.0,
            unit: "MB".to_string(),
            amount_min: 1.0,
            amount_max: 1024.0,
            amount_step: "1".to_string(),
            range_min: 1.0,
            range_max: 1024.0,
            range_style: "--cfe-size-fraction: 0;".to_string(),
            value_label: "1.0 MB".to_string(),
            min_label: "1.0 MB".to_string(),
            max_label: "1.0 GB max".to_string(),
            gb_disabled: false,
        }
    }
}

impl FileBrowserSizeControl {
    fn sync_from_value(&mut self) {
        let (min, max) = self.bounds();
        self.range_min = min;
        self.range_max = max;
        self.gb_disabled = max < 1024.0;
        if self.unit != "GB" || self.gb_disabled {
            self.unit = "MB".to_string();
        }

        let value = clamp_mib(self.value, min, max);
        if changed(self.value, value) {
            self.value = value;
        }

        let factor = unit_factor(&self.unit);
        self.amount = format_amount_value(value / factor, &self.unit);
        self.amount_min = format_amount_value(min / factor, &self.unit);
        self.amount_max = format_amount_value(max / factor, &self.unit);
        self.amount_step = if self.unit == "GB" { "0.01" } else { "1" }.to_string();
        self.value_label = format_mib_label(value);
        self.min_label = format_mib_label(min);
        self.max_label = format!("{} max", format_mib_label(max));

        let span = (max - min).max(f64::EPSILON);
        let fraction = ((value - min) / span).clamp(0.0, 1.0);
        self.range_style = format!("--cfe-size-fraction: {fraction:.6};");
    }

    fn commit_amount(&mut self) {
        let (min, max) = self.bounds();
        if self.gb_disabled {
            self.unit = "MB".to_string();
        }
        let amount = if self.amount.is_finite() {
            self.amount
        } else {
            min / unit_factor(&self.unit)
        };
        let next = amount * unit_factor(&self.unit);
        let value = clamp_mib(next, min, max);
        if changed(self.value, value) {
            self.value = value;
        }
        self.sync_from_value();
    }

    fn bounds(&self) -> (f64, f64) {
        let min = mib_value(self.min.max(1.0));
        let max = mib_value(self.max.max(min));
        (min, max)
    }
}

#[handlers]
impl FileBrowserSizeControl {
    fn on_setup(&mut self) {
        self.sync_from_value();
    }

    #[watch(value)]
    fn on_value_change(&mut self, _next: f64, _prev: Option<f64>) {
        self.sync_from_value();
    }

    #[watch(min)]
    fn on_min_change(&mut self, _next: f64, _prev: Option<f64>) {
        self.sync_from_value();
    }

    #[watch(max)]
    fn on_max_change(&mut self, _next: f64, _prev: Option<f64>) {
        self.sync_from_value();
    }

    #[watch(amount)]
    fn on_amount_change(&mut self, _next: f64, _prev: Option<f64>) {
        self.commit_amount();
    }

    #[watch(unit)]
    fn on_unit_change(&mut self, _next: String, _prev: Option<String>) {
        self.commit_amount();
    }
}

fn unit_factor(unit: &str) -> f64 {
    if unit == "GB" {
        1024.0
    } else {
        1.0
    }
}

fn mib_value(value: f64) -> f64 {
    value.round().clamp(1.0, 1024.0)
}

fn clamp_mib(value: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() {
        mib_value(value).clamp(min, max)
    } else {
        min
    }
}

fn changed(left: f64, right: f64) -> bool {
    (left - right).abs() > f64::EPSILON
}

fn format_amount_value(value: f64, unit: &str) -> f64 {
    if unit == "GB" {
        (value * 1000.0).round() / 1000.0
    } else {
        value.round()
    }
}

fn format_mib_label(value: f64) -> String {
    crate::format_size((mib_value(value) * MIB) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_and_unit_commit_to_mib_value() {
        let mut control = FileBrowserSizeControl {
            amount: 0.5,
            unit: "GB".to_string(),
            ..Default::default()
        };

        control.commit_amount();

        assert_eq!(control.value, 512.0);
        assert_eq!(control.value_label, "512 MB");
    }

    #[test]
    fn small_max_disables_gb_and_clamps_value() {
        let mut control = FileBrowserSizeControl {
            value: 128.0,
            max: 64.0,
            unit: "GB".to_string(),
            ..Default::default()
        };

        control.sync_from_value();

        assert_eq!(control.value, 64.0);
        assert_eq!(control.unit, "MB");
        assert!(control.gb_disabled);
        assert_eq!(control.amount, 64.0);
    }
}
