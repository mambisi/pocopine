use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "TextDemo.poco", style = "text.css", role = "panel")]
pub struct TextDemo {
    pub width: f64,
}

impl Default for TextDemo {
    fn default() -> Self {
        Self { width: 240.0 }
    }
}

#[handlers]
impl TextDemo {}
