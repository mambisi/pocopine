//! Shared datetime types — port of reka-ui's
//! `packages/core/src/shared/date/types.ts` and
//! `packages/core/src/date/types.ts`.
//!
//! v1 is pure Gregorian, date-only. `DateValue` stands in for
//! `@internationalized/date`'s `CalendarDate`. The enum shape is
//! reserved for v2 when `CalendarDateTime` / `ZonedDateTime` land —
//! consumers that pattern-match today should still compile when the
//! variant set grows.

use crate::datetime::DateValue;

/// Predicate over a date. Pine passes these through from component
/// props (e.g. "is this date disabled?") to the calendar helpers.
/// `Box<dyn Fn>` so authors can close over their own state.
pub type Matcher = std::sync::Arc<dyn Fn(&DateValue) -> bool + Send + Sync>;

/// Calendar grid — a value anchor plus a flat `cells` list and a
/// row-major `rows` layout. Matches reka's `Grid<T>` shape.
#[derive(Debug, Clone)]
pub struct Grid<T> {
    /// Anchor date for the grid (typically the first day of the
    /// month or year it represents).
    pub value: DateValue,
    /// Week-major rows. Each inner `Vec<T>` is one week.
    pub rows: Vec<Vec<T>>,
    /// All cells flattened in display order.
    pub cells: Vec<T>,
}

/// Half-open date range with nullable edges — matches reka's
/// shape where either bound can be unselected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DateRange {
    pub start: Option<DateValue>,
    pub end: Option<DateValue>,
}

/// Day-of-week — Sunday = 0, Saturday = 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum DayOfWeek {
    Sunday = 0,
    Monday = 1,
    Tuesday = 2,
    Wednesday = 3,
    Thursday = 4,
    Friday = 5,
    Saturday = 6,
}

impl DayOfWeek {
    pub const fn from_u8(n: u8) -> Option<Self> {
        match n {
            0 => Some(Self::Sunday),
            1 => Some(Self::Monday),
            2 => Some(Self::Tuesday),
            3 => Some(Self::Wednesday),
            4 => Some(Self::Thursday),
            5 => Some(Self::Friday),
            6 => Some(Self::Saturday),
            _ => None,
        }
    }
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// `weekStartsOn` — 0 = Sunday … 6 = Saturday.
pub type WeekStartsOn = u8;

/// Format spec for the weekday labels in a calendar header.
/// Mirrors reka's `WeekDayFormat` union; renderers are free to
/// ignore it for v1 and ship English short names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekDayFormat {
    Narrow,
    Short,
    Long,
}

impl Default for WeekDayFormat {
    fn default() -> Self {
        WeekDayFormat::Short
    }
}
