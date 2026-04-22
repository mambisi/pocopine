use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PopoverDemo.poco", role = "panel")]
pub struct PopoverDemo {
    pub open: bool,
}

#[handlers]
impl PopoverDemo {}
