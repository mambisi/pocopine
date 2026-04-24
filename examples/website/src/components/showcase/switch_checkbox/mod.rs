use pine::{PineCheckbox, PineSwitch};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "SwitchCheckboxDemo.poco",
    style = "switch_checkbox.css",
    role = "panel",
    // RFC 049 — leaf primitives; no typed-slot parent here.
    uses = [PineSwitch, PineCheckbox]
)]
pub struct SwitchCheckboxDemo {
    pub dark_mode: bool,
    pub agree_state: String,
}

#[handlers]
impl SwitchCheckboxDemo {
    pub fn on_setup(&mut self) {
        self.agree_state = "unchecked".into();
    }
}
