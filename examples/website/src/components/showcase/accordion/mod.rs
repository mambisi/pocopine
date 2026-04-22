use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "AccordionDemo.poco", style = "accordion.css", role = "panel")]
pub struct AccordionDemo {
    pub value: String,
}

#[handlers]
impl AccordionDemo {}
