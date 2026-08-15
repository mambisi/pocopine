//! Template-path validation — a `pp-on` listener naming a handler
//! that no `#[handlers]` method provides fails to compile (missing
//! `__poc_handler_rest` marker).
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "tp-unknown-handler",
    template = poco! {<button pp-on:click="rest">reset</button>}
)]
struct TpUnknownHandler {
    count: i32,
}

#[handlers]
impl TpUnknownHandler {
    pub fn reset(&mut self) {
        self.count = 0;
    }
}

fn main() {}
