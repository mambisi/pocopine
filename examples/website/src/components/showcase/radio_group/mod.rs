use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "RadioGroupDemo.poco", style = "radio_group.css", role = "panel")]
pub struct RadioGroupDemo {
    pub plan: String,
}

#[handlers]
impl RadioGroupDemo {
    pub fn on_mount(&mut self) { self.plan = "free".into(); }
}
