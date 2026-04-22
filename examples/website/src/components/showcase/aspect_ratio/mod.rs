use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "AspectRatioDemo.poco", style = "aspect_ratio.css", role = "panel")]
pub struct AspectRatioDemo {}

#[handlers]
impl AspectRatioDemo {}
