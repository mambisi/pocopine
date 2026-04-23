//! `<pine-date-picker>` — drop-in popover + calendar combo.
//!
//! A single primitive that wraps [`PinePopoverRoot`][crate::popover::PinePopoverRoot]
//! around [`PineCalendarRoot`][crate::calendar::PineCalendarRoot] so
//! authors can stand up a conventional date picker without hand-wiring
//! the two. For layouts that need to customise the trigger, content
//! shell, or calendar internals, drop to the compositional pattern —
//! `<pine-popover-root>` wrapping `<pine-calendar-root>` directly.
//!
//! Props:
//! - `value` — two-way bindable via `pp-model:value="…"`.
//!   `Option<DateValue>` at the Rust boundary; ISO strings in
//!   templates still deserialize into it. `None` = unselected.
//! - `placeholder` — visible-month anchor. `None` = today.
//! - `placeholder_text` — label shown on the trigger when
//!   `value` is empty. Defaults to `"Pick a date"`.
//! - `min_value` / `max_value` — optional bounds forwarded to the
//!   inner calendar.
//! - `week_starts_on` (0-6), `fixed_weeks`, `disabled`, `readonly` —
//!   forwarded unchanged.
//! - `close_on_select` — close the popover after a pick. `true` by
//!   default; set to `false` to keep it open for multi-step flows.
//!
//! State hooks on the root span (`<pine-date-picker>`):
//! - `data-open` — `true` while the popover is open.
//! - `data-has-value` — `true` when `value` is set.
//! - `data-disabled` — forwarded.

use crate::datetime::DateValue;
use pocopine::emit_model_field;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineDatePicker.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineDatePicker {
    #[prop]
    pub value: Option<DateValue>,
    #[prop]
    pub placeholder: Option<DateValue>,
    #[prop]
    pub placeholder_text: String,
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

    /// Internal popover open state. Exposed for author styling via
    /// `data-open` on the host; flipped by the trigger and by the
    /// `#[watch(value)]` handler below.
    pub open: bool,
}

impl Default for PineDatePicker {
    fn default() -> Self {
        Self {
            value: None,
            placeholder: None,
            placeholder_text: "Pick a date".into(),
            min_value: None,
            max_value: None,
            week_starts_on: 0,
            fixed_weeks: false,
            disabled: false,
            readonly: false,
            close_on_select: true,
            open: false,
        }
    }
}

#[handlers]
impl PineDatePicker {
    pub fn on_mount(&mut self) {
        if self.placeholder_text.is_empty() {
            self.placeholder_text = "Pick a date".into();
        }
    }

    /// Close on real pick — ignore the initial no-op flow and the
    /// user-initiated deselect (value becomes empty again).
    #[watch(value)]
    fn on_value_change(&mut self, new: Option<DateValue>, prev: Option<Option<DateValue>>) {
        emit_model_field("value", maybe_date_string(new));
        if !self.close_on_select {
            return;
        }
        if new.is_none() {
            return;
        }
        if prev == Some(new) {
            return;
        }
        self.open = false;
    }
}

fn maybe_date_string(value: Option<DateValue>) -> String {
    value.map(|d| d.to_string()).unwrap_or_default()
}
