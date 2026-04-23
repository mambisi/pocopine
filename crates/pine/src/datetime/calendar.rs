//! Port of reka's `calendar.ts` — grid builders for month /
//! year / decade calendar layouts, plus ISO + US week-number
//! resolution.
//!
//! Locale-aware week-start derivation is deferred — v1 callers
//! pass an explicit [`WeekStartsOn`]. [`get_week_number`] accepts
//! a [`WeekMode`] enum (ISO / US) instead of reka's `locale`
//! string, with direct shortcuts [`get_week_number_iso`] /
//! [`get_week_number_us`].

use crate::datetime::comparators::{
    get_days_in_month, get_last_first_day_of_week, get_next_last_day_of_week,
};
use crate::datetime::types::{DateRange, Grid, WeekStartsOn};
use crate::datetime::utils::chunk;
use crate::datetime::DateValue;

// ── month / year / decade anchors ──────────────────────────────

pub fn start_of_month(d: &DateValue) -> DateValue {
    d.set_day(1)
}

pub fn end_of_month(d: &DateValue) -> DateValue {
    d.set_day(get_days_in_month(d))
}

pub fn start_of_year(d: &DateValue) -> DateValue {
    d.set_month(1).set_day(1)
}

pub fn end_of_year(d: &DateValue) -> DateValue {
    // December has 31 days every year — no further clamping needed.
    d.set_month(12).set_day(31)
}

/// First day of the week containing `date`, where the week starts
/// on `week_starts_on` (0 = Sunday … 6 = Saturday).
pub fn start_of_week(date: &DateValue, week_starts_on: WeekStartsOn) -> DateValue {
    get_last_first_day_of_week(date, week_starts_on)
}

/// First day of the decade containing `d` — e.g. 2023 → 2020-01-01.
/// Matches reka's rounding: `floor(year / 10) * 10`.
pub fn start_of_decade(d: &DateValue) -> DateValue {
    let start_year = (d.year() / 10) * 10;
    start_of_year(&d.set_year(start_year))
}

/// Last day of the decade containing `d` — e.g. 2023 → 2029-12-31.
pub fn end_of_decade(d: &DateValue) -> DateValue {
    let end_year = (d.year() / 10) * 10 + 9;
    end_of_year(&d.set_year(end_year))
}

// ── range enumerators ──────────────────────────────────────────

/// Days strictly between `start` and `end` (both exclusive).
/// Mirrors reka's `getDaysBetween`.
pub fn get_days_between(start: &DateValue, end: &DateValue) -> Vec<DateValue> {
    let mut out = Vec::new();
    let mut cur = start.add_days(1);
    while cur.compare(end) < 0 {
        out.push(cur);
        cur = cur.add_days(1);
    }
    out
}

pub fn create_year_range(range: &DateRange) -> Vec<DateValue> {
    let Some(start) = range.start else { return Vec::new() };
    let Some(end) = range.end else { return Vec::new() };
    let mut out = Vec::new();
    let mut cur = start_of_year(&start);
    while cur.compare(&end) <= 0 {
        out.push(cur);
        cur = start_of_year(&cur.add_years(1));
    }
    out
}

pub fn create_date_range(range: &DateRange) -> Vec<DateValue> {
    let Some(start) = range.start else { return Vec::new() };
    let Some(end) = range.end else { return Vec::new() };
    let mut out = Vec::new();
    let mut cur = start;
    while cur.compare(&end) <= 0 {
        out.push(cur);
        cur = cur.add_days(1);
    }
    out
}

// ── decade / year pickers ──────────────────────────────────────

/// Build a contiguous decade window centered loosely on `date`.
/// `start_index` defaults to 0, `end_index` is the (exclusive)
/// upper bound: the total length is `|start_index| + end_index`
/// years, with indices `< |start_index|` subtracted from `date`
/// and indices `≥ |start_index|` added.
///
/// Ported from reka-ui's `createDecade`. The math expressed as a
/// single offset `k = i − |start_index|`: negative `k` subtracts
/// `|k|` years, non-negative `k` adds `k`. That way the output is
/// monotone ascending by construction, so no post-sort is needed.
///
/// Examples:
///   - `create_decade(date, None, 5)` →
///     `[date, date+1, date+2, date+3, date+4]`
///   - `create_decade(date, Some(-3), 4)` →
///     `[date-3, date-2, date-1, date, date+1, date+2, date+3]`
pub fn create_decade(
    date: &DateValue,
    start_index: Option<i32>,
    end_index: i32,
) -> Vec<DateValue> {
    let start_index = start_index.unwrap_or(0);
    let abs_start = start_index.unsigned_abs() as i32;
    let total = abs_start + end_index;
    (0..total)
        .map(|i| {
            let offset = i - abs_start;
            let anchor = if offset < 0 {
                date.subtract_years(-offset)
            } else {
                date.add_years(offset)
            };
            anchor.set_day(1).set_month(1)
        })
        .collect()
}

/// Twelve first-of-months for a year, or a `numberOfMonths`-step
/// paginated subset when `paged_navigation` is true.
pub fn create_year(
    date: &DateValue,
    number_of_months: u32,
    paged_navigation: bool,
) -> Vec<DateValue> {
    let n = number_of_months.max(1);
    if paged_navigation && n > 1 {
        let pages = 12 / n;
        return (0..pages)
            .map(|i| start_of_month(&date.set_month((i * n + 1) as u8)))
            .collect();
    }
    (1..=12)
        .map(|m| start_of_month(&date.set_month(m)))
        .collect()
}

/// 3x4 grid of months for a year.
pub fn create_month_grid(date: &DateValue) -> Grid<DateValue> {
    let months = create_year(date, 1, false);
    let rows = chunk(&months, 4);
    Grid {
        value: *date,
        cells: months,
        rows,
    }
}

/// 3x4 grid of years (decade-aligned by default).
pub fn create_year_grid(
    date: &DateValue,
    years_per_page: u32,
    decade_aligned: bool,
) -> Grid<DateValue> {
    let years_per_page = years_per_page.max(1);
    let start_year = if decade_aligned {
        start_of_decade(date).year()
    } else {
        date.year()
    };
    let years: Vec<DateValue> = (0..years_per_page)
        .map(|i| start_of_year(&date.set_year(start_year + i as i32)))
        .collect();
    let first_year = *years.first().expect("years_per_page >= 1");
    let rows = chunk(&years, 4);
    Grid {
        value: first_year,
        cells: years,
        rows,
    }
}

// ── month grid ─────────────────────────────────────────────────

pub struct CreateMonthProps<'a> {
    pub date: &'a DateValue,
    pub week_starts_on: WeekStartsOn,
    pub fixed_weeks: bool,
}

/// Build a calendar grid for the month containing `date`.
/// The grid is padded with trailing days from the previous month
/// and leading days from the next month so each row is a full
/// week starting on `week_starts_on`.
pub fn create_month(props: CreateMonthProps<'_>) -> Grid<DateValue> {
    let CreateMonthProps {
        date,
        week_starts_on,
        fixed_weeks,
    } = props;

    let days_in_month = get_days_in_month(date);
    let dates: Vec<DateValue> = (1..=days_in_month).map(|d| date.set_day(d)).collect();

    let first_day = start_of_month(date);
    let last_day = end_of_month(date);
    let last_prev_week_start = get_last_first_day_of_week(&first_day, week_starts_on);
    let next_week_end = get_next_last_day_of_week(&last_day, week_starts_on);

    let last_month_days = get_days_between(&last_prev_week_start.subtract_days(1), &first_day);
    let mut next_month_days = get_days_between(&last_day, &next_week_end.add_days(1));

    let total = last_month_days.len() + dates.len() + next_month_days.len();
    if fixed_weeks && total < 42 {
        let extra = 42 - total;
        let mut start_from = next_month_days.last().copied().unwrap_or_else(|| end_of_month(date));
        for _ in 0..extra {
            start_from = start_from.add_days(1);
            next_month_days.push(start_from);
        }
    }

    let mut all: Vec<DateValue> = Vec::with_capacity(last_month_days.len() + dates.len() + next_month_days.len());
    all.extend(last_month_days);
    all.extend(dates);
    all.extend(next_month_days);

    let rows = chunk(&all, 7);
    Grid {
        value: *date,
        cells: all,
        rows,
    }
}

pub struct CreateMonthsProps<'a> {
    pub date: &'a DateValue,
    pub week_starts_on: WeekStartsOn,
    pub fixed_weeks: bool,
    /// `1` = just the current month. `> 1` renders that many
    /// consecutive months starting with the current one.
    pub number_of_months: u32,
}

pub fn create_months(props: CreateMonthsProps<'_>) -> Vec<Grid<DateValue>> {
    let CreateMonthsProps {
        date,
        week_starts_on,
        fixed_weeks,
        number_of_months,
    } = props;
    let n = number_of_months.max(1);
    let mut out = Vec::with_capacity(n as usize);
    out.push(create_month(CreateMonthProps {
        date,
        week_starts_on,
        fixed_weeks,
    }));
    for i in 1..n {
        let next = date.add_months(i as i32);
        out.push(create_month(CreateMonthProps {
            date: &next,
            week_starts_on,
            fixed_weeks,
        }));
    }
    out
}

// ── week start / week number ───────────────────────────────────

/// Returns `WeekStartsOn::Sunday` — v1 has no locale plumbing so
/// we default to the US convention. Calendars that want a
/// different start day should pass it explicitly.
///
/// Accepted for signature parity with reka; v2 will consult the
/// author's `Intl.Locale` via a `wasm-bindgen` bridge.
pub fn get_week_starts_on(_locale: &str) -> WeekStartsOn {
    0
}

/// Week-number convention — see [`get_week_number`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekMode {
    /// ISO 8601: week starts Monday; week 1 contains January 4.
    /// Matches `de-DE`, `fr-FR`, most of Europe.
    Iso,
    /// US convention: week starts Sunday; week 1 contains January 1.
    /// Matches `en-US`.
    Us,
}

pub fn get_week_number_iso(date: &DateValue) -> u32 {
    get_week_number(date, WeekMode::Iso)
}

pub fn get_week_number_us(date: &DateValue) -> u32 {
    get_week_number(date, WeekMode::Us)
}

/// Week-of-year for `date`, using the `mode` convention. Port of
/// reka's `getWeekNumber` algorithm.
pub fn get_week_number(date: &DateValue, mode: WeekMode) -> u32 {
    let (week_starts_on, first_week_contains_date) = match mode {
        WeekMode::Iso => (1u8, 4i32),
        WeekMode::Us => (0u8, 1i32),
    };

    // reka: `dayOfWeek = getDayOfWeek(date, locale, weekStartsOn)`
    // — the weekday position relative to the week's start day.
    let day_of_week = relative_day_of_week(date, week_starts_on);

    // The "deciding day" — its year determines which year's
    // numbering we use. Shifts the cursor into the week.
    let deciding_day = date.add_days((7 - first_week_contains_date - day_of_week as i32) as i64);
    let week_year = deciding_day.year();

    // Week 1 anchor = (weekYear, Jan, firstWeekContainsDate).
    let week1_ref =
        DateValue::new(week_year, 1, first_week_contains_date as u8).expect("1..=31 + Jan");
    let week1_start = start_of_week(&week1_ref, week_starts_on);
    let current_week_start = start_of_week(date, week_starts_on);

    let diff_days = (current_week_start.as_time_date() - week1_start.as_time_date()).whole_days();
    let week_diff = (diff_days as f64 / 7.0).round() as i64;
    (week_diff + 1) as u32
}

fn relative_day_of_week(date: &DateValue, week_starts_on: WeekStartsOn) -> u8 {
    // Sunday=0 .. Saturday=6, rotated so the week start is 0.
    let sunday_first = date.day_of_week().as_u8();
    (sunday_first + 7 - week_starts_on) % 7
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u8, day: u8) -> DateValue {
        DateValue::new(y, m, day).unwrap()
    }

    #[test]
    fn start_end_of_month_work() {
        let anchor = d(2024, 2, 15);
        assert_eq!(start_of_month(&anchor), d(2024, 2, 1));
        assert_eq!(end_of_month(&anchor), d(2024, 2, 29));
        assert_eq!(end_of_month(&d(2023, 2, 15)), d(2023, 2, 28));
    }

    #[test]
    fn decade_anchors() {
        assert_eq!(start_of_decade(&d(2023, 5, 12)), d(2020, 1, 1));
        assert_eq!(end_of_decade(&d(2023, 5, 12)), d(2029, 12, 31));
        assert_eq!(start_of_decade(&d(2020, 1, 1)), d(2020, 1, 1));
        assert_eq!(end_of_decade(&d(2029, 12, 31)), d(2029, 12, 31));
    }

    #[test]
    fn month_grid_has_42_cells_when_fixed() {
        let grid = create_month(CreateMonthProps {
            date: &d(2024, 2, 15),
            week_starts_on: 0,
            fixed_weeks: true,
        });
        assert_eq!(grid.cells.len(), 42);
        assert_eq!(grid.rows.len(), 6);
        assert!(grid.rows.iter().all(|r| r.len() == 7));
    }

    #[test]
    fn month_grid_respects_week_starts_on() {
        // Feb 2024 starts Thursday. Sunday-first → grid begins
        // 2024-01-28 (Sunday). Monday-first → 2024-01-29.
        let sun = create_month(CreateMonthProps {
            date: &d(2024, 2, 1),
            week_starts_on: 0,
            fixed_weeks: false,
        });
        assert_eq!(sun.cells[0], d(2024, 1, 28));

        let mon = create_month(CreateMonthProps {
            date: &d(2024, 2, 1),
            week_starts_on: 1,
            fixed_weeks: false,
        });
        assert_eq!(mon.cells[0], d(2024, 1, 29));
    }

    #[test]
    fn create_months_walks_forward() {
        let months = create_months(CreateMonthsProps {
            date: &d(2024, 1, 15),
            week_starts_on: 0,
            fixed_weeks: true,
            number_of_months: 3,
        });
        assert_eq!(months.len(), 3);
        assert_eq!(months[0].value.month(), 1);
        assert_eq!(months[1].value.month(), 2);
        assert_eq!(months[2].value.month(), 3);
    }

    #[test]
    fn year_grid_is_decade_aligned_by_default() {
        let g = create_year_grid(&d(2023, 6, 1), 12, true);
        assert_eq!(g.value.year(), 2020);
        assert_eq!(g.cells.first().unwrap().year(), 2020);
        assert_eq!(g.cells.last().unwrap().year(), 2031);
        assert_eq!(g.rows.len(), 3);
    }

    #[test]
    fn date_range_enumerates_inclusive() {
        let r = DateRange {
            start: Some(d(2024, 1, 1)),
            end: Some(d(2024, 1, 5)),
        };
        let days = create_date_range(&r);
        assert_eq!(days.len(), 5);
        assert_eq!(days.first().unwrap(), &d(2024, 1, 1));
        assert_eq!(days.last().unwrap(), &d(2024, 1, 5));
    }

    // Parity with reka's `calendar.test.ts` — every expectation
    // here must match what @internationalized/date computes for
    // the same input.
    #[test]
    fn week_number_iso_parity() {
        let cases: &[(i32, u8, u8, u32)] = &[
            (2025, 12, 31, 1),
            (2025, 12, 29, 1),
            (2025, 12, 28, 52),
            (2024, 12, 31, 1),
            (2024, 1, 1, 1),
            (2023, 1, 1, 52),
            (2023, 1, 2, 1),
            (2021, 1, 4, 1),
            (2020, 12, 31, 53),
            (2005, 1, 2, 53),
            (2008, 12, 29, 1),
            (2016, 1, 1, 53),
            (2016, 5, 1, 17),
            (2016, 5, 2, 18),
            (2016, 5, 31, 22),
            (2014, 7, 14, 29),
        ];
        for &(y, m, day, expected) in cases {
            assert_eq!(
                get_week_number_iso(&d(y, m, day)),
                expected,
                "ISO week for {}-{:02}-{:02}",
                y,
                m,
                day
            );
        }
    }

    #[test]
    fn week_number_us_parity() {
        let cases: &[(i32, u8, u8, u32)] = &[
            (2025, 12, 31, 1),
            (2025, 1, 1, 1),
            (2025, 1, 4, 1),
            (2005, 1, 2, 2),
            (2005, 1, 4, 2),
            (2008, 12, 29, 1),
        ];
        for &(y, m, day, expected) in cases {
            assert_eq!(
                get_week_number_us(&d(y, m, day)),
                expected,
                "US week for {}-{:02}-{:02}",
                y,
                m,
                day
            );
        }
    }

    #[test]
    fn create_decade_default_walks_forward_from_anchor() {
        // reka-ui parity: default call shape is "forward-only window
        // starting AT the anchor". Previous implementation walked
        // backwards and produced [date-4, date-3, date-2, date-1, date].
        let years: Vec<i32> = create_decade(&d(2023, 1, 1), None, 5)
            .into_iter()
            .map(|v| v.year())
            .collect();
        assert_eq!(years, vec![2023, 2024, 2025, 2026, 2027]);
    }

    #[test]
    fn create_decade_negative_start_wraps_around_anchor() {
        // reka-ui parity: `Some(-3), 4` yields 7 distinct ascending
        // years centred on the anchor. Previous impl double-counted
        // the anchor (off-by-one pivot) and dropped `date+3`.
        let years: Vec<i32> = create_decade(&d(2023, 1, 1), Some(-3), 4)
            .into_iter()
            .map(|v| v.year())
            .collect();
        assert_eq!(
            years,
            vec![2020, 2021, 2022, 2023, 2024, 2025, 2026]
        );
        assert_eq!(years.len(), 7);
    }
}
