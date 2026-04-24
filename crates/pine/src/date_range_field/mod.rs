//! `<pine-date-range-field>` — two paired `<pine-date-field>`s for
//! a `start` / `end` pair. The outer primitive owns the pair
//! state + a small bounds-aware mirror (writing `start` also
//! updates the inner end-field's `min`, and vice versa) so a
//! picked range can't invert on itself.
//!
//! Composes `<pine-date-field>` twice rather than re-implementing
//! segment editing — native `<input type="date">` already handles
//! the keyboard side.

use crate::datetime::DateValue;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineDateRangeField.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineDateRangeField {
    #[model]
    pub start: Option<DateValue>,
    #[model]
    pub end: Option<DateValue>,
    /// Lower bound propagated to **both** inner fields as their
    /// native `min`. The end-field's effective min is clamped
    /// further up to the current `start` (see `effective_end_min`).
    #[prop]
    pub min_value: Option<DateValue>,
    /// Upper bound propagated to **both** inner fields as their
    /// native `max`. The start-field's effective max is clamped
    /// down to the current `end` (see `effective_start_max`).
    #[prop]
    pub max_value: Option<DateValue>,
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
impl PineDateRangeField {
    pub fn on_mount(&mut self) {
        if self.separator.is_empty() {
            self.separator = " – ".into();
        }
    }
}

impl PineDateRangeField {
    /// End-field min: the greater of the author's `min_value` and
    /// the currently picked `start`. Keeps the user from picking
    /// an end that precedes start.
    pub fn effective_end_min(&self) -> Option<DateValue> {
        match (self.min_value, self.start) {
            (Some(m), Some(s)) if s > m => Some(s),
            (None, Some(s)) => Some(s),
            (m, _) => m,
        }
    }

    /// Start-field max: the lesser of the author's `max_value`
    /// and the currently picked `end`.
    pub fn effective_start_max(&self) -> Option<DateValue> {
        match (self.max_value, self.end) {
            (Some(m), Some(e)) if e < m => Some(e),
            (None, Some(e)) => Some(e),
            (m, _) => m,
        }
    }
}
