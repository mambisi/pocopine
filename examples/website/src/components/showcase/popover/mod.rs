use pine::{
    PinePopoverClose, PinePopoverContent, PinePopoverPortal, PinePopoverRoot, PinePopoverTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PopoverDemo.poco",
    style = "popover.css",
    role = "panel",
    // RFC 049 — Popover has strict Root + Portal; Content is
    // author-driven (`accepts`) so forms, cards, text all fit.
    uses = [
        PinePopoverRoot,
        PinePopoverTrigger,
        PinePopoverPortal,
        PinePopoverContent,
        PinePopoverClose,
    ]
)]
pub struct PopoverDemo {
    pub open: bool,
}

#[handlers]
impl PopoverDemo {}
