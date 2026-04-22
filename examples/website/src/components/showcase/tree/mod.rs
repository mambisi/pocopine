use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TreeDemo.poco", style = "tree.css", role = "panel")]
pub struct TreeDemo {
    pub value: String,
}

#[handlers]
impl TreeDemo {}
