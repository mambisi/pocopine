use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ButtonDemo.poco", role = "panel")]
pub struct ButtonDemo {
    pub clicks: u32,
}

#[handlers]
impl ButtonDemo {
    pub fn bump(&mut self) { self.clicks += 1; }
}
