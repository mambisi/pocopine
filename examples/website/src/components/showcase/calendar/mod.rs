use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "CalendarDemo.poco", style = "calendar.css", role = "panel")]
pub struct CalendarDemo {
    /// Selected date as ISO `YYYY-MM-DD`. Two-way bound via
    /// `pp-model:value` on the calendar root.
    pub value: String,
    /// Visible-month anchor. Bound via `pp-model:placeholder` so
    /// Prev/Next writes flow back into demo state.
    pub placeholder: String,
}

#[handlers]
impl CalendarDemo {
    pub fn on_mount(&mut self) {
        if self.placeholder.is_empty() {
            self.placeholder = "2024-06-15".into();
        }
    }
}
