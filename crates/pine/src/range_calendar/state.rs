//! Pure range-calendar state — port of reka-ui's
//! `useRangeCalendarState` layered on top of the single-date
//! [`CalendarState`](crate::calendar::CalendarState).
//!
//! Composition rather than duplication: the inner `cal` handles
//! the grid / placeholder / navigation math, and this wrapper
//! layers two-date selection + hover-preview on top. Anything
//! that doesn't interact with the range (navigation, bounds,
//! days-in-month) delegates straight through.

use crate::calendar::state::CalendarState;
use crate::datetime::{is_before, is_between, is_same_day, DateValue, WeekStartsOn};

#[derive(Debug, Clone)]
pub struct RangeCalendarState {
    /// Single-date grid engine providing `grid`, `placeholder`,
    /// navigation + bounds.
    pub cal: CalendarState,
    /// Start of the current range. `None` means nothing picked yet.
    pub start: Option<DateValue>,
    /// End of the current range. `Some(_)` only after the user
    /// has completed the second click of the two-step selection.
    pub end: Option<DateValue>,
    /// Currently hovered / keyboard-focused date — drives the
    /// dynamic range *preview* shown between the existing `start`
    /// and the cursor before `end` is committed.
    pub focused: Option<DateValue>,
}

impl RangeCalendarState {
    pub fn new(placeholder: DateValue) -> Self {
        Self {
            cal: CalendarState::new(placeholder),
            start: None,
            end: None,
            focused: None,
        }
    }

    // ── builders forwarding to the inner CalendarState ──────

    pub fn with_start(mut self, v: Option<DateValue>) -> Self {
        self.start = v;
        self
    }
    pub fn with_end(mut self, v: Option<DateValue>) -> Self {
        self.end = v;
        self
    }
    pub fn with_focused(mut self, v: Option<DateValue>) -> Self {
        self.focused = v;
        self
    }
    pub fn with_min(mut self, v: Option<DateValue>) -> Self {
        self.cal = self.cal.with_min(v);
        self
    }
    pub fn with_max(mut self, v: Option<DateValue>) -> Self {
        self.cal = self.cal.with_max(v);
        self
    }
    pub fn with_fixed_weeks(mut self, v: bool) -> Self {
        self.cal = self.cal.with_fixed_weeks(v);
        self
    }
    pub fn with_number_of_months(mut self, n: u32) -> Self {
        self.cal = self.cal.with_number_of_months(n);
        self
    }
    pub fn with_week_starts_on(mut self, d: WeekStartsOn) -> Self {
        self.cal = self.cal.with_week_starts_on(d);
        self
    }
    pub fn with_paged_navigation(mut self, v: bool) -> Self {
        self.cal = self.cal.with_paged_navigation(v);
        self
    }
    pub fn with_disabled(mut self, v: bool) -> Self {
        self.cal = self.cal.with_disabled(v);
        self
    }

    // ── navigation — delegate ───────────────────────────────

    pub fn next_page(&mut self) {
        self.cal.next_page();
    }
    pub fn prev_page(&mut self) {
        self.cal.prev_page();
    }

    // ── range-selection transitions ─────────────────────────

    /// Click transition — the canonical two-step selection:
    ///
    /// 1. No `start` yet → set `start`, clear `end`.
    /// 2. `start` set, no `end` → set `end` (swapping to keep
    ///    `start ≤ end` if the user clicked before `start`).
    /// 3. Both set → reset to a fresh `start` at the clicked date.
    ///
    /// Also snaps the placeholder to keep the grid in view.
    pub fn select_date(&mut self, date: DateValue) {
        match (self.start, self.end) {
            (None, _) => {
                self.start = Some(date);
                self.end = None;
            }
            (Some(s), None) => {
                if is_before(&date, &s) {
                    self.start = Some(date);
                    self.end = Some(s);
                } else if is_same_day(&s, &date) {
                    // Click the anchor again — clear and restart.
                    self.start = None;
                    self.end = None;
                } else {
                    self.end = Some(date);
                }
            }
            (Some(_), Some(_)) => {
                self.start = Some(date);
                self.end = None;
            }
        }
        // Keep the grid anchored on the visible month.
        if !self
            .cal
            .grid
            .iter()
            .any(|g| g.value.year() == date.year() && g.value.month() == date.month())
        {
            self.cal.placeholder = date;
            self.cal.rebuild_grid();
        }
    }

    /// Update the hover / focus cursor; used while `start` is set
    /// but `end` isn't to drive a live range preview.
    pub fn set_focused(&mut self, date: Option<DateValue>) {
        self.focused = date;
    }

    /// Forget both dates and the hover.
    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.focused = None;
    }

    // ── per-cell predicates the template iterates over ──────

    pub fn is_selection_start(&self, date: &DateValue) -> bool {
        self.start.as_ref().is_some_and(|s| is_same_day(s, date))
    }

    pub fn is_selection_end(&self, date: &DateValue) -> bool {
        self.end.as_ref().is_some_and(|e| is_same_day(e, date))
    }

    /// Inside the committed [start, end] range, exclusive of the
    /// two endpoints (which get their own `is_selection_start` /
    /// `is_selection_end` flags so CSS can round the ends).
    pub fn is_in_range(&self, date: &DateValue) -> bool {
        match (self.start, self.end) {
            (Some(s), Some(e)) => is_between(date, &s, &e),
            _ => false,
        }
    }

    /// Any part of the committed selection — start, end, or
    /// in-between. Convenience for consumers that don't care which.
    pub fn is_selected(&self, date: &DateValue) -> bool {
        self.is_selection_start(date) || self.is_selection_end(date) || self.is_in_range(date)
    }

    /// True while the user is in mid-selection (`start` set,
    /// `end` not) and the cell falls between `start` and the
    /// current `focused` cursor.
    pub fn is_in_preview(&self, date: &DateValue) -> bool {
        if self.end.is_some() {
            return false;
        }
        let (Some(start), Some(focus)) = (self.start, self.focused) else {
            return false;
        };
        if is_same_day(&start, &focus) {
            return false;
        }
        let (lo, hi) = if is_before(&focus, &start) {
            (focus, start)
        } else {
            (start, focus)
        };
        is_same_day(date, &lo) || is_same_day(date, &hi) || is_between(date, &lo, &hi)
    }

    pub fn is_preview_start(&self, date: &DateValue) -> bool {
        if self.end.is_some() {
            return false;
        }
        let (Some(start), Some(focus)) = (self.start, self.focused) else {
            return false;
        };
        let lo = if is_before(&focus, &start) {
            focus
        } else {
            start
        };
        is_same_day(date, &lo)
    }

    pub fn is_preview_end(&self, date: &DateValue) -> bool {
        if self.end.is_some() {
            return false;
        }
        let (Some(start), Some(focus)) = (self.start, self.focused) else {
            return false;
        };
        let hi = if is_before(&focus, &start) {
            start
        } else {
            focus
        };
        is_same_day(date, &hi)
    }

    /// End-before-start would be visually confusing; surface it so
    /// consumers can flip a `data-invalid` flag.
    pub fn is_invalid(&self) -> bool {
        matches!(
            (self.start, self.end),
            (Some(s), Some(e)) if is_before(&e, &s)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u8, day: u8) -> DateValue {
        DateValue::new(y, m, day).unwrap()
    }

    #[test]
    fn new_starts_empty() {
        let s = RangeCalendarState::new(d(2024, 6, 1));
        assert!(s.start.is_none());
        assert!(s.end.is_none());
        assert!(s.focused.is_none());
    }

    #[test]
    fn first_click_sets_start_only() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        assert_eq!(s.start, Some(d(2024, 6, 10)));
        assert!(s.end.is_none());
    }

    #[test]
    fn second_click_after_start_sets_end() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        s.select_date(d(2024, 6, 20));
        assert_eq!(s.start, Some(d(2024, 6, 10)));
        assert_eq!(s.end, Some(d(2024, 6, 20)));
    }

    #[test]
    fn second_click_before_start_swaps() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 20));
        s.select_date(d(2024, 6, 10));
        assert_eq!(s.start, Some(d(2024, 6, 10)));
        assert_eq!(s.end, Some(d(2024, 6, 20)));
    }

    #[test]
    fn clicking_anchor_again_clears() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        s.select_date(d(2024, 6, 10));
        assert!(s.start.is_none());
        assert!(s.end.is_none());
    }

    #[test]
    fn third_click_restarts() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        s.select_date(d(2024, 6, 20));
        s.select_date(d(2024, 6, 5));
        assert_eq!(s.start, Some(d(2024, 6, 5)));
        assert!(s.end.is_none());
    }

    #[test]
    fn is_in_range_covers_interior_only() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        s.select_date(d(2024, 6, 20));
        assert!(!s.is_in_range(&d(2024, 6, 10))); // start endpoint
        assert!(!s.is_in_range(&d(2024, 6, 20))); // end endpoint
        assert!(s.is_in_range(&d(2024, 6, 15)));
        assert!(!s.is_in_range(&d(2024, 6, 25)));
    }

    #[test]
    fn is_selected_covers_endpoints_and_interior() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        s.select_date(d(2024, 6, 20));
        assert!(s.is_selected(&d(2024, 6, 10)));
        assert!(s.is_selected(&d(2024, 6, 15)));
        assert!(s.is_selected(&d(2024, 6, 20)));
        assert!(!s.is_selected(&d(2024, 6, 25)));
    }

    #[test]
    fn preview_tracks_focus_forward_and_backward() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        s.set_focused(Some(d(2024, 6, 15)));
        assert!(s.is_in_preview(&d(2024, 6, 12)));
        assert!(s.is_preview_start(&d(2024, 6, 10)));
        assert!(s.is_preview_end(&d(2024, 6, 15)));

        // Focus before start — preview reverses.
        s.set_focused(Some(d(2024, 6, 5)));
        assert!(s.is_in_preview(&d(2024, 6, 8)));
        assert!(s.is_preview_start(&d(2024, 6, 5)));
        assert!(s.is_preview_end(&d(2024, 6, 10)));
    }

    #[test]
    fn preview_off_once_end_committed() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        s.select_date(d(2024, 6, 20));
        s.set_focused(Some(d(2024, 6, 25)));
        assert!(!s.is_in_preview(&d(2024, 6, 22)));
    }

    #[test]
    fn is_invalid_flips_when_end_before_start() {
        // Direct construction — the transition always normalizes,
        // but nothing stops a caller from setting fields manually.
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.start = Some(d(2024, 6, 20));
        s.end = Some(d(2024, 6, 10));
        assert!(s.is_invalid());
    }

    #[test]
    fn clear_resets_everything() {
        let mut s = RangeCalendarState::new(d(2024, 6, 1));
        s.select_date(d(2024, 6, 10));
        s.select_date(d(2024, 6, 20));
        s.set_focused(Some(d(2024, 6, 15)));
        s.clear();
        assert!(s.start.is_none());
        assert!(s.end.is_none());
        assert!(s.focused.is_none());
    }
}
