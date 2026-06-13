//! Range-selection calendar — port of reka-ui's `<RangeCalendar*>`
//! family, layered on top of the single-date [`crate::calendar`]
//! primitives.
//!
//! Shares the grid math with `calendar::state::CalendarState` via
//! composition — see [`state::RangeCalendarState`]. The component
//! surface mirrors `crate::calendar`: Root + Grid family + Cell
//! family + Header / Nav, all wired through a fresh context key
//! (`RANGE_ROOT`) so the single and range calendars can co-exist
//! on the same page without stepping on each other.

pub mod cell;
pub mod grid;
pub mod header;
pub mod root;
pub mod state;

pub use cell::{PineRangeCalendarCell, PineRangeCalendarCellTrigger};
pub use grid::{PineRangeCalendarGrid, PineRangeCalendarGridBody, PineRangeCalendarGridHead};
pub use header::{
    PineRangeCalendarHeader, PineRangeCalendarHeading, PineRangeCalendarNext, PineRangeCalendarPrev,
};
pub use root::{PineRangeCalendarRoot, RANGE_ROOT, RangeCellView};
pub use state::RangeCalendarState;
