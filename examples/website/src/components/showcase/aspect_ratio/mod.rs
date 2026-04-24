use pine::PineAspectRatio;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "AspectRatioDemo.poco",
    style = "aspect_ratio.css",
    role = "panel",
    // RFC 049 — single-element primitive.
    uses = [PineAspectRatio]
)]
pub struct AspectRatioDemo {}

#[handlers]
impl AspectRatioDemo {}
