use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ToggleDemo.poco", role = "panel")]
pub struct ToggleDemo {
    pub bold: bool,
    pub align: String,
    pub format: Vec<String>,
}

#[handlers]
impl ToggleDemo {
    pub fn on_mount(&mut self) { self.align = "left".into(); }
}
