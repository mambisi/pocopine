use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "SwitchCheckboxDemo.poco", style = "switch_checkbox.css", role = "panel")]
pub struct SwitchCheckboxDemo {}

#[handlers]
impl SwitchCheckboxDemo {}
