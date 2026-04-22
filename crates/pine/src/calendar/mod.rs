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

pub mod root;
pub mod state;

pub use root::{CalendarCellView, CalendarMonthView, PineCalendarRoot, ROOT};
pub use state::CalendarState;
