use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TooltipDemo.poco", style = "tooltip.css", role = "panel")]
pub struct TooltipDemo {}

#[handlers]
impl TooltipDemo {}
