//! `<pine-calendar-grid*>` — semantic wrappers for the month grid.
//!
//! Each sub-part is a thin primitive mapping to the equivalent
//! HTML table-family element with role / ARIA hooks per reka's
//! Vue components. They observe [`super::root::ROOT`] just for
//! the `disabled` / `readonly` state they mirror onto `data-*`.

use crate::calendar::root::ROOT;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

// ── Grid ─────────────────────────────────────────────────────────

/// `<pine-calendar-grid>` — the outermost `<table role="application">`
/// wrapping one month's worth of cells.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCalendarGrid.poco", role = "list", display = "contents")]
pub struct PineCalendarGrid {
    #[observe(ROOT)]
    pub disabled: bool,
    #[observe(ROOT)]
    pub readonly: bool,
}

#[handlers]
impl PineCalendarGrid {}

// ── Grid parts ───────────────────────────────────────────────────

/// `<pine-calendar-grid-head>` — `<thead>` with weekday labels.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarGridHead.poco",
    role = "list",
    display = "contents"
)]
pub struct PineCalendarGridHead {}

#[handlers]
impl PineCalendarGridHead {}

/// `<pine-calendar-grid-body>` — `<tbody>` containing the weeks.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarGridBody.poco",
    role = "list",
    display = "contents"
)]
pub struct PineCalendarGridBody {}

#[handlers]
impl PineCalendarGridBody {}

/// `<pine-calendar-grid-row>` — `<tr>` for one week.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarGridRow.poco",
    role = "item",
    display = "contents"
)]
pub struct PineCalendarGridRow {}

#[handlers]
impl PineCalendarGridRow {}

/// `<pine-calendar-head-cell>` — `<th scope="col">` weekday cell.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarHeadCell.poco",
    role = "label",
    display = "contents"
)]
pub struct PineCalendarHeadCell {}

#[handlers]
impl PineCalendarHeadCell {}
