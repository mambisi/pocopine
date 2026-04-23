//! `<pine-date-range-picker>` — drop-in popover + range calendar.
//!
//! Same shape as [`crate::date_picker::PineDatePicker`] but tracks
//! a start/end pair and closes only after both endpoints commit.
//! Authors who need custom trigger or content layout should fall
//! back to hand-wiring `<pine-popover-root>` around
//! `<pine-range-calendar-root>`.
//!
//! Props:
//! - `start`, `end` — two-way ISO `YYYY-MM-DD` dates. Bind both
//!   on the same element with `pp-model:start="…"` +
//!   `pp-model:end="…"`; updates flow through the per-field
//!   `pp:update:start` / `pp:update:end` channels.
//! - `placeholder` — visible-month anchor.
//! - `placeholder_text` — trigger label when both endpoints are
//!   empty. Defaults to `"Pick a date range"`.
//! - `separator` — glyph shown between the two dates on the
//!   trigger. Defaults to `" – "` (en dash + surrounding spaces).
//! - `min_value` / `max_value`, `week_starts_on`, `fixed_weeks`,
//!   `disabled`, `readonly` — forwarded to the inner range calendar.
//! - `close_on_select` — close after `end` commits. `true` by
//!   default.

use pocopine::emit_model_field;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineDateRangePicker.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineDateRangePicker {
    #[prop]
    pub start: String,
    #[prop]
    pub end: String,
    #[prop]
    pub placeholder: String,
    #[prop]
    pub placeholder_text: String,
    #[prop]
    pub separator: String,
    #[prop]
    pub min_value: String,
    #[prop]
    pub max_value: String,
    #[prop]
    pub week_starts_on: u32,
    #[prop]
    pub fixed_weeks: bool,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub readonly: bool,
    #[prop]
    pub close_on_select: bool,

    pub open: bool,
    /// Rendered trigger text — recomputed from `start` / `end` /
    /// `placeholder_text` / `separator` in `recompute_label`.
    pub display_label: String,
}

impl Default for PineDateRangePicker {
    fn default() -> Self {
        Self {
            start: String::new(),
            end: String::new(),
            placeholder: String::new(),
            placeholder_text: "Pick a date range".into(),
            separator: " – ".into(),
            min_value: String::new(),
            max_value: String::new(),
            week_starts_on: 0,
            fixed_weeks: false,
            disabled: false,
            readonly: false,
            close_on_select: true,
            open: false,
            display_label: String::new(),
        }
    }
}

#[handlers]
impl PineDateRangePicker {
    pub fn on_mount(&mut self) {
        if self.placeholder_text.is_empty() {
            self.placeholder_text = "Pick a date range".into();
        }
        if self.separator.is_empty() {
            self.separator = " – ".into();
        }
        self.recompute_label();
    }

    /// Close-on-select only fires once `end` commits — picking
    /// `start` alone keeps the popover open so the user can
    /// continue clicking through to the second endpoint.
    #[watch(end)]
    fn on_end_change(&mut self, new: String, prev: Option<String>) {
        self.recompute_label();
        // Re-emit so the author's `pp-model:end="…"` sees the
        // change even when it originated inside the inner range
        // calendar (which writes the prop directly via pp-model).
        emit_model_field("end", new.clone());
        if !self.close_on_select {
            return;
        }
        if new.is_empty() {
            return;
        }
        if prev.as_deref() == Some(new.as_str()) {
            return;
        }
        self.open = false;
    }

    #[watch(start)]
    fn on_start_change(&mut self, new: String, _prev: Option<String>) {
        self.recompute_label();
        emit_model_field("start", new);
    }
}

impl PineDateRangePicker {
    fn recompute_label(&mut self) {
        // Rendered via `pp-text` bound to `display_label` on the
        // trigger below; keeps the two branches (empty / one-
        // endpoint / full range) in one place.
        self.display_label = match (self.start.as_str(), self.end.as_str()) {
            ("", "") => self.placeholder_text.clone(),
            (s, "") => format!("{s}{}…", self.separator),
            ("", e) => format!("…{}{e}", self.separator),
            (s, e) => format!("{s}{}{e}", self.separator),
        };
    }
}
