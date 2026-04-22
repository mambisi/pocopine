use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "SplitterDemo.poco", role = "panel")]
pub struct SplitterDemo {
    pub sizes: Vec<f64>,
}

#[handlers]
impl SplitterDemo {
    pub fn on_mount(&mut self) { self.sizes = vec![25.0, 50.0, 25.0]; }
}
