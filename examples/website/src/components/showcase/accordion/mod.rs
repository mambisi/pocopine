use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "AccordionDemo.poco", role = "panel")]
pub struct AccordionDemo {
    pub value: String,
}

#[handlers]
impl AccordionDemo {}
