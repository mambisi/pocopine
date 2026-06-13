//! Editable segment state for `<pine-time-field>`. Structurally
//! identical to [`crate::date_field::segments`] — just with
//! `HH`, `MM`, and optional `SS` segments instead of month / day /
//! year.
//!
//! Scope relative to reka's `useTimeField`:
//! - 24-hour cycle only. AM/PM toggle + `hourCycle: 12` are
//!   deferred to v2.
//! - Granularity is either `minute` (HH:MM) or `second`
//!   (HH:MM:SS). Hour-only and timezone segments are deferred.
//! - No step snapping — arrow-up / arrow-down walk one unit at a
//!   time.

use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimePart {
    Hour,
    Minute,
    Second,
}

impl TimePart {
    pub fn as_str(self) -> &'static str {
        match self {
            TimePart::Hour => "hour",
            TimePart::Minute => "minute",
            TimePart::Second => "second",
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            TimePart::Hour => "HH",
            TimePart::Minute => "MM",
            TimePart::Second => "SS",
        }
    }

    pub fn max_len(self) -> usize {
        2
    }

    pub fn cap(self) -> u8 {
        match self {
            TimePart::Hour => 23,
            TimePart::Minute | TimePart::Second => 59,
        }
    }

    /// Next editable segment in HH → MM → (SS) order. Pass
    /// `has_seconds = true` when the field is showing the seconds
    /// segment.
    pub fn next(self, has_seconds: bool) -> Option<TimePart> {
        match (self, has_seconds) {
            (TimePart::Hour, _) => Some(TimePart::Minute),
            (TimePart::Minute, true) => Some(TimePart::Second),
            (TimePart::Minute, false) => None,
            (TimePart::Second, _) => None,
        }
    }

    pub fn prev(self, _has_seconds: bool) -> Option<TimePart> {
        match self {
            TimePart::Hour => None,
            TimePart::Minute => Some(TimePart::Hour),
            TimePart::Second => Some(TimePart::Minute),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimeSegments {
    pub hour: Option<u8>,
    pub minute: Option<u8>,
    pub second: Option<u8>,
}

impl TimeSegments {
    /// Parse a native-input `HH:MM` or `HH:MM:SS` value. Empty
    /// or malformed input yields empty segments.
    pub fn parse(value: &str) -> Self {
        let mut s = Self::default();
        let mut parts = value.split(':');
        if let Some(h) = parts.next().and_then(|p| p.trim().parse::<u8>().ok())
            && h <= 23 {
                s.hour = Some(h);
            }
        if let Some(m) = parts.next().and_then(|p| p.trim().parse::<u8>().ok())
            && m <= 59 {
                s.minute = Some(m);
            }
        if let Some(sec) = parts.next().and_then(|p| p.trim().parse::<u8>().ok())
            && sec <= 59 {
                s.second = Some(sec);
            }
        s
    }

    /// Emit `HH:MM` (or `HH:MM:SS` when `has_seconds`) when every
    /// required segment is set. Returns `None` otherwise — a
    /// partial time never commits a model value.
    pub fn to_value(&self, has_seconds: bool) -> Option<String> {
        let h = self.hour?;
        let m = self.minute?;
        let mut out = String::with_capacity(if has_seconds { 8 } else { 5 });
        let _ = write!(out, "{h:02}:{m:02}");
        if has_seconds {
            let s = self.second?;
            let _ = write!(out, ":{s:02}");
        }
        Some(out)
    }

    pub fn is_complete(&self, has_seconds: bool) -> bool {
        self.to_value(has_seconds).is_some()
    }

    pub fn clear_part(&mut self, part: TimePart) {
        match part {
            TimePart::Hour => self.hour = None,
            TimePart::Minute => self.minute = None,
            TimePart::Second => self.second = None,
        }
    }

    pub fn display(&self, part: TimePart) -> String {
        let slot = match part {
            TimePart::Hour => self.hour,
            TimePart::Minute => self.minute,
            TimePart::Second => self.second,
        };
        match slot {
            Some(v) => format!("{v:02}"),
            None => part.placeholder().into(),
        }
    }

    /// Append a digit to `part`. Shares the date-field rollover
    /// semantics: when the combined two-digit value overflows the
    /// part's cap (23 for hour, 59 for minute/second), the new
    /// digit becomes a fresh first digit.
    ///
    /// Returns `true` if the segment just filled its last slot.
    pub fn type_digit(&mut self, part: TimePart, d: u8) -> bool {
        if d > 9 {
            return false;
        }
        let cap = part.cap();
        let slot = match part {
            TimePart::Hour => &mut self.hour,
            TimePart::Minute => &mut self.minute,
            TimePart::Second => &mut self.second,
        };
        type_digit_u8(slot, d, cap)
    }

    pub fn backspace(&mut self, part: TimePart) {
        let slot = match part {
            TimePart::Hour => &mut self.hour,
            TimePart::Minute => &mut self.minute,
            TimePart::Second => &mut self.second,
        };
        backspace_u8(slot);
    }

    pub fn increment(&mut self, part: TimePart) {
        match part {
            TimePart::Hour => {
                let cur = self.hour.unwrap_or(23);
                self.hour = Some(if cur >= 23 { 0 } else { cur + 1 });
            }
            TimePart::Minute => {
                let cur = self.minute.unwrap_or(59);
                self.minute = Some(if cur >= 59 { 0 } else { cur + 1 });
            }
            TimePart::Second => {
                let cur = self.second.unwrap_or(59);
                self.second = Some(if cur >= 59 { 0 } else { cur + 1 });
            }
        }
    }

    pub fn decrement(&mut self, part: TimePart) {
        match part {
            TimePart::Hour => {
                let cur = self.hour.unwrap_or(0);
                self.hour = Some(if cur == 0 { 23 } else { cur - 1 });
            }
            TimePart::Minute => {
                let cur = self.minute.unwrap_or(0);
                self.minute = Some(if cur == 0 { 59 } else { cur - 1 });
            }
            TimePart::Second => {
                let cur = self.second.unwrap_or(0);
                self.second = Some(if cur == 0 { 59 } else { cur - 1 });
            }
        }
    }
}

// ── internal helpers ───────────────────────────────────────────

fn type_digit_u8(slot: &mut Option<u8>, d: u8, cap: u8) -> bool {
    match *slot {
        None => {
            *slot = Some(d);
            d > (cap / 10)
        }
        Some(cur) => {
            let combined = cur as u16 * 10 + d as u16;
            if combined > cap as u16 {
                *slot = Some(d);
                d > (cap / 10)
            } else {
                *slot = Some(combined as u8);
                true
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hh_mm() {
        let s = TimeSegments::parse("09:30");
        assert_eq!(s.hour, Some(9));
        assert_eq!(s.minute, Some(30));
        assert_eq!(s.second, None);
    }

    #[test]
    fn parse_hh_mm_ss() {
        let s = TimeSegments::parse("09:30:45");
        assert_eq!(s.hour, Some(9));
        assert_eq!(s.minute, Some(30));
        assert_eq!(s.second, Some(45));
    }

    #[test]
    fn parse_garbage_is_empty() {
        let s = TimeSegments::parse("not a time");
        assert_eq!(s, TimeSegments::default());
    }

    #[test]
    fn display_and_roundtrip() {
        let mut s = TimeSegments::default();
        assert_eq!(s.display(TimePart::Hour), "HH");
        s.hour = Some(5);
        s.minute = Some(7);
        assert_eq!(s.display(TimePart::Hour), "05");
        assert_eq!(s.display(TimePart::Minute), "07");
        assert_eq!(s.to_value(false).as_deref(), Some("05:07"));
        assert!(s.to_value(true).is_none());
        s.second = Some(9);
        assert_eq!(s.to_value(true).as_deref(), Some("05:07:09"));
    }

    #[test]
    fn type_digit_hour_auto_advance_when_leader_locks() {
        let mut s = TimeSegments::default();
        // 2 is a possible prefix for 20..=23, so don't auto-advance.
        assert!(!s.type_digit(TimePart::Hour, 2));
        assert_eq!(s.hour, Some(2));

        let mut s = TimeSegments::default();
        // 3 can only ever be a single-digit hour — auto-advance.
        assert!(s.type_digit(TimePart::Hour, 3));
        assert_eq!(s.hour, Some(3));
    }

    #[test]
    fn type_digit_hour_rolls_over_on_overflow() {
        let mut s = TimeSegments::default();
        s.type_digit(TimePart::Hour, 2);
        // 2 + 5 → 25 > 23, roll to 5.
        s.type_digit(TimePart::Hour, 5);
        assert_eq!(s.hour, Some(5));
    }

    #[test]
    fn type_digit_minute_waits_for_second_digit() {
        let mut s = TimeSegments::default();
        // 5 could still be the tens digit (50–59). Don't advance.
        assert!(!s.type_digit(TimePart::Minute, 5));
        assert_eq!(s.minute, Some(5));
        // "59" fills the segment.
        assert!(s.type_digit(TimePart::Minute, 9));
        assert_eq!(s.minute, Some(59));
    }

    #[test]
    fn backspace_shortens_then_clears() {
        let mut s = TimeSegments {
            hour: Some(12),
            ..Default::default()
        };
        s.backspace(TimePart::Hour);
        assert_eq!(s.hour, Some(1));
        s.backspace(TimePart::Hour);
        assert_eq!(s.hour, None);
        s.backspace(TimePart::Hour);
        assert_eq!(s.hour, None);
    }

    #[test]
    fn increment_wraps_past_max() {
        let mut s = TimeSegments::default();
        s.increment(TimePart::Hour);
        assert_eq!(s.hour, Some(0)); // default starts at 23 → wraps to 0
        s.hour = Some(23);
        s.increment(TimePart::Hour);
        assert_eq!(s.hour, Some(0));
        s.minute = Some(59);
        s.increment(TimePart::Minute);
        assert_eq!(s.minute, Some(0));
    }

    #[test]
    fn decrement_wraps_past_zero() {
        let mut s = TimeSegments {
            hour: Some(0),
            minute: Some(0),
            second: Some(0),
        };
        s.decrement(TimePart::Hour);
        assert_eq!(s.hour, Some(23));
        s.decrement(TimePart::Minute);
        assert_eq!(s.minute, Some(59));
        s.decrement(TimePart::Second);
        assert_eq!(s.second, Some(59));
    }

    #[test]
    fn full_sequence_composes_a_time() {
        let mut s = TimeSegments::default();
        s.type_digit(TimePart::Hour, 0);
        s.type_digit(TimePart::Hour, 9);
        s.type_digit(TimePart::Minute, 3);
        s.type_digit(TimePart::Minute, 0);
        assert_eq!(s.to_value(false).as_deref(), Some("09:30"));
        s.type_digit(TimePart::Second, 4);
        s.type_digit(TimePart::Second, 5);
        assert_eq!(s.to_value(true).as_deref(), Some("09:30:45"));
    }
}
