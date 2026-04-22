use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Compositional `<pine-popover>` + `<pine-calendar-root>` demo.
///
/// Serves two jobs:
/// 1. Shows authors how to hand-compose a working date picker
///    out of Pine primitives they already have.
/// 2. Stress-tests the shared `value` / `placeholder` state
///    (pp-model on both sides) + close-on-select wiring.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "DatePickerDemo.poco",
    style = "date_picker.css",
    role = "panel"
)]
pub struct DatePickerDemo {
    pub value: String,
    pub placeholder: String,
    pub open: bool,
}

#[handlers]
impl DatePickerDemo {
    pub fn on_mount(&mut self) {
        if self.placeholder.is_empty() {
            self.placeholder = "2024-06-15".into();
        }
    }

    /// Fires whenever the calendar writes back through
    /// `pp-model:value`. Used for the close-on-select behavior
    /// plus keeping the trigger label in sync.
    #[watch(value)]
    fn on_value_change(&mut self, new: String, prev: Option<String>) {
        // Ignore the no-op initial flow — only close when the
        // user actually picked a non-empty date.
        if new.is_empty() {
            return;
        }
        if prev.as_deref() == Some(new.as_str()) {
            return;
        }
        self.open = false;
    }

    pub fn clear(&mut self) {
        self.value = String::new();
    }
}
