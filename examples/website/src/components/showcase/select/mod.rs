use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "SelectDemo.poco", role = "panel")]
pub struct SelectDemo {
    pub city: String,
}

#[handlers]
impl SelectDemo {}
