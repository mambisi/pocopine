use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "InputDemo.poco", style = "input.css", role = "panel")]
pub struct InputDemo {
    pub name: String,
}

#[handlers]
impl InputDemo {}
