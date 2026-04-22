use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "DialogDemo.poco", style = "dialog.css", role = "panel")]
pub struct DialogDemo {
    pub open: bool,
}

#[handlers]
impl DialogDemo {}
