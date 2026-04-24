//! Three-segment (month / day / year) editable state for
//! `<pine-date-field>`. Pure logic — no DOM, no reactivity — so
//! the keyboard semantics can be unit-tested on the host.
//!
//! Scope relative to reka's `useDateField`:
//! - Format order is fixed `MM / DD / YYYY`. Locale-aware orders
//!   (`DD / MM / YYYY`, `YYYY / MM / DD`) are deferred.
//! - No time segments, no timezone, no granularity. The time
//!   field lives in its own module.

use crate::datetime::DateValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatePart {
    Month,
    Day,
    Year,
}

impl DatePart {
    pub const ALL: [DatePart; 3] = [DatePart::Month, DatePart::Day, DatePart::Year];

    pub fn as_str(self) -> &'static str {
        match self {
            DatePart::Month => "month",
            DatePart::Day => "day",
            DatePart::Year => "year",
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            DatePart::Month => "MM",
            DatePart::Day => "DD",
            DatePart::Year => "YYYY",
        }
    }

    pub fn max_len(self) -> usize {
        match self {
            DatePart::Month | DatePart::Day => 2,
            DatePart::Year => 4,
        }
    }

    /// The next segment in the fixed MM/DD/YYYY order, or `None`
    /// at the tail. Used by auto-advance logic after a segment
    /// reaches its max length.
    pub fn next(self) -> Option<DatePart> {
        match self {
            DatePart::Month => Some(DatePart::Day),
            DatePart::Day => Some(DatePart::Year),
            DatePart::Year => None,
        }
    }

    pub fn prev(self) -> Option<DatePart> {
        match self {
            DatePart::Month => None,
            DatePart::Day => Some(DatePart::Month),
            DatePart::Year => Some(DatePart::Day),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DateSegments {
    pub month: Option<u8>,
    pub day: Option<u8>,
    pub year: Option<i32>,
}

impl DateSegments {
    pub fn from_date(d: DateValue) -> Self {
        Self {
            month: Some(d.month()),
            day: Some(d.day()),
            year: Some(d.year()),
        }
    }

    /// Compose a `DateValue` when every segment is set and the
    /// combination forms a valid Gregorian date.
    pub fn to_date(&self) -> Option<DateValue> {
        let year = self.year?;
        let month = self.month?;
        let day = self.day?;
        DateValue::new(year, month, day)
    }

    pub fn is_complete(&self) -> bool {
        self.to_date().is_some()
    }

    pub fn clear(&mut self) {
        self.month = None;
        self.day = None;
        self.year = None;
    }

    pub fn clear_part(&mut self, part: DatePart) {
        match part {
            DatePart::Month => self.month = None,
            DatePart::Day => self.day = None,
            DatePart::Year => self.year = None,
        }
    }

    /// Rendered text for a segment — either the digits (zero-
    /// padded for month/day, 4-digit for year) or the part's
    /// placeholder when unset.
    pub fn display(&self, part: DatePart) -> String {
        match part {
            DatePart::Month => match self.month {
                Some(m) => format!("{m:02}"),
                None => part.placeholder().into(),
            },
            DatePart::Day => match self.day {
                Some(d) => format!("{d:02}"),
                None => part.placeholder().into(),
            },
            DatePart::Year => match self.year {
                Some(y) => format!("{:04}", y.max(0)),
                None => part.placeholder().into(),
            },
        }
    }

    /// Append a digit to `part`. Semantics roughly match reka's
    /// `useDateField`:
    ///
    /// - An empty segment keeps the digit as a fresh value.
    /// - A partial segment appends the digit, then caps to the
    ///   valid range (12 / 31 / 9999). If the appended value
    ///   would overflow, the digit becomes the new first digit
    ///   (typing 5 on month 7 → 5 (invalid 75 rollover), typing
    ///   3 on day 25 → 3 (invalid 253)).
    /// - Returns `true` if the segment just filled its last slot
    ///   (caller should advance focus).
    pub fn type_digit(&mut self, part: DatePart, d: u8) -> bool {
        if d > 9 {
            return false;
        }
        match part {
            DatePart::Month => type_digit_u8(&mut self.month, d, 12),
            DatePart::Day => type_digit_u8(&mut self.day, d, days_cap(self.month, self.year)),
            DatePart::Year => type_digit_year(&mut self.year, d),
        }
    }

    /// Backspace strips the last digit from `part`, or clears
    /// it outright when only one digit remains.
    pub fn backspace(&mut self, part: DatePart) {
        match part {
            DatePart::Month => backspace_u8(&mut self.month),
            DatePart::Day => backspace_u8(&mut self.day),
            DatePart::Year => backspace_year(&mut self.year),
        }
    }

    /// Arrow-up / scroll-up — `+1` with wrap.
    pub fn increment(&mut self, part: DatePart) {
        match part {
            DatePart::Month => {
                let cur = self.month.unwrap_or(0);
                self.month = Some(if cur >= 12 { 1 } else { cur + 1 });
            }
            DatePart::Day => {
                let cap = days_cap(self.month, self.year);
                let cur = self.day.unwrap_or(0);
                self.day = Some(if cur >= cap { 1 } else { cur + 1 });
                self.clamp_day();
            }
            DatePart::Year => {
                let cur = self.year.unwrap_or(fallback_today_year() - 1);
                self.year = Some(cur + 1);
                self.clamp_day();
            }
        }
    }

    /// Arrow-down / scroll-down — `-1` with wrap.
    pub fn decrement(&mut self, part: DatePart) {
        match part {
            DatePart::Month => {
                let cur = self.month.unwrap_or(13);
                self.month = Some(if cur <= 1 { 12 } else { cur - 1 });
                self.clamp_day();
            }
            DatePart::Day => {
                let cap = days_cap(self.month, self.year);
                let cur = self.day.unwrap_or(cap + 1);
                self.day = Some(if cur <= 1 { cap } else { cur - 1 });
            }
            DatePart::Year => {
                let cur = self.year.unwrap_or(fallback_today_year() + 1);
                self.year = Some(cur - 1);
                self.clamp_day();
            }
        }
    }

    /// If month / year shifted under `day`, pull day down to the
    /// new month's length (Jan 31 + month→Feb = Feb 28/29).
    fn clamp_day(&mut self) {
        if let Some(d) = self.day {
            let cap = days_cap(self.month, self.year);
            if d > cap {
                self.day = Some(cap);
            }
        }
    }
}

// ── internal helpers ───────────────────────────────────────────

fn type_digit_u8(slot: &mut Option<u8>, d: u8, cap: u8) -> bool {
    match *slot {
        None => {
            *slot = Some(d);
            // "0" alone doesn't commit the segment — but a single
            // digit ≥ cap/10 + 1 (e.g. 4 with cap 31) fills two
            // slots in one press.
            d > (cap / 10)
        }
        Some(cur) => {
            let combined = cur as u16 * 10 + d as u16;
            if combined == 0 || combined > cap as u16 {
                // Roll over — start fresh with this digit.
                *slot = Some(d);
                d > (cap / 10)
            } else {
                *slot = Some(combined as u8);
                true
            }
        }
    }
}

fn type_digit_year(slot: &mut Option<i32>, d: u8) -> bool {
    let d = d as i32;
    match *slot {
        None => {
            *slot = Some(d);
            false
        }
        Some(cur) if cur < 1000 => {
            let combined = cur * 10 + d;
            *slot = Some(combined);
            combined >= 1000
        }
        Some(_) => {
            // Already 4 digits — restart.
            *slot = Some(d);
            false
        }
    }
}

fn backspace_u8(slot: &mut Option<u8>) {
    match *slot {
        None => {}
        Some(v) if v < 10 => *slot = None,
        Some(v) => *slot = Some(v / 10),
    }
}

fn backspace_year(slot: &mut Option<i32>) {
    match *slot {
        None => {}
        Some(v) if v < 10 => *slot = None,
        Some(v) => *slot = Some(v / 10),
    }
}

fn days_cap(month: Option<u8>, year: Option<i32>) -> u8 {
    match (month, year) {
        (Some(m), Some(y)) => days_in_month_for(y, m),
        // Fallback: 31 so leading-digit checks still work when
        // the user hasn't committed a month yet.
        _ => 31,
    }
}

fn days_in_month_for(year: i32, month: u8) -> u8 {
    use time::Month;
    let m = match month {
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
        _ => return 31,
    };
    m.length(year)
}

fn fallback_today_year() -> i32 {
    // Only used as an anchor when the user hits arrow-up/down on
    // an empty year segment. 2024 is a reasonable modern default
    // on host builds; wasm has js_sys::Date but this stays pure
    // for the unit tests.
    2024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_segments_are_incomplete() {
        let s = DateSegments::default();
        assert!(!s.is_complete());
        assert_eq!(s.to_date(), None);
    }

    #[test]
    fn from_date_round_trip() {
        let d = DateValue::new(2024, 6, 15).unwrap();
        let s = DateSegments::from_date(d);
        assert_eq!(s.month, Some(6));
        assert_eq!(s.day, Some(15));
        assert_eq!(s.year, Some(2024));
        assert_eq!(s.to_date(), Some(d));
    }

    #[test]
    fn display_zero_pads_and_placeholders() {
        let mut s = DateSegments::default();
        assert_eq!(s.display(DatePart::Month), "MM");
        assert_eq!(s.display(DatePart::Day), "DD");
        assert_eq!(s.display(DatePart::Year), "YYYY");
        s.month = Some(3);
        s.day = Some(5);
        s.year = Some(2024);
        assert_eq!(s.display(DatePart::Month), "03");
        assert_eq!(s.display(DatePart::Day), "05");
        assert_eq!(s.display(DatePart::Year), "2024");
    }

    #[test]
    fn type_digit_month_auto_advance_on_cap_leader() {
        let mut s = DateSegments::default();
        // "2" can still be start of "20" ... no, wait — month cap
        // is 12. "2" leading → can still be 20? 12 is cap.
        // 2 ≤ 12 / 10 = 1, so 2 > 1 → autoadvance true.
        assert!(s.type_digit(DatePart::Month, 2));
        assert_eq!(s.month, Some(2));

        // Reset — "1" alone is still waiting for second digit.
        let mut s = DateSegments::default();
        assert!(!s.type_digit(DatePart::Month, 1));
        assert_eq!(s.month, Some(1));

        // Then "2" → combined 12, valid, auto-advance.
        assert!(s.type_digit(DatePart::Month, 2));
        assert_eq!(s.month, Some(12));
    }

    #[test]
    fn type_digit_month_rolls_over_on_overflow() {
        let mut s = DateSegments::default();
        s.type_digit(DatePart::Month, 1);
        // 1 + 3 → 13 (invalid). Should roll to "3".
        s.type_digit(DatePart::Month, 3);
        assert_eq!(s.month, Some(3));
    }

    #[test]
    fn type_digit_day_respects_month_length() {
        let mut s = DateSegments {
            month: Some(2),
            year: Some(2023),
            ..Default::default()
        };
        // Feb 2023 → 28 days. Typing "3" → first digit, 3 > 2 so
        // auto-advances.
        assert!(s.type_digit(DatePart::Day, 3));
        assert_eq!(s.day, Some(3));
        // Further typing "5" → combined 35 > 28, roll to 5.
        s.type_digit(DatePart::Day, 5);
        assert_eq!(s.day, Some(5));
    }

    #[test]
    fn backspace_shortens_then_clears() {
        let mut s = DateSegments {
            month: Some(12),
            ..Default::default()
        };
        s.backspace(DatePart::Month);
        assert_eq!(s.month, Some(1));
        s.backspace(DatePart::Month);
        assert_eq!(s.month, None);
        // Backspace on empty is a no-op.
        s.backspace(DatePart::Month);
        assert_eq!(s.month, None);
    }

    #[test]
    fn increment_wraps_within_valid_range() {
        let mut s = DateSegments::default();
        s.increment(DatePart::Month);
        assert_eq!(s.month, Some(1));
        s.month = Some(12);
        s.increment(DatePart::Month);
        assert_eq!(s.month, Some(1));
    }

    #[test]
    fn decrement_month_clamps_day_to_new_month_length() {
        let mut s = DateSegments {
            month: Some(3),
            day: Some(31),
            year: Some(2023),
        };
        // March → February. Feb 2023 = 28 days, so day clamps to 28.
        s.decrement(DatePart::Month);
        assert_eq!(s.month, Some(2));
        assert_eq!(s.day, Some(28));
    }

    #[test]
    fn increment_year_clamps_leap_day() {
        let mut s = DateSegments {
            month: Some(2),
            day: Some(29),
            year: Some(2024), // leap
        };
        s.increment(DatePart::Year);
        assert_eq!(s.year, Some(2025));
        assert_eq!(s.day, Some(28));
    }

    #[test]
    fn year_type_digit_accumulates_and_autoadvances_after_fourth() {
        let mut s = DateSegments::default();
        assert!(!s.type_digit(DatePart::Year, 2));
        assert!(!s.type_digit(DatePart::Year, 0));
        assert!(!s.type_digit(DatePart::Year, 2));
        // Fourth digit fills the year and signals auto-advance.
        assert!(s.type_digit(DatePart::Year, 4));
        assert_eq!(s.year, Some(2024));
    }

    #[test]
    fn full_sequence_composes_a_date() {
        let mut s = DateSegments::default();
        // 06/15/2024
        s.type_digit(DatePart::Month, 0);
        s.type_digit(DatePart::Month, 6);
        s.type_digit(DatePart::Day, 1);
        s.type_digit(DatePart::Day, 5);
        s.type_digit(DatePart::Year, 2);
        s.type_digit(DatePart::Year, 0);
        s.type_digit(DatePart::Year, 2);
        s.type_digit(DatePart::Year, 4);
        assert_eq!(s.to_date(), DateValue::new(2024, 6, 15));
    }
}
