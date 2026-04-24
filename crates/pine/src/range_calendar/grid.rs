//! `<pine-range-calendar-grid*>` — grid shell for the range
//! calendar. Same shape as `pine/src/calendar/grid.rs`, but the
//! body observes `RANGE_ROOT.cells` (which carry the range-specific
//! flags) rather than the plain Calendar's cells.

use crate::range_calendar::root::{RangeCellView, RANGE_ROOT};
use pocopine::inject;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarGrid.poco",
    role = "list",
    display = "contents"
)]
// RFC 049 — mirror of PineCalendarGrid: GridHead + GridBody.
#[slot(default, only = [PineRangeCalendarGridHead, PineRangeCalendarGridBody])]
pub struct PineRangeCalendarGrid {
    #[observe(RANGE_ROOT)]
    pub disabled: bool,
    #[observe(RANGE_ROOT)]
    pub readonly: bool,
}

#[handlers]
impl PineRangeCalendarGrid {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarGridHead.poco",
    role = "list",
    display = "contents"
)]
pub struct PineRangeCalendarGridHead {
    #[observe(RANGE_ROOT)]
    pub weekdays: Vec<String>,
}

#[handlers]
impl PineRangeCalendarGridHead {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarGridBody.poco",
    role = "list",
    display = "contents"
)]
pub struct PineRangeCalendarGridBody {
    #[observe(RANGE_ROOT)]
    pub cells: Vec<RangeCellView>,
}

#[handlers]
impl PineRangeCalendarGridBody {
    /// `@mouseleave` on the grid body — clears the Root's
    /// `focused` cursor so the range-preview tint stops painting
    /// once the pointer leaves every cell. Per-cell `mouseleave`
    /// would flicker across cell boundaries; one listener on the
    /// whole grid body only fires when the pointer actually exits
    /// the calendar.
    pub fn clear_hover(&mut self) {
        if let Some(root) = inject(&RANGE_ROOT) {
            root.update(
                |r: &mut crate::range_calendar::root::PineRangeCalendarRoot| {
                    r.clear_hover();
                },
            );
        }
    }
}
