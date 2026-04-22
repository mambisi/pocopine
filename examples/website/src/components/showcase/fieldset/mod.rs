use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "FieldsetDemo.poco", style = "fieldset.css", role = "panel")]
pub struct FieldsetDemo {
    pub disabled: bool,
    pub first_name: String,
    pub last_name: String,
}

#[handlers]
impl FieldsetDemo {
    pub fn toggle(&mut self) {
        self.disabled = !self.disabled;
    }
}
