//! Range-calendar header + nav primitives. Identical shape to the
//! single-date Calendar versions, but observing `RANGE_ROOT`
//! instead of `ROOT`.

use crate::range_calendar::root::{PineRangeCalendarRoot, RANGE_ROOT};
use pocopine::inject;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarHeader.poco",
    role = "panel",
    display = "contents"
)]
// RFC 049 — mirror of PineCalendarHeader: exactly Prev + Heading
// + Next for month navigation.
#[slot(default, only = [
    PineRangeCalendarPrev,
    PineRangeCalendarHeading,
    PineRangeCalendarNext,
])]
pub struct PineRangeCalendarHeader {}

#[handlers]
impl PineRangeCalendarHeader {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarHeading.poco",
    role = "heading",
    display = "contents"
)]
pub struct PineRangeCalendarHeading {
    #[observe(RANGE_ROOT)]
    pub heading: String,
    #[observe(RANGE_ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineRangeCalendarHeading {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarPrev.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineRangeCalendarPrev {
    #[observe(RANGE_ROOT)]
    pub prev_disabled: bool,
    #[observe(RANGE_ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineRangeCalendarPrev {
    pub fn on_click(&mut self) {
        if let Some(root) = inject(&RANGE_ROOT) {
            root.update(|r: &mut PineRangeCalendarRoot| r.prev_page());
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarNext.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineRangeCalendarNext {
    #[observe(RANGE_ROOT)]
    pub next_disabled: bool,
    #[observe(RANGE_ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineRangeCalendarNext {
    pub fn on_click(&mut self) {
        if let Some(root) = inject(&RANGE_ROOT) {
            root.update(|r: &mut PineRangeCalendarRoot| r.next_page());
        }
    }
}
