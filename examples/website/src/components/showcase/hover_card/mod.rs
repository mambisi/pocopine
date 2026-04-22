use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "HoverCardDemo.poco", style = "hover_card.css", role = "panel")]
pub struct HoverCardDemo {}

#[handlers]
impl HoverCardDemo {}
