use pine::{
    PineAlertDialogAction, PineAlertDialogCancel, PineAlertDialogContent,
    PineAlertDialogDescription, PineAlertDialogOverlay, PineAlertDialogPortal, PineAlertDialogRoot,
    PineAlertDialogTitle, PineAlertDialogTrigger, PineButton,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "AlertDialogDemo.poco",
    style = "alert_dialog.css",
    role = "panel",
    // RFC 049 — same shape as Dialog plus Action/Cancel buttons.
    uses = [
        PineAlertDialogRoot,
        PineAlertDialogTrigger,
        PineAlertDialogPortal,
        PineAlertDialogOverlay,
        PineAlertDialogContent,
        PineAlertDialogTitle,
        PineAlertDialogDescription,
        PineAlertDialogAction,
        PineAlertDialogCancel,
        PineButton,
    ]
)]
pub struct AlertDialogDemo {
    pub open: bool,
    pub last_action: String,
}

#[handlers]
impl AlertDialogDemo {
    pub fn confirm_destroy(&mut self) {
        self.last_action = "destroyed".into();
    }
}
