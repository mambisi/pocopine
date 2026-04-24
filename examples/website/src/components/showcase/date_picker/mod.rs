use pine::datetime::DateValue;
use pine::{
    PineCalendarGrid, PineCalendarGridBody, PineCalendarGridHead, PineCalendarHeader,
    PineCalendarHeading, PineCalendarNext, PineCalendarPrev, PineCalendarRoot, PinePopoverContent,
    PinePopoverPortal, PinePopoverRoot, PinePopoverTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Compositional `<pine-popover>` + `<pine-calendar-root>` demo.
///
/// Serves two jobs:
/// 1. Shows authors how to hand-compose a working date picker
///    out of Pine primitives they already have.
/// 2. Stress-tests the shared `value` / `placeholder` state
///    (pp-model on both sides) + close-on-select wiring.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "DatePickerDemo.poco",
    style = "date_picker.css",
    role = "panel",
    // RFC 049 — two compounds wired together.
    //
    // Popover:
    //   Root → [Trigger, Portal]
    //   Portal → [Content]
    //
    // Calendar:
    //   Root → [Header, Grid]
    //   Header → [Prev, Heading, Next]
    //   Grid → [GridHead, GridBody]
    //
    // `<pine-date-picker>` is the single-struct primitive
    // that wraps the whole compound — no typed slot of its
    // own — so it doesn't appear here. Trigger's author
    // content (a custom button) is silently skipped.
    uses = [
        PinePopoverRoot,
        PinePopoverTrigger,
        PinePopoverPortal,
        PinePopoverContent,
        PineCalendarRoot,
        PineCalendarHeader,
        PineCalendarPrev,
        PineCalendarHeading,
        PineCalendarNext,
        PineCalendarGrid,
        PineCalendarGridHead,
        PineCalendarGridBody,
    ]
)]
pub struct DatePickerDemo {
    pub value: Option<DateValue>,
    pub placeholder: Option<DateValue>,
    pub open: bool,
}

#[handlers]
impl DatePickerDemo {
    pub fn on_mount(&mut self) {
        if self.placeholder.is_none() {
            self.placeholder = DateValue::parse_iso("2024-06-15");
        }
    }

    /// Fires whenever the calendar writes back through
    /// `pp-model:value`. Used for the close-on-select behavior
    /// plus keeping the trigger label in sync.
    #[watch(value)]
    fn on_value_change(&mut self, new: Option<DateValue>, prev: Option<Option<DateValue>>) {
        // Ignore the no-op initial flow — only close when the
        // user actually picked a non-empty date.
        if new.is_none() {
            return;
        }
        if prev == Some(new) {
            return;
        }
        self.open = false;
    }

    pub fn clear(&mut self) {
        self.value = None;
    }
}
