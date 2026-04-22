//! Calendar math for date pickers — Rust port of reka-ui's
//! `packages/core/src/date` module.
//!
//! reka's TS uses `@internationalized/date` under the hood;
//! this port uses the [`time`] crate for Gregorian math and
//! re-implements the `calendar.ts` / `comparators.ts` / `utils.ts`
//! helpers on top so future Pine Calendar / DatePicker / DateRange
//! primitives have a single place to reach for date logic.
//!
//! **v1 scope (LTR Gregorian):**
//! - [`DateValue`] — civil year/month/day with clip-to-month
//!   arithmetic.
//! - [`types`] — `DateRange`, `Matcher`, `Grid<T>`, `DayOfWeek`,
//!   `WeekStartsOn`.
//! - [`comparators`] — `is_before` / `is_after` / `is_between*`,
//!   same-year / same-year-month, days-in-month, months / years
//!   between, the `are_all_*_between_valid` family.
//! - [`calendar`] — `create_month`, `create_months`,
//!   `create_year_grid`, decade / year range helpers, plus
//!   `get_week_number` (ISO + US).
//! - [`utils`] — `chunk`.
//!
//! **Deferred to v2:** `CalendarDateTime`, `ZonedDateTime`, non-
//! Gregorian calendars, locale-aware week-start derivation (v1
//! callers pass an explicit [`WeekStartsOn`]).

pub mod calendar;
pub mod comparators;
pub mod date_value;
pub mod types;
pub mod utils;

pub use calendar::{
    create_date_range, create_decade, create_month, create_month_grid, create_months,
    create_year, create_year_grid, create_year_range, end_of_decade, end_of_month, end_of_year,
    get_days_between, get_week_number, get_week_number_iso, get_week_number_us,
    get_week_starts_on, start_of_decade, start_of_month, start_of_week, start_of_year,
    CreateMonthProps, CreateMonthsProps, WeekMode,
};
pub use comparators::{
    are_all_days_between_valid, are_all_months_between_valid, are_all_years_between_valid,
    compare_year_month, get_days_in_month, get_last_first_day_of_week, get_months_between,
    get_next_last_day_of_week, get_years_between, is_after, is_after_or_same, is_before,
    is_before_or_same, is_between, is_between_inclusive, is_equal_month,
    is_month_between_inclusive, is_same_day, is_same_month, is_same_year, is_same_year_month,
    is_year_between_inclusive,
};
pub use date_value::DateValue;
pub use types::{DateRange, DayOfWeek, Grid, Matcher, WeekDayFormat, WeekStartsOn};
pub use utils::chunk;
