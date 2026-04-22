use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "RangeCalendarDemo.poco",
    style = "range_calendar.css",
    role = "panel"
)]
pub struct RangeCalendarDemo {
    pub start: String,
    pub end: String,
    pub placeholder: String,
}

#[handlers]
impl RangeCalendarDemo {
    pub fn on_mount(&mut self) {
        if self.placeholder.is_empty() {
            self.placeholder = "2024-06-15".into();
        }
    }

    pub fn clear(&mut self) {
        self.start = String::new();
        self.end = String::new();
    }
}
