//! `<pine-range-calendar-cell-trigger>` — interactive day button.
//!
//! Same shape as the single-date calendar's CellTrigger, but on
//! top of the click + arrow-key handlers it **also fires
//! `hover_date` / `clear_hover` on `RANGE_ROOT`** so the
//! `is_in_preview` flags on every cell track the cursor between
//! `start` and the hovered date.

use crate::datetime::DateValue;
use crate::range_calendar::root::{PineRangeCalendarRoot, RANGE_ROOT};
use pocopine::prelude::*;
use pocopine::{current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

// ── Cell ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarCell.poco",
    role = "item",
    display = "contents"
)]
pub struct PineRangeCalendarCell {
    #[prop]
    pub date: String,
}

#[handlers]
impl PineRangeCalendarCell {}

// ── CellTrigger ──────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarCellTrigger.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineRangeCalendarCellTrigger {
    #[prop]
    pub date: String,
    #[prop]
    pub day: u32,
}

#[handlers]
impl PineRangeCalendarCellTrigger {
    pub fn on_click(&mut self) {
        let date = self.date.clone();
        if let Some(root) = RANGE_ROOT.inject() {
            root.update(|r: &mut PineRangeCalendarRoot| r.select_date(date.clone()));
        }
    }

    pub fn on_activate(&mut self) {
        self.on_click();
    }

    pub fn on_hover(&mut self) {
        let date = self.date.clone();
        if let Some(root) = RANGE_ROOT.inject() {
            root.update(|r: &mut PineRangeCalendarRoot| r.hover_date(date.clone()));
        }
    }

    pub fn on_leave(&mut self) {
        // No-op here — the grid body handles @mouseleave on the
        // whole body to clear focus once the pointer leaves every
        // cell. Leaving individual cells would flicker the preview
        // off as the cursor crosses cell boundaries.
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

impl PineRangeCalendarCellTrigger {
    fn shift_focus(&self, delta: i32) {
        let Some(cur) = DateValue::parse_iso(&self.date) else {
            return;
        };
        let target = cur.add_days(delta as i64);

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
            "[data-pine-range-calendar-cell-trigger][data-value=\"{}\"]",
            target
        );
        let Ok(Some(target_el)) = parent_el.query_selector(&selector) else {
            return;
        };
        if let Ok(html) = target_el.dyn_into::<HtmlElement>() {
            let _ = html.focus();
            // Keyboard-driven range preview: update focused to
            // match the newly-focused cell so the preview tracks.
            let iso = target.to_string();
            if let Some(root) = RANGE_ROOT.inject() {
                root.update(|r: &mut PineRangeCalendarRoot| r.hover_date(iso.clone()));
            }
        }
    }
}

fn find_grid_root(from: &web_sys::Element) -> Option<web_sys::Element> {
    let mut cursor: Option<web_sys::Element> = Some(from.clone());
    while let Some(el) = cursor {
        let tag = el.tag_name().to_ascii_lowercase();
        if tag == "pine-range-calendar-grid"
            || tag == "div" && el.class_list().contains("pine-range-calendar-grid")
        {
            return Some(el);
        }
        cursor = el.parent_element();
    }
    None
}
