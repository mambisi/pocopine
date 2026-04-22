use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Basics.poco", style = "basics.css", role = "panel")]
pub struct Basics {}

#[handlers]
impl Basics {}
