use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "CollapsibleDemo.poco", role = "panel")]
pub struct CollapsibleDemo {
    pub open: bool,
}

#[handlers]
impl CollapsibleDemo {}
