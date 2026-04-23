//! `<pine-date-range-picker>` — drop-in popover + range calendar.
//!
//! Same shape as [`crate::date_picker::PineDatePicker`] but tracks
//! a start/end pair and closes only after both endpoints commit.
//! Authors who need custom trigger or content layout should fall
//! back to hand-wiring `<pine-popover-root>` around
//! `<pine-range-calendar-root>`.
//!
//! Props:
//! - `start`, `end` — two-way `Option<DateValue>` endpoints. ISO
//!   strings in templates still deserialize into them. Bind both
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

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use crate::datetime::DateValue;

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineDateRangePicker.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineDateRangePicker {
    #[model]
    pub start: Option<DateValue>,
    #[model]
    pub end: Option<DateValue>,
    #[model]
    pub placeholder: Option<DateValue>,
    #[prop]
    pub placeholder_text: String,
    #[prop]
    pub separator: String,
    #[prop]
    pub min_value: Option<DateValue>,
    #[prop]
    pub max_value: Option<DateValue>,
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
            start: None,
            end: None,
            placeholder: None,
            placeholder_text: "Pick a date range".into(),
            separator: " – ".into(),
            min_value: None,
            max_value: None,
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

    /// Close-on-select only fires when a **complete range**
    /// commits — the popover stays open after the first click
    /// (which sets `start` and leaves `end` empty) and only
    /// closes once the second click lands a non-empty `end` on
    /// top of a non-empty `start`.
    ///
    /// The `self.open` guard keeps initial pp-model flow from
    /// tripping the close on mount, e.g. when the author seeds
    /// `start` / `end` with defaults — the watcher fires once
    /// but `open` is still `false`, so it's a no-op.
    #[watch(end)]
    fn on_end_change(&mut self, new: Option<DateValue>, prev: Option<Option<DateValue>>) {
        self.recompute_label();
        if !self.close_on_select || !self.open {
            return;
        }
        if self.start.is_none() || new.is_none() {
            return;
        }
        if prev == Some(new) {
            return;
        }
        self.open = false;
    }

    #[watch(start)]
    fn on_start_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.recompute_label();
    }
}

impl PineDateRangePicker {
    fn recompute_label(&mut self) {
        // Rendered via `pp-text` bound to `display_label` on the
        // trigger below; keeps the two branches (empty / one-
        // endpoint / full range) in one place.
        self.display_label = match (self.start, self.end) {
            (None, None) => self.placeholder_text.clone(),
            (Some(s), None) => format!("{}{}…", s, self.separator),
            (None, Some(e)) => format!("…{}{}", self.separator, e),
            (Some(s), Some(e)) => format!("{}{}{}", s, self.separator, e),
        };
    }
}
