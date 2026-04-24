use pine::PineInput;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "InputDemo.poco",
    style = "input.css",
    role = "panel",
    // RFC 049 — single-element primitive.
    uses = [PineInput]
)]
pub struct InputDemo {
    pub name: String,
}

#[handlers]
impl InputDemo {}
