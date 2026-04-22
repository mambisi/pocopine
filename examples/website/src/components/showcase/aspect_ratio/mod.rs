use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "AspectRatioDemo.poco", role = "panel")]
pub struct AspectRatioDemo {}

#[handlers]
impl AspectRatioDemo {}
