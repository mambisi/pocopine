use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "FieldDemo.poco", style = "field.css", role = "panel")]
pub struct FieldDemo {
    pub name: String,
}

#[handlers]
impl FieldDemo {}
