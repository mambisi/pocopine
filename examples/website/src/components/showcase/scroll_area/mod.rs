use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ScrollAreaDemo.poco", role = "panel")]
pub struct ScrollAreaDemo {
    pub scroll_type: String,
}

#[handlers]
impl ScrollAreaDemo {
    pub fn on_mount(&mut self) { self.scroll_type = "hover".into(); }
}
