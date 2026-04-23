//! Port of reka's `comparators.ts` — pure predicates and arithmetic
//! helpers over [`DateValue`].
//!
//! All comparisons are inclusive/exclusive variants of the same
//! `compare` call so callers pick the edge behavior they want
//! (`is_before` vs. `is_before_or_same`). Locale-aware helpers in
//! reka (`getLastFirstDayOfWeek` / `getNextLastDayOfWeek`) take a
//! locale string there; here we take an explicit
//! [`WeekStartsOn`] instead (0 = Sunday … 6 = Saturday) — v1 has
//! no locale plumbing, and the explicit int is what the rest of
//! Pine actually passes in today.

use crate::datetime::types::{Matcher, WeekStartsOn};
use crate::datetime::DateValue;
use time::Month;

// ── single comparisons ─────────────────────────────────────────

pub fn is_before(a: &DateValue, r: &DateValue) -> bool {
    a.compare(r) < 0
}
pub fn is_after(a: &DateValue, r: &DateValue) -> bool {
    a.compare(r) > 0
}
pub fn is_before_or_same(a: &DateValue, r: &DateValue) -> bool {
    a.compare(r) <= 0
}
pub fn is_after_or_same(a: &DateValue, r: &DateValue) -> bool {
    a.compare(r) >= 0
}

pub fn is_between(date: &DateValue, start: &DateValue, end: &DateValue) -> bool {
    is_after(date, start) && is_before(date, end)
}

pub fn is_between_inclusive(date: &DateValue, start: &DateValue, end: &DateValue) -> bool {
    is_after_or_same(date, start) && is_before_or_same(date, end)
}

// ── year / month coarse comparisons ────────────────────────────

pub fn is_same_year(a: &DateValue, b: &DateValue) -> bool {
    a.year() == b.year()
}

pub fn is_same_year_month(a: &DateValue, b: &DateValue) -> bool {
    a.year() == b.year() && a.month() == b.month()
}

/// `@internationalized/date`-style alias used directly by reka's
/// `useCalendar.ts`. In the single-calendar v1 both operands are
/// always Gregorian, so this just delegates to [`is_same_year_month`].
pub fn is_same_month(a: &DateValue, b: &DateValue) -> bool {
    is_same_year_month(a, b)
}

/// Reka's `useCalendar.ts` uses `isEqualMonth` interchangeably with
/// `isSameMonth`. Kept as a separate alias so call sites read the
/// same as the TS source.
pub fn is_equal_month(a: &DateValue, b: &DateValue) -> bool {
    is_same_year_month(a, b)
}

/// True when `a` and `b` point at the same civil day. For the
/// date-only v1 this is plain equality; the alias exists so v2
/// `CalendarDateTime` / `ZonedDateTime` values can strip their
/// time components without callers changing.
pub fn is_same_day(a: &DateValue, b: &DateValue) -> bool {
    a == b
}

pub fn compare_year_month(a: &DateValue, b: &DateValue) -> i32 {
    if a.year() != b.year() {
        a.year() - b.year()
    } else {
        a.month() as i32 - b.month() as i32
    }
}

pub fn is_month_between_inclusive(date: &DateValue, start: &DateValue, end: &DateValue) -> bool {
    compare_year_month(date, start) >= 0 && compare_year_month(date, end) <= 0
}

pub fn is_year_between_inclusive(date: &DateValue, start: &DateValue, end: &DateValue) -> bool {
    date.year() >= start.year() && date.year() <= end.year()
}

pub fn get_months_between(start: &DateValue, end: &DateValue) -> i32 {
    (end.year() - start.year()) * 12 + (end.month() as i32 - start.month() as i32) + 1
}

pub fn get_years_between(start: &DateValue, end: &DateValue) -> i32 {
    end.year() - start.year() + 1
}

// ── month length ───────────────────────────────────────────────

/// Number of days in the month containing `date`.
pub fn get_days_in_month(date: &DateValue) -> u8 {
    month_from_u8(date.month()).length(date.year())
}

// ── week-start alignment ───────────────────────────────────────

/// Walk `date` back to the most recent occurrence of
/// `first_day_of_week` (inclusive — returns `date` unchanged if it
/// already lands on that weekday).
pub fn get_last_first_day_of_week(date: &DateValue, first_day_of_week: WeekStartsOn) -> DateValue {
    let day = date.day_of_week().as_u8();
    if first_day_of_week > day {
        date.subtract_days((day + 7 - first_day_of_week) as i64)
    } else if first_day_of_week == day {
        *date
    } else {
        date.subtract_days((day - first_day_of_week) as i64)
    }
}

/// Walk `date` forward to the last day of its week, where the week
/// starts on `first_day_of_week`. Matches reka's
/// `getNextLastDayOfWeek`.
pub fn get_next_last_day_of_week(date: &DateValue, first_day_of_week: WeekStartsOn) -> DateValue {
    let day = date.day_of_week().as_u8();
    let last = if first_day_of_week == 0 {
        6
    } else {
        first_day_of_week - 1
    };

    if day == last {
        *date
    } else if day > last {
        date.add_days((7 - day + last) as i64)
    } else {
        date.add_days((last - day) as i64)
    }
}

// ── bulk validity scans ────────────────────────────────────────

/// Returns `true` iff every day strictly between `start` and `end`
/// is valid — i.e. not disabled / not unavailable, unless
/// `is_highlightable` rescues it. Matches reka's
/// `areAllDaysBetweenValid` semantics including the quirk that
/// `start` itself is skipped and `end` is included.
pub fn are_all_days_between_valid(
    start: &DateValue,
    end: &DateValue,
    is_unavailable: Option<&Matcher>,
    is_disabled: Option<&Matcher>,
    is_highlightable: Option<&Matcher>,
) -> bool {
    if is_unavailable.is_none() && is_disabled.is_none() && is_highlightable.is_none() {
        return true;
    }
    let bad = |d: &DateValue| -> bool {
        let disabled = is_disabled.map_or(false, |f| f(d));
        let unavailable = is_unavailable.map_or(false, |f| f(d));
        let saved = is_highlightable.map_or(false, |f| f(d));
        (disabled || unavailable) && !saved
    };

    let mut cur = start.add_days(1);
    if bad(&cur) {
        return false;
    }
    while cur.compare(end) < 0 {
        cur = cur.add_days(1);
        if bad(&cur) {
            return false;
        }
    }
    true
}

pub fn are_all_months_between_valid(
    start: &DateValue,
    end: &DateValue,
    is_unavailable: Option<&Matcher>,
    is_disabled: Option<&Matcher>,
) -> bool {
    if is_unavailable.is_none() && is_disabled.is_none() {
        return true;
    }
    let mut cur = start.set_day(1);
    let end_month = end.set_day(1);
    while compare_year_month(&cur, &end_month) <= 0 {
        if is_disabled.map_or(false, |f| f(&cur)) || is_unavailable.map_or(false, |f| f(&cur)) {
            return false;
        }
        cur = cur.add_months(1);
    }
    true
}

pub fn are_all_years_between_valid(
    start: &DateValue,
    end: &DateValue,
    is_unavailable: Option<&Matcher>,
    is_disabled: Option<&Matcher>,
) -> bool {
    if is_unavailable.is_none() && is_disabled.is_none() {
        return true;
    }
    let mut cur = start.set_day(1).set_month(1);
    let end_year = end.year();
    while cur.year() <= end_year {
        if is_disabled.map_or(false, |f| f(&cur)) || is_unavailable.map_or(false, |f| f(&cur)) {
            return false;
        }
        cur = cur.add_years(1);
    }
    true
}

// ── internal ───────────────────────────────────────────────────

fn month_from_u8(n: u8) -> Month {
    match n {
        1 => Month::January,
        2 => Month::February,
        3 => Month::March,
        4 => Month::April,
        5 => Month::May,
        6 => Month::June,
        7 => Month::July,
        8 => Month::August,
        9 => Month::September,
        10 => Month::October,
        11 => Month::November,
        12 => Month::December,
        _ => unreachable!("DateValue invariants guarantee 1..=12"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u8, day: u8) -> DateValue {
        DateValue::new(y, m, day).unwrap()
    }

    #[test]
    fn before_after_variants() {
        let a = d(2024, 6, 1);
        let b = d(2024, 6, 15);
        assert!(is_before(&a, &b));
        assert!(is_after(&b, &a));
        assert!(is_before_or_same(&a, &a));
        assert!(is_after_or_same(&a, &a));
    }

    #[test]
    fn between_variants() {
        let start = d(2024, 1, 1);
        let mid = d(2024, 6, 15);
        let end = d(2024, 12, 31);
        assert!(is_between(&mid, &start, &end));
        assert!(!is_between(&start, &start, &end));
        assert!(is_between_inclusive(&start, &start, &end));
    }

    #[test]
    fn days_in_month_matches_calendar() {
        assert_eq!(get_days_in_month(&d(2024, 2, 1)), 29); // leap
        assert_eq!(get_days_in_month(&d(2023, 2, 1)), 28);
        assert_eq!(get_days_in_month(&d(2024, 4, 1)), 30);
        assert_eq!(get_days_in_month(&d(2024, 7, 1)), 31);
    }

    #[test]
    fn last_first_day_walks_back_to_week_start() {
        // 2025-01-06 is Monday. With Sunday-first (0), last first
        // day is 2025-01-05.
        let mon = d(2025, 1, 6);
        assert_eq!(get_last_first_day_of_week(&mon, 0), d(2025, 1, 5));
        // With Monday-first (1), it returns itself.
        assert_eq!(get_last_first_day_of_week(&mon, 1), mon);
    }

    #[test]
    fn next_last_day_walks_to_week_end() {
        // 2025-01-06 Monday, Sunday-first week ends Saturday
        // (2025-01-11).
        let mon = d(2025, 1, 6);
        assert_eq!(get_next_last_day_of_week(&mon, 0), d(2025, 1, 11));
        // Monday-first week ends Sunday (2025-01-12).
        assert_eq!(get_next_last_day_of_week(&mon, 1), d(2025, 1, 12));
    }

    #[test]
    fn year_month_comparisons() {
        let a = d(2023, 6, 15);
        let b = d(2024, 2, 1);
        let c = d(2024, 6, 30);
        assert!(compare_year_month(&a, &b) < 0);
        assert!(compare_year_month(&b, &c) < 0);
        assert!(compare_year_month(&a, &c) < 0);
        assert!(is_same_year_month(&b, &d(2024, 2, 20)));
        assert!(!is_same_year_month(&b, &c));
        assert!(is_month_between_inclusive(&b, &a, &c));
    }

    #[test]
    fn calendar_aliases_delegate_correctly() {
        let jan1 = d(2024, 1, 1);
        let jan31 = d(2024, 1, 31);
        let feb1 = d(2024, 2, 1);
        // Same day / same month aliases used by useCalendar.
        assert!(is_same_day(&jan1, &jan1));
        assert!(!is_same_day(&jan1, &jan31));
        assert!(is_same_month(&jan1, &jan31));
        assert!(is_equal_month(&jan1, &jan31));
        assert!(!is_same_month(&jan31, &feb1));
    }
}
