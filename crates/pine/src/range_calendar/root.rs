//! `<pine-range-calendar-root>` — state owner + context provider
//! for the range-selection calendar. Mirrors
//! [`crate::calendar::root::PineCalendarRoot`] but tracks
//! **two** typed dates (`start` / `end`) plus a `focused` cursor
//! for hover preview.
//!
//! Props: `start`, `end`, `placeholder`, `min_value`, `max_value`,
//! `week_starts_on`, `fixed_weeks`, `number_of_months`,
//! `paged_navigation`, `disabled`, `readonly`.

use crate::datetime::DateValue;
use crate::range_calendar::state::RangeCalendarState;
use pocopine::prelude::*;
use pocopine::{inject_key, provide};
use serde::{Deserialize, Serialize};

inject_key!(pub RANGE_ROOT: Handle<PineRangeCalendarRoot>);

// ── view model — per-cell flags for the template to iterate ──

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeCellView {
    pub date: String,
    pub day: u8,
    pub is_current_month: bool,
    pub is_selection_start: bool,
    pub is_selection_end: bool,
    pub is_in_range: bool,
    pub is_selected: bool,
    pub is_preview_start: bool,
    pub is_preview_end: bool,
    pub is_in_preview: bool,
    pub is_out_of_bounds: bool,
    pub is_disabled: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRangeCalendarRoot.poco",
    role = "scope",
    display = "contents"
)]
pub struct PineRangeCalendarRoot {
    #[model]
    pub start: Option<DateValue>,
    #[model]
    pub end: Option<DateValue>,
    #[model]
    pub placeholder: Option<DateValue>,
    #[prop]
    pub min_value: Option<DateValue>,
    #[prop]
    pub max_value: Option<DateValue>,
    #[prop]
    pub week_starts_on: u32,
    #[prop]
    pub fixed_weeks: bool,
    #[prop]
    pub number_of_months: u32,
    #[prop]
    pub paged_navigation: bool,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub readonly: bool,

    // ── derived view model ──────────────────────────────────
    pub heading: String,
    pub weekdays: Vec<String>,
    pub cells: Vec<RangeCellView>,
    pub invalid: bool,
    pub prev_disabled: bool,
    pub next_disabled: bool,
    /// ISO of the currently-focused cell (mouse hover or keyboard
    /// focus). Drives the live range preview while `end` is unset.
    pub focused: Option<DateValue>,
}

#[handlers]
impl PineRangeCalendarRoot {
    pub fn on_setup(&mut self) {
        if self.number_of_months == 0 {
            self.number_of_months = 1;
        }
        provide(&RANGE_ROOT, this::<Self>());
        self.reflow();
    }

    pub fn on_mount(&mut self) {
        self.reflow();
    }

    #[watch(start)]
    fn on_start_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.reflow();
    }
    #[watch(end)]
    fn on_end_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.reflow();
    }
    #[watch(placeholder)]
    fn on_placeholder_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.reflow();
    }
    #[watch(min_value)]
    fn on_min_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.reflow();
    }
    #[watch(max_value)]
    fn on_max_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.reflow();
    }
    #[watch(fixed_weeks)]
    fn on_fixed_change(&mut self, _new: bool, _prev: Option<bool>) {
        self.reflow();
    }
    #[watch(number_of_months)]
    fn on_months_change(&mut self, _new: u32, _prev: Option<u32>) {
        self.reflow();
    }
    #[watch(week_starts_on)]
    fn on_week_starts_change(&mut self, _new: u32, _prev: Option<u32>) {
        self.reflow();
    }
    #[watch(focused)]
    fn on_focused_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        // Focus changes don't rebuild the grid — only the preview
        // flags on the cells. Cheap enough to re-run reflow().
        self.reflow();
    }

    pub fn next_page(&mut self) {
        if self.disabled {
            return;
        }
        let mut s = self.build_state();
        s.next_page();
        self.placeholder = Some(s.cal.placeholder);
        self.apply(&s);
    }

    pub fn prev_page(&mut self) {
        if self.disabled {
            return;
        }
        let mut s = self.build_state();
        s.prev_page();
        self.placeholder = Some(s.cal.placeholder);
        self.apply(&s);
    }

    /// Two-step range selection. Cells dispatch this through
    /// `inject(&RANGE_ROOT).update(|r| r.select_date(...))`.
    ///
    /// Emits on the per-field `pp:update:start` and
    /// `pp:update:end` channels via `emit_model_field`, so
    /// authors wire both endpoints with
    /// `pp-model:start="start"` + `pp-model:end="end"` on the
    /// same element without the two bindings clobbering each
    /// other.
    pub fn select_date(&mut self, iso: String) {
        if self.disabled || self.readonly {
            return;
        }
        let Some(date) = DateValue::parse_iso(&iso) else {
            return;
        };
        let mut s = self.build_state();
        if s.cal.is_date_out_of_bounds(&date) {
            return;
        }
        s.select_date(date);
        self.start = s.start;
        self.end = s.end;
        self.placeholder = Some(s.cal.placeholder);
        self.apply(&s);
    }

    /// Hover / focus cursor updates. Drives the dynamic range
    /// preview (`is_in_preview` / `is_preview_start` / `_end` on
    /// every cell view).
    pub fn hover_date(&mut self, iso: String) {
        self.focused = DateValue::parse_iso(&iso);
    }

    pub fn clear_hover(&mut self) {
        self.focused = None;
    }

    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.focused = None;
        let s = self.build_state();
        self.apply(&s);
    }
}

impl PineRangeCalendarRoot {
    fn build_state(&self) -> RangeCalendarState {
        let placeholder = self
            .placeholder
            .or(self.start)
            .unwrap_or_else(fallback_today);
        let week_starts_on = self.week_starts_on.min(6) as u8;
        RangeCalendarState::new(placeholder)
            .with_start(self.start)
            .with_end(self.end)
            .with_focused(self.focused)
            .with_min(self.min_value)
            .with_max(self.max_value)
            .with_fixed_weeks(self.fixed_weeks)
            .with_number_of_months(self.number_of_months.max(1))
            .with_week_starts_on(week_starts_on)
            .with_paged_navigation(self.paged_navigation)
            .with_disabled(self.disabled)
    }

    fn reflow(&mut self) {
        let s = self.build_state();
        self.apply(&s);
    }

    fn apply(&mut self, s: &RangeCalendarState) {
        self.heading = format_heading(s);
        self.weekdays = weekday_labels(self.week_starts_on.min(6) as u8);
        self.prev_disabled = s.cal.is_prev_button_disabled();
        self.next_disabled = s.cal.is_next_button_disabled();
        self.invalid = s.is_invalid();
        self.cells = build_cells(s);
    }
}

// ── helpers (mostly mirror the single-date Calendar root) ─────

fn build_cells(s: &RangeCalendarState) -> Vec<RangeCellView> {
    s.cal
        .grid
        .iter()
        .flat_map(|month| {
            let anchor_year = month.value.year();
            let anchor_month = month.value.month();
            month.rows.iter().flat_map(move |row| {
                row.iter().map(move |date| RangeCellView {
                    date: date.to_string(),
                    day: date.day(),
                    is_current_month: date.year() == anchor_year && date.month() == anchor_month,
                    is_selection_start: s.is_selection_start(date),
                    is_selection_end: s.is_selection_end(date),
                    is_in_range: s.is_in_range(date),
                    is_selected: s.is_selected(date),
                    is_preview_start: s.is_preview_start(date),
                    is_preview_end: s.is_preview_end(date),
                    is_in_preview: s.is_in_preview(date),
                    is_out_of_bounds: s.cal.is_date_out_of_bounds(date),
                    is_disabled: s.cal.disabled,
                })
            })
        })
        .collect()
}

fn format_heading(s: &RangeCalendarState) -> String {
    if s.cal.grid.is_empty() {
        return String::new();
    }
    let first = &s.cal.grid[0].value;
    let last = &s.cal.grid.last().unwrap().value;
    if first.year() == last.year() && first.month() == last.month() {
        return format_month_label(first);
    }
    format!(
        "{} – {}",
        format_month_label(first),
        format_month_label(last)
    )
}

fn format_month_label(date: &DateValue) -> String {
    format!("{} {}", month_name(date.month()), date.year())
}

fn month_name(m: u8) -> &'static str {
    match m {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "",
    }
}

fn weekday_labels(week_starts_on: u8) -> Vec<String> {
    let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    (0..7)
        .map(|i| names[((week_starts_on as usize) + i) % 7].to_string())
        .collect()
}

fn fallback_today() -> DateValue {
    #[cfg(target_arch = "wasm32")]
    {
        let js_date = js_sys::Date::new_0();
        let year = js_date.get_full_year() as i32;
        let month = (js_date.get_month() + 1) as u8;
        let day = js_date.get_date() as u8;
        if let Some(d) = DateValue::new(year, month, day) {
            return d;
        }
    }
    DateValue::new(1970, 1, 1).expect("epoch")
}
