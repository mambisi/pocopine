//! Range-selection calendar — port of reka-ui's `<RangeCalendar*>`
//! family, layered on top of the single-date [`crate::calendar`]
//! primitives.
//!
//! Only the root, cell trigger, and the `cells` view model differ
//! from the plain Calendar — navigation, grid math, and the
//! weekday header are identical. The module is split along the
//! same seams as `calendar/`: pure [`state`] machine, then the
//! `#[component]` parts (landing in follow-up commits).

pub mod root;
pub mod state;

pub use root::{PineRangeCalendarRoot, RangeCellView, RANGE_ROOT};
pub use state::RangeCalendarState;
