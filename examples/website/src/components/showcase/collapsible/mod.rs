use pine::{PineCollapsibleContent, PineCollapsibleRoot, PineCollapsibleTrigger};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "CollapsibleDemo.poco",
    style = "collapsible.css",
    role = "panel",
    // RFC 049 — Collapsible is exactly Trigger + Content.
    uses = [PineCollapsibleRoot, PineCollapsibleTrigger, PineCollapsibleContent]
)]
pub struct CollapsibleDemo {
    pub open: bool,
}

#[handlers]
impl CollapsibleDemo {}
