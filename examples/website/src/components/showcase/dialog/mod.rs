use pine::{
    PineButton, PineDialogClose, PineDialogContent, PineDialogDescription, PineDialogOverlay,
    PineDialogPortal, PineDialogRoot, PineDialogTitle, PineDialogTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "DialogDemo.poco",
    style = "dialog.css",
    role = "panel",
    // RFC 060 Tier 2 — every custom-element tag in the template
    // must be declared. The Dialog compound enforces (RFC 049):
    //   Root → [Trigger, Portal]
    //   Portal → [Overlay, Content]
    //   Content → accepts [Title, Description, Close, …]
    // PineButton is loose author content but still has to be
    // listed so the registry walk covers it.
    uses = [
        PineDialogRoot,
        PineDialogTrigger,
        PineDialogPortal,
        PineDialogOverlay,
        PineDialogContent,
        PineDialogTitle,
        PineDialogDescription,
        PineDialogClose,
        PineButton,
    ]
)]
pub struct DialogDemo {
    pub open: bool,
}

#[handlers]
impl DialogDemo {}
