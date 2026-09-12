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

use crate::datetime::DateValue;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineDateRangePicker.poco",
    role = "interactive",
    display = "contents",
    uses = [
        crate::popover::PinePopoverRoot,
        crate::popover::PinePopoverTrigger,
        crate::popover::PinePopoverPortal,
        crate::popover::PinePopoverContent,
        crate::range_calendar::PineRangeCalendarRoot,
        crate::range_calendar::PineRangeCalendarHeader,
        crate::range_calendar::PineRangeCalendarPrev,
        crate::range_calendar::PineRangeCalendarHeading,
        crate::range_calendar::PineRangeCalendarNext,
        crate::range_calendar::PineRangeCalendarGrid,
        crate::range_calendar::PineRangeCalendarGridHead,
        crate::range_calendar::PineRangeCalendarGridBody,
    ],
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
    fn on_mount(&mut self) {
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
    #[watch(start, end, close_on_select)]
    fn on_range_change(
        start: Option<DateValue>,
        end: Change<Option<DateValue>>,
        close_on_select: bool,
    ) -> Update<Self, (Self::Open,)> {
        if close_on_select && start.is_some() && end.current.is_some() && end.changed() {
            Update::new().open(false)
        } else {
            Update::new()
        }
    }

    #[watch(start, end, placeholder_text, separator)]
    fn on_label_change(
        start: Option<DateValue>,
        end: Option<DateValue>,
        placeholder_text: &str,
        separator: &str,
    ) -> Update<Self, (Self::DisplayLabel,)> {
        Update::new().display_label(range_label(start, end, placeholder_text, separator))
    }
}

impl PineDateRangePicker {
    fn recompute_label(&mut self) {
        // Rendered via `pp-text` bound to `display_label` on the
        // trigger below; keeps the two branches (empty / one-
        // endpoint / full range) in one place.
        self.display_label = range_label(
            self.start,
            self.end,
            &self.placeholder_text,
            &self.separator,
        );
    }
}

fn range_label(
    start: Option<DateValue>,
    end: Option<DateValue>,
    placeholder_text: &str,
    separator: &str,
) -> String {
    match (start, end) {
        (None, None) => placeholder_text.to_owned(),
        (Some(start), None) => format!("{start}{separator}…"),
        (None, Some(end)) => format!("…{separator}{end}"),
        (Some(start), Some(end)) => format!("{start}{separator}{end}"),
    }
}
