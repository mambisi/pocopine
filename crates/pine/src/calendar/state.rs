//! Pure calendar state machine — Rust port of the logic half of
//! reka-ui's `useCalendar.ts` / `useCalendarState`.
//!
//! No DOM, no reactivity, no props reflection — just the data model
//! and transitions that [`super::root::PineCalendarRoot`] wraps and
//! exposes to child components via context. Isolated here so the
//! navigation math is unit-testable on the host.
//!
//! Scope decisions relative to reka:
//!
//! - Selection is single-value for v1. Multi-select can be added by
//!   swapping `selected: Option<DateValue>` for a `Vec<DateValue>`;
//!   the transitions stay structurally the same.
//! - Custom `next_page` / `prev_page` functions are not represented
//!   here — navigation always moves by whole months (or
//!   `number_of_months` when `paged_navigation` is on).
//! - Locale-driven formatting (headings, weekday labels) is deferred
//!   to the component layer where `js-sys::Intl` can be reached.

use crate::datetime::{
    compare_year_month, create_months, get_days_in_month, is_after, is_before, is_same_day,
    is_same_month, start_of_month, CreateMonthsProps, DateValue, Grid, WeekStartsOn,
};

/// Configuration plus resolved state of a calendar at a point in
/// time. Written via the transition methods; read by renderers.
#[derive(Debug, Clone)]
pub struct CalendarState {
    // ── authored config (mirrors reka's useCalendar props) ──
    pub placeholder: DateValue,
    pub selected: Option<DateValue>,
    pub min_value: Option<DateValue>,
    pub max_value: Option<DateValue>,
    pub disabled: bool,
    pub fixed_weeks: bool,
    pub number_of_months: u32,
    pub week_starts_on: WeekStartsOn,
    pub paged_navigation: bool,
    pub prevent_deselect: bool,

    // ── derived ────────────────────────────────────────────
    pub grid: Vec<Grid<DateValue>>,
}

impl CalendarState {
    /// Start a new state anchored at `placeholder`, with the
    /// specified number of months visible.
    pub fn new(placeholder: DateValue) -> Self {
        let mut s = Self {
            placeholder,
            selected: None,
            min_value: None,
            max_value: None,
            disabled: false,
            fixed_weeks: false,
            number_of_months: 1,
            week_starts_on: 0,
            paged_navigation: false,
            prevent_deselect: false,
            grid: Vec::new(),
        };
        s.rebuild_grid();
        s
    }

    // ── builder helpers so tests / callers can stay fluent ──

    pub fn with_selected(mut self, v: Option<DateValue>) -> Self {
        self.selected = v;
        self
    }
    pub fn with_min(mut self, v: Option<DateValue>) -> Self {
        self.min_value = v;
        self
    }
    pub fn with_max(mut self, v: Option<DateValue>) -> Self {
        self.max_value = v;
        self
    }
    pub fn with_fixed_weeks(mut self, v: bool) -> Self {
        self.fixed_weeks = v;
        self.rebuild_grid();
        self
    }
    pub fn with_number_of_months(mut self, n: u32) -> Self {
        self.number_of_months = n.max(1);
        self.rebuild_grid();
        self
    }
    pub fn with_week_starts_on(mut self, d: WeekStartsOn) -> Self {
        self.week_starts_on = d.min(6);
        self.rebuild_grid();
        self
    }
    pub fn with_paged_navigation(mut self, v: bool) -> Self {
        self.paged_navigation = v;
        self
    }
    pub fn with_prevent_deselect(mut self, v: bool) -> Self {
        self.prevent_deselect = v;
        self
    }
    pub fn with_disabled(mut self, v: bool) -> Self {
        self.disabled = v;
        self
    }

    /// Rebuild [`grid`] from the current [`placeholder`] /
    /// [`number_of_months`] / [`fixed_weeks`] / [`week_starts_on`].
    /// Called automatically by the transitions that change those
    /// inputs. Exposed so callers that mutate fields directly can
    /// resync without a navigation call.
    pub fn rebuild_grid(&mut self) {
        self.grid = create_months(CreateMonthsProps {
            date: &self.placeholder,
            week_starts_on: self.week_starts_on,
            fixed_weeks: self.fixed_weeks,
            number_of_months: self.number_of_months,
        });
    }

    // ── navigation ────────────────────────────────────────

    /// Move the visible view `step` whole months forward. The
    /// paged-navigation flag multiplies by `number_of_months` to
    /// match reka's behavior when rendering multiple months at once.
    pub fn next_page(&mut self) {
        let step = if self.paged_navigation {
            self.number_of_months as i32
        } else {
            1
        };
        let first = self
            .grid
            .first()
            .map(|g| g.value)
            .unwrap_or(self.placeholder);
        let new_placeholder = start_of_month(&first.add_months(step));
        self.placeholder = new_placeholder;
        self.rebuild_grid();
    }

    pub fn prev_page(&mut self) {
        let step = if self.paged_navigation {
            self.number_of_months as i32
        } else {
            1
        };
        let first = self
            .grid
            .first()
            .map(|g| g.value)
            .unwrap_or(self.placeholder);
        let new_placeholder = start_of_month(&first.subtract_months(step));
        self.placeholder = new_placeholder;
        self.rebuild_grid();
    }

    /// Select `date`. Deselects when `date` is already selected and
    /// `prevent_deselect` is off, matching reka's single-value
    /// toggle. Also snaps the placeholder onto the selected date so
    /// the grid scrolls to contain it.
    pub fn select_date(&mut self, date: DateValue) {
        match self.selected {
            Some(cur) if is_same_day(&cur, &date) && !self.prevent_deselect => {
                self.selected = None;
                self.placeholder = date;
                self.rebuild_grid();
            }
            _ => {
                self.selected = Some(date);
                if !self.grid.iter().any(|g| is_same_month(&g.value, &date)) {
                    self.placeholder = date;
                    self.rebuild_grid();
                }
            }
        }
    }

    // ── predicates used by cell / nav renderers ──

    pub fn is_date_selected(&self, date: &DateValue) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|sel| is_same_day(sel, date))
    }

    /// True when `date` is outside the caller-enforced min/max
    /// window or the calendar itself is disabled. Author-supplied
    /// matchers (`isDateDisabled`) layer on top at the component
    /// level — this method is just for the bounds math.
    pub fn is_date_out_of_bounds(&self, date: &DateValue) -> bool {
        if self.disabled {
            return true;
        }
        if self.min_value.as_ref().is_some_and(|m| is_before(date, m)) {
            return true;
        }
        if self.max_value.as_ref().is_some_and(|m| is_after(date, m)) {
            return true;
        }
        false
    }

    pub fn is_outside_visible_view(&self, date: &DateValue) -> bool {
        !self.grid.iter().any(|g| is_same_month(&g.value, date))
    }

    /// True when the Prev button should be disabled — `min_value`
    /// is set and the last day of the previous page falls before it.
    pub fn is_prev_button_disabled(&self) -> bool {
        let Some(min) = self.min_value else {
            return false;
        };
        if self.disabled || self.grid.is_empty() {
            return self.disabled;
        }
        let first = self.grid[0].value;
        let last_of_prev = first
            .subtract_months(1)
            .set_day(get_days_in_month(&first.subtract_months(1)));
        is_before(&last_of_prev, &min)
    }

    /// True when the Next button should be disabled.
    pub fn is_next_button_disabled(&self) -> bool {
        let Some(max) = self.max_value else {
            return false;
        };
        if self.disabled || self.grid.is_empty() {
            return self.disabled;
        }
        let last = self.grid.last().expect("non-empty").value;
        let first_of_next = last.add_months(1).set_day(1);
        is_after(&first_of_next, &max)
    }

    /// First date in the current view that an author-supplied
    /// disabled matcher would accept — useful for
    /// `initial_focus` placement. Accepts a closure to stay
    /// decoupled from the matcher types the component layer owns.
    pub fn first_focusable_date<F: Fn(&DateValue) -> bool>(
        &self,
        is_disabled: F,
    ) -> Option<DateValue> {
        for month in &self.grid {
            if self.min_value.is_some_and(|m| is_before(&month.value, &m)) {
                continue;
            }
            let days = get_days_in_month(&month.value);
            let start = self
                .min_value
                .filter(|m| is_same_month(&month.value, m))
                .map_or(1, |m| m.day());
            for day in start..=days {
                let date = month.value.set_day(day);
                if self.is_date_out_of_bounds(&date) || is_disabled(&date) {
                    continue;
                }
                return Some(date);
            }
        }
        None
    }

    /// The first day of the month closest to the current
    /// placeholder that still sits in the visible view. Used by
    /// [`super::root::PineCalendarRoot`] to keep the roving focus
    /// from landing on an off-view day after navigation.
    pub fn placeholder_in_view(&self) -> bool {
        self.grid
            .iter()
            .any(|g| compare_year_month(&g.value, &self.placeholder) == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u8, day: u8) -> DateValue {
        DateValue::new(y, m, day).unwrap()
    }

    #[test]
    fn new_builds_grid_for_placeholder_month() {
        let s = CalendarState::new(d(2024, 6, 15));
        assert_eq!(s.grid.len(), 1);
        assert_eq!(s.grid[0].value.year(), 2024);
        assert_eq!(s.grid[0].value.month(), 6);
    }

    #[test]
    fn next_page_steps_by_one_month_default() {
        let mut s = CalendarState::new(d(2024, 6, 15));
        s.next_page();
        assert_eq!(s.placeholder, d(2024, 7, 1));
        s.prev_page();
        assert_eq!(s.placeholder, d(2024, 6, 1));
    }

    #[test]
    fn paged_navigation_steps_by_number_of_months() {
        let mut s = CalendarState::new(d(2024, 1, 15))
            .with_number_of_months(3)
            .with_paged_navigation(true);
        assert_eq!(s.grid.len(), 3);
        s.next_page();
        // Started at January → three months shown → next page
        // jumps to April.
        assert_eq!(s.placeholder, d(2024, 4, 1));
    }

    #[test]
    fn select_toggles_single_value() {
        let mut s = CalendarState::new(d(2024, 6, 1));
        let target = d(2024, 6, 15);
        s.select_date(target);
        assert_eq!(s.selected, Some(target));
        // Clicking again deselects.
        s.select_date(target);
        assert_eq!(s.selected, None);
    }

    #[test]
    fn prevent_deselect_keeps_selection() {
        let mut s = CalendarState::new(d(2024, 6, 1)).with_prevent_deselect(true);
        let t = d(2024, 6, 15);
        s.select_date(t);
        s.select_date(t);
        assert_eq!(s.selected, Some(t));
    }

    #[test]
    fn select_outside_view_moves_placeholder() {
        let mut s = CalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 9, 10));
        assert!(s
            .grid
            .iter()
            .any(|g| g.value.month() == 9 && g.value.year() == 2024));
        assert_eq!(s.placeholder, d(2024, 9, 10));
    }

    #[test]
    fn is_prev_button_disabled_respects_min() {
        let s = CalendarState::new(d(2024, 6, 1)).with_min(Some(d(2024, 5, 20)));
        // Previous page would end 2024-05-31, which is after the
        // min of 2024-05-20 — Prev still enabled.
        assert!(!s.is_prev_button_disabled());

        let blocked = CalendarState::new(d(2024, 6, 1)).with_min(Some(d(2024, 6, 1)));
        // Previous page would end 2024-05-31, which is before
        // min 2024-06-01 — Prev disabled.
        assert!(blocked.is_prev_button_disabled());
    }

    #[test]
    fn is_next_button_disabled_respects_max() {
        let blocked = CalendarState::new(d(2024, 6, 15)).with_max(Some(d(2024, 6, 30)));
        // Next page would begin 2024-07-01, after max 2024-06-30.
        assert!(blocked.is_next_button_disabled());
        let open = CalendarState::new(d(2024, 6, 15)).with_max(Some(d(2024, 7, 15)));
        assert!(!open.is_next_button_disabled());
    }

    #[test]
    fn first_focusable_date_skips_disabled() {
        let s = CalendarState::new(d(2024, 6, 1));
        // Disable everything before the 10th.
        let focus = s.first_focusable_date(|date| date.day() < 10);
        assert_eq!(focus, Some(d(2024, 6, 10)));
    }

    #[test]
    fn is_date_out_of_bounds_checks_min_max() {
        let s = CalendarState::new(d(2024, 6, 15))
            .with_min(Some(d(2024, 6, 10)))
            .with_max(Some(d(2024, 6, 20)));
        assert!(s.is_date_out_of_bounds(&d(2024, 6, 5)));
        assert!(s.is_date_out_of_bounds(&d(2024, 6, 25)));
        assert!(!s.is_date_out_of_bounds(&d(2024, 6, 15)));
    }

    #[test]
    fn is_outside_visible_view_detects_other_months() {
        let s = CalendarState::new(d(2024, 6, 1));
        assert!(s.is_outside_visible_view(&d(2024, 8, 1)));
        assert!(!s.is_outside_visible_view(&d(2024, 6, 30)));
    }
}
