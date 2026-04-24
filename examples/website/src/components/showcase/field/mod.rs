use pine::{
    PineFieldControl, PineFieldDescription, PineFieldError, PineFieldLabel, PineFieldRoot,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FieldDemo.poco",
    style = "field.css",
    role = "panel",
    // RFC 049 — Field Root is loose (authors nest arbitrary form
    // markup) but declares the parts it knows how to wire.
    uses = [
        PineFieldRoot,
        PineFieldLabel,
        PineFieldControl,
        PineFieldDescription,
        PineFieldError,
    ]
)]
pub struct FieldDemo {
    pub name: String,
}

#[handlers]
impl FieldDemo {}
