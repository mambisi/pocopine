use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "HoverCardDemo.poco", role = "panel")]
pub struct HoverCardDemo {}

#[handlers]
impl HoverCardDemo {}
