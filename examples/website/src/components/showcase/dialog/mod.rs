use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "DialogDemo.poco", role = "panel")]
pub struct DialogDemo {
    pub open: bool,
}

#[handlers]
impl DialogDemo {}
