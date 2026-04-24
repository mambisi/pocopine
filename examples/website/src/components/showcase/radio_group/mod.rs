use pine::{PineRadioGroupIndicator, PineRadioGroupItem, PineRadioGroupRoot};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "RadioGroupDemo.poco",
    style = "radio_group.css",
    role = "panel",
    // RFC 049 — Root only accepts Items. Wrapping items in
    // `<div class="list">` would break roving-tabindex walking;
    // the compile-time check catches that shape mistake.
    uses = [
        PineRadioGroupRoot,
        PineRadioGroupItem,
        PineRadioGroupIndicator,
    ]
)]
pub struct RadioGroupDemo {
    pub plan: String,
}

#[handlers]
impl RadioGroupDemo {
    pub fn on_mount(&mut self) {
        self.plan = "free".into();
    }
}
