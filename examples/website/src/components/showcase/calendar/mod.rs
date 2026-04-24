use pine::datetime::DateValue;
use pine::{
    PineCalendarGrid, PineCalendarGridBody, PineCalendarGridHead, PineCalendarHeader,
    PineCalendarHeading, PineCalendarNext, PineCalendarPrev, PineCalendarRoot,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "CalendarDemo.poco",
    style = "calendar.css",
    role = "panel",
    // RFC 049 — Calendar Root only accepts Header + Grid; Header
    // takes Prev/Heading/Next; Grid nests Head + Body.
    uses = [
        PineCalendarRoot,
        PineCalendarHeader,
        PineCalendarPrev,
        PineCalendarHeading,
        PineCalendarNext,
        PineCalendarGrid,
        PineCalendarGridHead,
        PineCalendarGridBody,
    ]
)]
pub struct CalendarDemo {
    /// Selected date. Two-way bound via `pp-model:value` on the
    /// calendar root; authored template strings still deserialize
    /// into it.
    pub value: Option<DateValue>,
    /// Visible-month anchor. Bound via `pp-model:placeholder` so
    /// Prev/Next writes flow back into demo state.
    pub placeholder: Option<DateValue>,
    pub min_value: Option<DateValue>,
    pub max_value: Option<DateValue>,
    pub week_starts_on: u32,
    pub fixed_weeks: bool,
    pub prevent_deselect: bool,
    pub disabled: bool,
    pub readonly: bool,
}

#[handlers]
impl CalendarDemo {
    pub fn on_mount(&mut self) {
        // Seed the reactive state here — not via `Default` —
        // so the writes propagate through pp-model / pp-bind to
        // <pine-calendar-root>. Values assigned inside Default
        // sit pre-reactivity and don't retrigger the child's
        // #[watch]ers the way a subsequent mutation does.
        //
        // Use a month that naturally renders as 5 weeks for both
        // Sunday- and Monday-start layouts so the "Fixed weeks"
        // toggle has an obvious visual effect in the demo.
        if self.placeholder.is_none() {
            self.placeholder = DateValue::parse_iso("2024-02-15");
        }
    }

    pub fn week_start_sun(&mut self) {
        self.week_starts_on = 0;
    }
    pub fn week_start_mon(&mut self) {
        self.week_starts_on = 1;
    }

    pub fn goto_today(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            let js = js_sys::Date::new_0();
            let y = js.get_full_year() as i32;
            let m = (js.get_month() + 1) as u32;
            let d = js.get_date() as u32;
            self.placeholder = DateValue::new(y, m as u8, d as u8);
        }
    }

    pub fn clear_all(&mut self) {
        self.value = None;
        self.min_value = None;
        self.max_value = None;
        self.placeholder = DateValue::parse_iso("2024-02-15");
        self.week_starts_on = 0;
        self.fixed_weeks = false;
        self.prevent_deselect = false;
        self.disabled = false;
        self.readonly = false;
    }
}
