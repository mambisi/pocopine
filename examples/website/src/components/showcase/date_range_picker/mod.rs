use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use web_sys::CustomEvent;

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "DateRangePickerDemo.poco",
    style = "date_range_picker.css",
    role = "panel"
)]
pub struct DateRangePickerDemo {
    pub start: String,
    pub end: String,
    pub placeholder: String,
}

#[handlers]
impl DateRangePickerDemo {
    pub fn on_mount(&mut self) {
        if self.placeholder.is_empty() {
            self.placeholder = "2024-06-15".into();
        }
    }

    // The drop-in re-emits `pp:update:start` / `pp:update:end` from
    // its own root so authors avoid the multi-pp-model clobber.
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
