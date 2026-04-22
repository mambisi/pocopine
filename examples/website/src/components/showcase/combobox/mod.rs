use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ComboboxDemo.poco", style = "combobox.css", role = "panel")]
pub struct ComboboxDemo {
    pub framework: String,
}

#[handlers]
impl ComboboxDemo {}
