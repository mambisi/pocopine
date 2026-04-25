//! `<pine-calendar-root>` — state owner + ARIA shell for the
//! Calendar primitive family. Port of reka-ui's `CalendarRoot.vue`.
//!
//! Owns every prop + state that child parts (Header, Heading,
//! Prev, Next, Grid, Cell…) need to read through the [`ROOT`]
//! context. Internal state delegates to [`super::state::CalendarState`]
//! for transitions; this module is just the `#[component]` shell
//! plus a serializable view model templates can iterate.
//!
//! v1 scope vs. reka:
//! - Single-select only (no array modelValue). Multi-select will
//!   layer on later by widening `value`.
//! - Dates cross the prop boundary as ISO `YYYY-MM-DD` strings,
//!   but deserialize directly into [`DateValue`] / `Option<DateValue>`
//!   so component state stays typed.
//! - Locale-aware labels are deferred — month names hardcode
//!   English. `weekday_format` picks a label length from a
//!   fixed English table.

use crate::calendar::state::CalendarState;
use crate::datetime::DateValue;
use pocopine::create_context;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

create_context!(pub ROOT: Handle<PineCalendarRoot>);

// ── serializable view model ─────────────────────────────────────

/// Per-day cell the template iterates over. All fields scalar so
/// they cross the WASM / JS boundary cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarCellView {
    pub date: String,
    pub day: u8,
    pub is_current_month: bool,
    pub is_selected: bool,
    pub is_disabled: bool,
    pub is_out_of_bounds: bool,
}

/// One month's grid of weeks × days, plus a label the header
/// can render verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarMonthView {
    pub value: String,
    pub label: String,
    pub weeks: Vec<Vec<CalendarCellView>>,
}

// ── root ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineCalendarRoot.poco",
    role = "scope",
    display = "contents"
)]
// RFC 049 — a Calendar is exactly a Header + a Grid. Any other
// direct child (stray labels, extra wrappers, arbitrary elements)
// breaks the visual and a11y contract: the Header owns month
// navigation, the Grid owns the date selection. Authors who want
// surrounding chrome wrap the whole `<pine-calendar-root>`, not
// its interior.
#[slot(default, only = [
    super::header::PineCalendarHeader,
    super::grid::PineCalendarGrid,
])]
pub struct PineCalendarRoot {
    // ── authored config ─────────────────────────────────────
    /// Visible-month anchor. `None` falls back to today.
    #[model]
    pub placeholder: Option<DateValue>,
    /// Controlled selected date. `None` = unselected. Two-way
    /// bindable via `pp-model:value="…"`.
    #[model]
    pub value: Option<DateValue>,
    /// Minimum selectable date (inclusive). `None` = no bound.
    #[prop]
    pub min_value: Option<DateValue>,
    /// Maximum selectable date (inclusive). `None` = no bound.
    #[prop]
    pub max_value: Option<DateValue>,
    /// Weekday index the grid starts on. 0 = Sunday … 6 = Saturday.
    #[prop]
    pub week_starts_on: u32,
    /// When set, renders 6 weeks per month so the grid height
    /// never jumps between shorter / longer months.
    #[prop]
    pub fixed_weeks: bool,
    /// How many months to render side by side. `0` normalizes to 1.
    #[prop]
    pub number_of_months: u32,
    /// Advance by `number_of_months` per page instead of 1.
    #[prop]
    pub paged_navigation: bool,
    /// Clicking a selected date clears it unless this is set.
    #[prop]
    pub prevent_deselect: bool,
    /// Disable all interaction.
    #[prop]
    pub disabled: bool,
    /// Prevent selection but keep navigation enabled.
    #[prop]
    pub readonly: bool,

    // ── derived state — template-visible ────────────────────
    /// Rendered title for `<pine-calendar-heading>`. E.g.
    /// `"June 2024"` or `"June 2024 – July 2024"` for ranges.
    pub heading: String,
    /// Localized weekday labels in column order, first entry
    /// aligned with `week_starts_on`.
    pub weekdays: Vec<String>,
    /// Grid of months the view is currently showing.
    pub months: Vec<CalendarMonthView>,
    /// Flat list of every cell across every visible month, in
    /// display order. Populated alongside `months` so templates
    /// can iterate a single pp-for (nested pp-for over a reactive
    /// Vec accumulates stale clones in this Pine revision).
    pub cells: Vec<CalendarCellView>,
    /// True when the selected value falls outside min/max or is
    /// disabled — flipped onto `data-invalid` for CSS.
    pub invalid: bool,
    /// True when Prev navigation would step past `min_value`.
    pub prev_disabled: bool,
    /// True when Next navigation would step past `max_value`.
    pub next_disabled: bool,
}

#[handlers]
impl PineCalendarRoot {
    pub fn on_setup(&mut self) {
        if self.number_of_months == 0 {
            self.number_of_months = 1;
        }
        ROOT.provide(this::<Self>());
        // Seed the view model once synchronously so the initial
        // render already has `heading` / `cells` / `weekdays`
        // populated. The flat `cells` Vec means a single pp-for
        // re-runs cleanly when subsequent prop writes land via
        // the #[watch]ers below — no nested pp-for clones to
        // leak.
        self.reflow();
    }

    pub fn on_mount(&mut self) {
        // pp-model / pp-bind effects fire between on_setup and
        // on_mount, so by this point any author-supplied
        // `placeholder`, `value`, `min-value`, `max-value`, etc.
        // have settled. A second reflow here guarantees the
        // view reflects the final prop state even if the
        // individual #[watch] handlers skipped an initial-write.
        self.reflow();
    }

    #[watch(placeholder)]
    fn on_placeholder_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.reflow();
    }
    #[watch(value)]
    fn on_value_change(&mut self, _new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
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
    fn on_fixed_weeks_change(&mut self, _new: bool, _prev: Option<bool>) {
        self.reflow();
    }
    #[watch(number_of_months)]
    fn on_months_change(&mut self, _new: u32, _prev: Option<u32>) {
        self.reflow();
    }
    #[watch(week_starts_on)]
    fn on_week_starts_on_change(&mut self, _new: u32, _prev: Option<u32>) {
        self.reflow();
    }

    /// Advance one page forward.
    pub fn next_page(&mut self) {
        if self.disabled {
            return;
        }
        let mut s = self.build_state();
        s.next_page();
        self.placeholder = Some(s.placeholder);
        self.apply(&s);
    }

    /// Advance one page backward.
    pub fn prev_page(&mut self) {
        if self.disabled {
            return;
        }
        let mut s = self.build_state();
        s.prev_page();
        self.placeholder = Some(s.placeholder);
        self.apply(&s);
    }

    /// Select a date given as ISO `YYYY-MM-DD`. Cells dispatch
    /// through this so the Root can observe the pick, update
    /// `value`, and re-anchor the placeholder when needed.
    pub fn select_date(&mut self, iso: String) {
        if self.disabled || self.readonly {
            return;
        }
        let Some(date) = DateValue::parse_iso(&iso) else {
            return;
        };
        let mut s = self.build_state();
        if s.is_date_out_of_bounds(&date) {
            return;
        }
        s.select_date(date);
        self.value = s.selected;
        self.placeholder = Some(s.placeholder);
        self.apply(&s);
    }
}

impl PineCalendarRoot {
    /// Snapshot the current props into a [`CalendarState`]. Cheap
    /// enough to rebuild in every handler — the grid construction
    /// is O(months × 42) scalar ops.
    fn build_state(&self) -> CalendarState {
        let placeholder = self
            .placeholder
            .or(self.value)
            .unwrap_or_else(fallback_today);
        let week_starts_on = (self.week_starts_on.min(6)) as u8;
        CalendarState::new(placeholder)
            .with_selected(self.value)
            .with_min(self.min_value)
            .with_max(self.max_value)
            .with_fixed_weeks(self.fixed_weeks)
            .with_number_of_months(self.number_of_months.max(1))
            .with_week_starts_on(week_starts_on)
            .with_paged_navigation(self.paged_navigation)
            .with_prevent_deselect(self.prevent_deselect)
            .with_disabled(self.disabled)
    }

    /// Re-derive every view-model field (heading, months,
    /// weekdays, button / invalid flags) from the current props.
    fn reflow(&mut self) {
        let s = self.build_state();
        self.apply(&s);
    }

    fn apply(&mut self, s: &CalendarState) {
        self.heading = format_heading(s);
        self.weekdays = weekday_labels((self.week_starts_on.min(6)) as u8);
        self.prev_disabled = s.is_prev_button_disabled();
        self.next_disabled = s.is_next_button_disabled();
        self.invalid = s
            .selected
            .map(|d| s.is_date_out_of_bounds(&d))
            .unwrap_or(false);
        let months = build_months(s);
        self.cells = months
            .iter()
            .flat_map(|m| m.weeks.iter().flatten().cloned())
            .collect();
        self.months = months;
    }
}

// ── view-model helpers ──────────────────────────────────────────

fn build_months(s: &CalendarState) -> Vec<CalendarMonthView> {
    s.grid
        .iter()
        .map(|month| {
            let weeks = month
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|date| CalendarCellView {
                            date: date.to_string(),
                            day: date.day(),
                            is_current_month: date.year() == month.value.year()
                                && date.month() == month.value.month(),
                            is_selected: s.is_date_selected(date),
                            is_disabled: s.disabled,
                            is_out_of_bounds: s.is_date_out_of_bounds(date),
                        })
                        .collect()
                })
                .collect();
            CalendarMonthView {
                value: month.value.to_string(),
                label: format_month_label(&month.value),
                weeks,
            }
        })
        .collect()
}

fn format_heading(s: &CalendarState) -> String {
    if s.grid.is_empty() {
        return String::new();
    }
    if s.grid.len() == 1 {
        return format_month_label(&s.grid[0].value);
    }
    let first = &s.grid[0].value;
    let last = &s.grid.last().unwrap().value;
    let first_label = format_month_label(first);
    let last_label = format_month_label(last);
    if first.year() == last.year() {
        format!("{} – {}", strip_year_suffix(&first_label), last_label,)
    } else {
        format!("{} – {}", first_label, last_label)
    }
}

fn format_month_label(date: &DateValue) -> String {
    format!("{} {}", month_name(date.month()), date.year())
}

fn strip_year_suffix(label: &str) -> String {
    // "June 2024" → "June". Works for v1 since our labels are
    // strictly `{Month} {Year}` — locale-aware labels in v2 will
    // need a dedicated range formatter.
    match label.rsplit_once(' ') {
        Some((head, _)) => head.to_string(),
        None => label.to_string(),
    }
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
    // Short English names, Sunday-indexed. Locale-aware labels
    // via Intl.DateTimeFormat arrive with the v2 formatter.
    let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    (0..7)
        .map(|i| {
            let idx = ((week_starts_on as usize) + i) % 7;
            names[idx].to_string()
        })
        .collect()
}

/// Best-effort "today" in the browser's local time. Falls back to
/// 1970-01-01 on host builds / when the JS date boundary is
/// unavailable (tests, SSR, etc.).
fn fallback_today() -> DateValue {
    #[cfg(target_arch = "wasm32")]
    {
        let js_date = js_sys::Date::new_0();
        let year = js_date.get_full_year() as i32;
        let month = (js_date.get_month() + 1) as u8; // JS months are 0-indexed
        let day = js_date.get_date() as u8;
        if let Some(d) = DateValue::new(year, month, day) {
            return d;
        }
    }
    DateValue::new(1970, 1, 1).expect("epoch")
}
