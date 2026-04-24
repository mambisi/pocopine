//! `<pine-time-range-field>` — two paired `<pine-time-field>`s.
//! Same shape as [`crate::date_range_field`] but for time-of-day.

use pocopine::prelude::*;
use pocopine::{current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, KeyboardEvent};

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

    pub fn on_key(&mut self, ev: KeyboardEvent) {
        if ev.key() != "Tab" {
            return;
        }
        let Some(target) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        let part = target.get_attribute("data-part");
        let in_start = target
            .closest("pine-time-field.pine-time-range-field-start")
            .ok()
            .flatten()
            .is_some();
        let in_end = target
            .closest("pine-time-field.pine-time-range-field-end")
            .ok()
            .flatten()
            .is_some();

        if !ev.shift_key() && in_start && self.is_last_start_part(part.as_deref()) {
            ev.prevent_default();
            self.focus_field_segment("end-field", "[data-part=\"hour\"]");
        } else if ev.shift_key() && in_end && part.as_deref() == Some("hour") {
            ev.prevent_default();
            let selector = if self.field_has_seconds("start-field") {
                "[data-part=\"second\"]"
            } else {
                "[data-part=\"minute\"]"
            };
            self.focus_field_segment("start-field", selector);
        }
    }
}

impl PineTimeRangeField {
    fn field_has_seconds(&self, field_ref: &str) -> bool {
        let Some(scope) = current_scope_id() else {
            return false;
        };
        let Some(field) = refs::get_on(scope, field_ref) else {
            return false;
        };
        field
            .query_selector("[data-part=\"second\"]")
            .ok()
            .flatten()
            .is_some()
    }

    fn is_last_start_part(&self, part: Option<&str>) -> bool {
        match part {
            Some("second") => true,
            Some("minute") => !self.field_has_seconds("start-field"),
            _ => false,
        }
    }

    fn focus_field_segment(&self, field_ref: &str, selector: &str) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(field) = refs::get_on(scope, field_ref) else {
            return;
        };
        let Some(segment) = field.query_selector(selector).ok().flatten() else {
            return;
        };
        if let Ok(html) = segment.dyn_into::<HtmlElement>() {
            let _ = html.focus();
        }
    }

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
