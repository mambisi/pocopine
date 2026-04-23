use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

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

    pub fn clear(&mut self) {
        self.start = String::new();
        self.end = String::new();
    }
}
