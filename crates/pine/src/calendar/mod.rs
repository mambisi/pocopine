//! Calendar primitive — port of reka-ui's `<Calendar*>` component
//! family built on the [`crate::datetime`] engine.
//!
//! The module is split along reka's own seams: a pure [`state`]
//! machine that owns the grid / placeholder / selection data plus
//! its navigation transitions, then (in follow-up commits) the
//! `#[component]` parts that wrap the state and render the
//! accessibility shell: `Root`, `Header`, `Heading`, `Prev`,
//! `Next`, `Grid` + `GridHead` / `GridBody` / `GridRow`,
//! `HeadCell`, `Cell`, `CellTrigger`.

pub mod cell;
pub mod grid;
pub mod header;
pub mod root;
pub mod state;

pub use cell::{PineCalendarCell, PineCalendarCellTrigger};
pub use grid::{
    PineCalendarGrid, PineCalendarGridBody, PineCalendarGridHead, PineCalendarGridRow,
    PineCalendarHeadCell,
};
pub use header::{PineCalendarHeader, PineCalendarHeading, PineCalendarNext, PineCalendarPrev};
pub use root::{CalendarCellView, CalendarMonthView, PineCalendarRoot, ROOT};
pub use state::CalendarState;
