use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use web_sys::CustomEvent;

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

    // Pine's `pp-model:X` all listen for the generic
    // `pp:update:model` event, so binding two fields on one child
    // clobbers both. The range calendar emits per-field events
    // (`pp:update:start` / `pp:update:end`) instead — handle them
    // explicitly here.
    pub fn on_start_update(&mut self, ev: CustomEvent) {
        self.start = ev.detail().as_string().unwrap_or_default();
    }

    pub fn on_end_update(&mut self, ev: CustomEvent) {
        self.end = ev.detail().as_string().unwrap_or_default();
    }

    pub fn clear(&mut self) {
        self.start = String::new();
        self.end = String::new();
    }
}
