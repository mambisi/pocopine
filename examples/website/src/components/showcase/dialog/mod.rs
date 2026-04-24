use pine::{
    PineDialogClose, PineDialogContent, PineDialogDescription, PineDialogOverlay,
    PineDialogPortal, PineDialogRoot, PineDialogTitle, PineDialogTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "DialogDemo.poco",
    style = "dialog.css",
    role = "panel",
    // RFC 049 — opt into compile-time slot-contract validation.
    // Listing Pine types here lets `#[component]` scan the
    // template for parent/child shape violations. The Dialog
    // compound enforces:
    //   Root → [Trigger, Portal]
    //   Portal → [Overlay, Content]
    //   Content → accepts [Title, Description, Close, …]
    //
    // Only the Dialog parts participate in the slot contract.
    // `<pine-button>` elsewhere in the template is treated as
    // arbitrary author content (not listed here, silently
    // skipped by the scan per RFC 049 §4.3).
    uses = [
        PineDialogRoot,
        PineDialogTrigger,
        PineDialogPortal,
        PineDialogOverlay,
        PineDialogContent,
        PineDialogTitle,
        PineDialogDescription,
        PineDialogClose,
    ]
)]
pub struct DialogDemo {
    pub open: bool,
}

#[handlers]
impl DialogDemo {}
