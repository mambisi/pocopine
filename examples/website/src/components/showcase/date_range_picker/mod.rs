use pine::PineDateRangePicker;
use pine::datetime::DateValue;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "DateRangePickerDemo.poco",
    style = "date_range_picker.css",
    role = "panel",
    // RFC 049 — Uses the pre-composed `<pine-date-range-picker>`.
    uses = [PineDateRangePicker]
)]
pub struct DateRangePickerDemo {
    pub start: Option<DateValue>,
    pub end: Option<DateValue>,
    pub placeholder: Option<DateValue>,
}

#[handlers]
impl DateRangePickerDemo {
    pub fn on_mount(&mut self) {
        if self.placeholder.is_none() {
            self.placeholder = DateValue::parse_iso("2024-06-15");
        }
    }

    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
    }
}
