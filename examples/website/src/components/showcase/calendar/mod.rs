use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "CalendarDemo.poco", style = "calendar.css", role = "panel")]
pub struct CalendarDemo {
    /// Selected date as ISO `YYYY-MM-DD`. Two-way bound via
    /// `pp-model:value` on the calendar root.
    pub value: String,
    /// Visible-month anchor. Bound via `pp-model:placeholder` so
    /// Prev/Next writes flow back into demo state.
    pub placeholder: String,
    pub min_value: String,
    pub max_value: String,
    pub week_starts_on: u32,
    pub fixed_weeks: bool,
    pub prevent_deselect: bool,
    pub disabled: bool,
    pub readonly: bool,
}

impl Default for CalendarDemo {
    fn default() -> Self {
        Self {
            value: String::new(),
            placeholder: "2024-06-15".into(),
            min_value: String::new(),
            max_value: String::new(),
            week_starts_on: 0,
            fixed_weeks: false,
            prevent_deselect: false,
            disabled: false,
            readonly: false,
        }
    }
}

#[handlers]
impl CalendarDemo {
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
            self.placeholder = format!("{y:04}-{m:02}-{d:02}");
        }
    }

    pub fn clear_all(&mut self) {
        self.value = String::new();
        self.min_value = String::new();
        self.max_value = String::new();
        self.placeholder = "2024-06-15".into();
        self.week_starts_on = 0;
        self.fixed_weeks = false;
        self.prevent_deselect = false;
        self.disabled = false;
        self.readonly = false;
    }
}
