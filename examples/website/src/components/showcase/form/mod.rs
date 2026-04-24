use pine::PineFormRoot;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FormDemo.poco",
    style = "form.css",
    role = "panel",
    // RFC 049 — Form Root wraps arbitrary markup; declared here
    // so consumers see the opt-in shape.
    uses = [PineFormRoot]
)]
pub struct FormDemo {
    pub username: String,
    pub submitted: String,
}

#[handlers]
impl FormDemo {
    pub fn submit(&mut self) {
        self.submitted = self.username.clone();
    }
}
