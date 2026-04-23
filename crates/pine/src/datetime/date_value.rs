//! [`DateValue`] — the pure-Gregorian Rust equivalent of
//! `@internationalized/date`'s `CalendarDate`.
//!
//! Represented as year (i32), month (1..=12), day (1..=31). All
//! arithmetic clips to the last valid day of the target month
//! (Jan 31 + 1 month → Feb 28 / 29), matching both reka's behavior
//! and `@internationalized/date`'s `add` / `subtract` semantics.
//!
//! Backed by [`time::Date`] — small, focused, no_std-friendly.
//! Month arithmetic is open-coded because `time` intentionally
//! doesn't ship a calendar-clipping `+Months` API (avoiding the
//! ambiguity `chrono` hides). Wrapping rather than re-exporting
//! so we can grow this into a sum type in v2 (CalendarDateTime,
//! ZonedDateTime) without breaking callers.

use std::cmp::Ordering;
use std::fmt;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{Date, Duration, Month};

use crate::datetime::types::DayOfWeek;

/// A civil Gregorian date — year, month, day. No time, no timezone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DateValue(Date);

impl DateValue {
    /// Construct from a (year, month, day) triple. `month` is
    /// 1-indexed. Returns `None` for an invalid combination
    /// (e.g. Feb 30) — callers that want reka's clamping behavior
    /// should go through [`DateValue::from_clamped`].
    pub fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        let month = month_from_u8(month)?;
        Date::from_calendar_date(year, month, day).ok().map(Self)
    }

    /// Like [`new`] but clamps `day` to the last valid day of the
    /// target month. Mirrors what reka's `date.set({ day: 100 })`
    /// trick does to probe month length.
    pub fn from_clamped(year: i32, month: u8, day: u8) -> Option<Self> {
        let m = month_from_u8(month)?;
        let max = m.length(year);
        let d = day.min(max).max(1);
        Date::from_calendar_date(year, m, d).ok().map(Self)
    }

    pub const fn year(&self) -> i32 {
        self.0.year()
    }
    pub fn month(&self) -> u8 {
        self.0.month() as u8
    }
    pub const fn day(&self) -> u8 {
        self.0.day()
    }

    /// Sunday = 0 … Saturday = 6. Matches reka's `getDayOfWeek`
    /// called with the implicit `'sun'` anchor.
    pub fn day_of_week(&self) -> DayOfWeek {
        // `number_from_sunday` returns 1..=7, Sunday first.
        let sunday_first = self.0.weekday().number_from_sunday();
        DayOfWeek::from_u8(sunday_first - 1).expect("1..=7")
    }

    /// Underlying [`time::Date`] — escape hatch for interop.
    pub const fn as_time_date(&self) -> Date {
        self.0
    }

    /// Parse an ISO-8601 `YYYY-MM-DD` string via [`time::Date`].
    /// Accepts leading / trailing whitespace for convenience
    /// (authored attrs often include newlines). Returns `None` on
    /// malformed input (wrong shape, invalid date, etc.).
    pub fn parse_iso(s: &str) -> Option<Self> {
        use time::macros::format_description;
        let fmt = format_description!("[year]-[month]-[day]");
        Date::parse(s.trim(), fmt).ok().map(Self)
    }

    // ── arithmetic — all clip end-of-month ──────────────────────

    pub fn add_days(&self, days: i64) -> Self {
        // `time::Duration` stores i64 seconds — more than enough for our day counts.
        let d = Duration::days(days);
        Self(self.0.saturating_add(d))
    }

    pub fn subtract_days(&self, days: i64) -> Self {
        self.add_days(-days)
    }

    pub fn add_months(&self, months: i32) -> Self {
        let total = self.year() as i64 * 12 + (self.month() as i64 - 1) + months as i64;
        let new_year = total.div_euclid(12) as i32;
        let new_month = (total.rem_euclid(12) + 1) as u8;
        let m = month_from_u8(new_month).expect("1..=12");
        let day = self.day().min(m.length(new_year));
        Self(Date::from_calendar_date(new_year, m, day).expect("clipped"))
    }

    pub fn subtract_months(&self, months: i32) -> Self {
        self.add_months(-months)
    }

    pub fn add_years(&self, years: i32) -> Self {
        let new_year = self.year() + years;
        let m = month_from_u8(self.month()).expect("1..=12");
        let day = self.day().min(m.length(new_year));
        Self(Date::from_calendar_date(new_year, m, day).expect("clipped"))
    }

    pub fn subtract_years(&self, years: i32) -> Self {
        self.add_years(-years)
    }

    // ── builders — reka's `.set({ day, month, year })` with clamping ──

    pub fn set_day(&self, day: u8) -> Self {
        let m = month_from_u8(self.month()).expect("1..=12");
        let day = day.min(m.length(self.year())).max(1);
        Self(Date::from_calendar_date(self.year(), m, day).expect("clipped"))
    }

    pub fn set_month(&self, month: u8) -> Self {
        let m = month_from_u8(month.clamp(1, 12)).expect("1..=12");
        let day = self.day().min(m.length(self.year()));
        Self(Date::from_calendar_date(self.year(), m, day).expect("clipped"))
    }

    pub fn set_year(&self, year: i32) -> Self {
        let m = month_from_u8(self.month()).expect("1..=12");
        let day = self.day().min(m.length(year));
        Self(Date::from_calendar_date(year, m, day).expect("clipped"))
    }

    /// JS-style compare — `< 0`, `0`, `> 0`. Mirrors reka's usage
    /// of `a.compare(b) < 0` etc.
    pub fn compare(&self, other: &Self) -> i32 {
        match self.0.cmp(&other.0) {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }
    }
}

impl Ord for DateValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}
impl PartialOrd for DateValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for DateValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // ISO-8601 `YYYY-MM-DD`.
        write!(f, "{:04}-{:02}-{:02}", self.year(), self.month(), self.day())
    }
}

impl Serialize for DateValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DateValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse_iso(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid ISO date `{}`", s.trim()))
        })
    }
}

fn month_from_u8(n: u8) -> Option<Month> {
    match n {
        1 => Some(Month::January),
        2 => Some(Month::February),
        3 => Some(Month::March),
        4 => Some(Month::April),
        5 => Some(Month::May),
        6 => Some(Month::June),
        7 => Some(Month::July),
        8 => Some(Month::August),
        9 => Some(Month::September),
        10 => Some(Month::October),
        11 => Some(Month::November),
        12 => Some(Month::December),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_uses_iso_strings() {
        let d = DateValue::new(2024, 6, 15).unwrap();
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"2024-06-15\"");

        let back: DateValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn serde_rejects_invalid_iso_string() {
        let err = serde_json::from_str::<DateValue>("\"2024-02-30\"").unwrap_err();
        assert!(err.to_string().contains("invalid ISO date"));
    }

    #[test]
    fn ymd_roundtrip() {
        let d = DateValue::new(2024, 2, 29).unwrap();
        assert_eq!(d.year(), 2024);
        assert_eq!(d.month(), 2);
        assert_eq!(d.day(), 29);
        assert_eq!(d.to_string(), "2024-02-29");
    }

    #[test]
    fn add_months_clips_to_end_of_month() {
        let d = DateValue::new(2023, 1, 31).unwrap().add_months(1);
        assert_eq!((d.year(), d.month(), d.day()), (2023, 2, 28));
        let leap = DateValue::new(2024, 1, 31).unwrap().add_months(1);
        assert_eq!((leap.year(), leap.month(), leap.day()), (2024, 2, 29));
    }

    #[test]
    fn add_months_wraps_years() {
        let d = DateValue::new(2024, 11, 15).unwrap().add_months(5);
        assert_eq!((d.year(), d.month(), d.day()), (2025, 4, 15));
        let back = DateValue::new(2024, 2, 15).unwrap().subtract_months(3);
        assert_eq!((back.year(), back.month(), back.day()), (2023, 11, 15));
    }

    #[test]
    fn add_years_clips_leap_day() {
        let leap = DateValue::new(2024, 2, 29).unwrap().add_years(1);
        assert_eq!((leap.year(), leap.month(), leap.day()), (2025, 2, 28));
    }

    #[test]
    fn set_day_clamps_to_month_length() {
        // reka's `date.set({ day: 100 }).day` → last-day-of-month.
        let feb23 = DateValue::new(2023, 2, 5).unwrap().set_day(100);
        assert_eq!(feb23.day(), 28);
        let feb24 = DateValue::new(2024, 2, 5).unwrap().set_day(100);
        assert_eq!(feb24.day(), 29);
    }

    #[test]
    fn day_of_week_is_sunday_first() {
        // 2025-01-05 is a Sunday.
        assert_eq!(
            DateValue::new(2025, 1, 5).unwrap().day_of_week(),
            DayOfWeek::Sunday
        );
        // 2025-01-06 is Monday.
        assert_eq!(
            DateValue::new(2025, 1, 6).unwrap().day_of_week(),
            DayOfWeek::Monday
        );
    }

    #[test]
    fn compare_matches_js_sign_convention() {
        let a = DateValue::new(2024, 1, 1).unwrap();
        let b = DateValue::new(2024, 6, 1).unwrap();
        assert!(a.compare(&b) < 0);
        assert!(b.compare(&a) > 0);
        assert_eq!(a.compare(&a), 0);
    }

    #[test]
    fn parse_iso_roundtrips_display() {
        let d = DateValue::new(2024, 6, 15).unwrap();
        assert_eq!(DateValue::parse_iso(&d.to_string()), Some(d));
        assert_eq!(DateValue::parse_iso("  2024-12-31 "), DateValue::new(2024, 12, 31));
        assert_eq!(DateValue::parse_iso("2024/06/15"), None);
        assert_eq!(DateValue::parse_iso("2024-13-01"), None);
        assert_eq!(DateValue::parse_iso(""), None);
    }
}
