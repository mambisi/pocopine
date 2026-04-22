use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Basics.poco", role = "panel")]
pub struct Basics {}

#[handlers]
impl Basics {}
