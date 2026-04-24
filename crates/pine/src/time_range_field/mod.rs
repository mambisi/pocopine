//! `<pine-time-range-field>` — two paired `<pine-time-field>`s.
//! Same shape as [`crate::date_range_field`] but for time-of-day.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineTimeRangeField.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineTimeRangeField {
    #[model]
    pub start: String,
    #[model]
    pub end: String,
    #[prop]
    pub min_value: String,
    #[prop]
    pub max_value: String,
    #[prop]
    pub step: f64,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub readonly: bool,
    #[prop]
    pub required: bool,
    #[prop]
    pub start_name: String,
    #[prop]
    pub end_name: String,
    #[prop]
    pub separator: String,
}

#[handlers]
impl PineTimeRangeField {
    pub fn on_mount(&mut self) {
        if self.separator.is_empty() {
            self.separator = " – ".into();
        }
    }
}

impl PineTimeRangeField {
    /// End-time min: prefer `start` when it's later than the
    /// author's `min_value`. Simple lexicographic compare works
    /// for `"HH:MM"` / `"HH:MM:SS"` strings.
    pub fn effective_end_min(&self) -> String {
        match (self.min_value.as_str(), self.start.as_str()) {
            ("", "") => String::new(),
            (m, "") => m.to_string(),
            ("", s) => s.to_string(),
            (m, s) => {
                if s > m {
                    s.to_string()
                } else {
                    m.to_string()
                }
            }
        }
    }

    pub fn effective_start_max(&self) -> String {
        match (self.max_value.as_str(), self.end.as_str()) {
            ("", "") => String::new(),
            (m, "") => m.to_string(),
            ("", e) => e.to_string(),
            (m, e) => {
                if e < m {
                    e.to_string()
                } else {
                    m.to_string()
                }
            }
        }
    }
}
