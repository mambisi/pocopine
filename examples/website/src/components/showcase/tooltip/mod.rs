use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TooltipDemo.poco", role = "panel")]
pub struct TooltipDemo {}

#[handlers]
impl TooltipDemo {}
