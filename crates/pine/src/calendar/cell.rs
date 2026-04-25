//! `<pine-calendar-cell>` + `<pine-calendar-cell-trigger>` — day cells
//! inside the month grid.
//!
//! The parent template decides which flags (`is_selected`,
//! `is_disabled`, `is_current_month`, `is_out_of_bounds`) belong on
//! the cell by iterating `ROOT.months[].weeks[].`
//! [`CalendarCellView`](super::root::CalendarCellView) and passing
//! the cell's `date` into each part. The cell itself stays
//! structural; the trigger owns click + keyboard selection.
//!
//! Keyboard navigation for v1:
//! - Arrow left / right = ±1 day (within the visible view)
//! - Arrow up / down = ±7 days
//! - Enter / Space = select
//!
//! Cross-page arrow navigation (arrow off the edge of the visible
//! month) is deferred — Prev / Next buttons are the only way to
//! step between pages. Implementing requires driving focus across
//! a reactive re-render which needs tick::next+querySelector
//! plumbing; tracked for v2.

use crate::calendar::root::{PineCalendarRoot, ROOT};
use crate::datetime::DateValue;
use pocopine::prelude::*;
use pocopine::{current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

// ── Cell ─────────────────────────────────────────────────────────

/// `<pine-calendar-cell>` — `<td role="gridcell">` wrapper. The
/// `date` prop exists so the trigger inside can forward it without
/// the author re-writing it twice.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarCell.poco",
    role = "item",
    display = "contents"
)]
pub struct PineCalendarCell {
    /// ISO `YYYY-MM-DD` date this cell represents.
    #[prop]
    pub date: String,
}

#[handlers]
impl PineCalendarCell {}

// ── CellTrigger ──────────────────────────────────────────────────

/// `<pine-calendar-cell-trigger>` — the interactive `<button>`
/// inside a cell. Click or Enter/Space dispatches `select_date` on
/// the root. Arrow keys shift focus by querying sibling triggers
/// by `data-value`.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarCellTrigger.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineCalendarCellTrigger {
    /// ISO `YYYY-MM-DD` date this trigger acts on.
    #[prop]
    pub date: String,
    /// Day-of-month displayed inside the button. Authors usually
    /// bind this via `:day="cell.day"` from the parent grid so the
    /// value evaluates in the iteration scope; interpolating
    /// iterator variables inside slot content is unsupported.
    #[prop]
    pub day: u32,
}

#[handlers]
impl PineCalendarCellTrigger {
    pub fn on_click(&mut self) {
        let date = self.date.clone();
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineCalendarRoot| r.select_date(date.clone()));
        }
    }

    pub fn on_activate(&mut self) {
        self.on_click();
    }

    pub fn on_arrow_left(&mut self) {
        self.shift_focus(-1);
    }
    pub fn on_arrow_right(&mut self) {
        self.shift_focus(1);
    }
    pub fn on_arrow_up(&mut self) {
        self.shift_focus(-7);
    }
    pub fn on_arrow_down(&mut self) {
        self.shift_focus(7);
    }
}

impl PineCalendarCellTrigger {
    /// Shift keyboard focus by `delta` days. v1 stays within the
    /// visible view — if the target day isn't rendered, the
    /// keystroke is a no-op.
    fn shift_focus(&self, delta: i32) {
        let Some(cur) = DateValue::parse_iso(&self.date) else {
            return;
        };
        let target = cur.add_days(delta as i64);

        // Scope-local root ref so we can query siblings that share
        // the same grid body without hitting the global document.
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(self_el) = refs::get_on(scope, "root") else {
            return;
        };
        let Some(parent_el) = find_grid_root(&self_el) else {
            return;
        };
        let selector = format!(
            "[data-pine-calendar-cell-trigger][data-value=\"{}\"]",
            target
        );
        let Ok(Some(target_el)) = parent_el.query_selector(&selector) else {
            return;
        };
        if let Ok(html) = target_el.dyn_into::<HtmlElement>() {
            let _ = html.focus();
        }
    }
}

/// Walk up from the trigger element to the enclosing
/// `<pine-calendar-grid>` host so `querySelector` scopes to the
/// current calendar instance (not any sibling calendars on the page).
fn find_grid_root(from: &web_sys::Element) -> Option<web_sys::Element> {
    let mut cursor: Option<web_sys::Element> = Some(from.clone());
    while let Some(el) = cursor {
        let tag = el.tag_name().to_ascii_lowercase();
        if tag == "pine-calendar-grid" || tag == "table" {
            return Some(el);
        }
        cursor = el.parent_element();
    }
    None
}
