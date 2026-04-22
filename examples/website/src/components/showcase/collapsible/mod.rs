use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "CollapsibleDemo.poco", style = "collapsible.css", role = "panel")]
pub struct CollapsibleDemo {
    pub open: bool,
}

#[handlers]
impl CollapsibleDemo {}
