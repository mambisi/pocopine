use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "SwitchCheckboxDemo.poco", role = "panel")]
pub struct SwitchCheckboxDemo {}

#[handlers]
impl SwitchCheckboxDemo {}
