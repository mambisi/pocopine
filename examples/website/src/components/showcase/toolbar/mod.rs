use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ToolbarDemo.poco", style = "toolbar.css", role = "panel")]
pub struct ToolbarDemo {}

#[handlers]
impl ToolbarDemo {}
