use pine::datetime::DateValue;
use pine::{
    PineRangeCalendarGrid, PineRangeCalendarGridBody, PineRangeCalendarGridHead,
    PineRangeCalendarHeader, PineRangeCalendarHeading, PineRangeCalendarNext,
    PineRangeCalendarPrev, PineRangeCalendarRoot,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "RangeCalendarDemo.poco",
    style = "range_calendar.css",
    role = "panel",
    // RFC 049 — Mirrors Calendar: Root → Header + Grid;
    // Header takes Prev/Heading/Next; Grid nests Head + Body.
    uses = [
        PineRangeCalendarRoot,
        PineRangeCalendarHeader,
        PineRangeCalendarPrev,
        PineRangeCalendarHeading,
        PineRangeCalendarNext,
        PineRangeCalendarGrid,
        PineRangeCalendarGridHead,
        PineRangeCalendarGridBody,
    ]
)]
pub struct RangeCalendarDemo {
    pub start: Option<DateValue>,
    pub end: Option<DateValue>,
    pub placeholder: Option<DateValue>,
}

#[handlers]
impl RangeCalendarDemo {
    pub fn on_mount(&mut self) {
        if self.placeholder.is_none() {
            self.placeholder = DateValue::parse_iso("2024-06-15");
        }
    }

    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
    }
}
