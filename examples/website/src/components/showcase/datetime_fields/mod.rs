use pine::datetime::DateValue;
use pine::{PineDateField, PineDateRangeField, PineTimeField, PineTimeRangeField};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "DatetimeFieldsDemo.poco",
    style = "datetime_fields.css",
    role = "panel",
    // RFC 049 — Each of these is a single-element primitive with
    // no typed-slot surface, so the list is flat.
    uses = [
        PineDateField,
        PineTimeField,
        PineDateRangeField,
        PineTimeRangeField,
    ]
)]
pub struct DatetimeFieldsDemo {
    // date field
    pub date_value: Option<DateValue>,
    // time field
    pub time_value: String,
    // date range
    pub range_start: Option<DateValue>,
    pub range_end: Option<DateValue>,
    // time range
    pub time_start: String,
    pub time_end: String,
}

#[handlers]
impl DatetimeFieldsDemo {}
