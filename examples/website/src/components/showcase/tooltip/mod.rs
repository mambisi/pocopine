use pine::{
    PineButton, PineTooltipContent, PineTooltipPortal, PineTooltipProvider, PineTooltipRoot,
    PineTooltipTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "TooltipDemo.poco",
    style = "tooltip.css",
    role = "panel",
    // RFC 049 — Provider groups Roots; each Root pairs a Trigger
    // with a Portal whose only child is Content.
    uses = [
        PineTooltipProvider,
        PineTooltipRoot,
        PineTooltipTrigger,
        PineTooltipPortal,
        PineTooltipContent,
        PineButton,
    ]
)]
pub struct TooltipDemo {}

#[handlers]
impl TooltipDemo {}
