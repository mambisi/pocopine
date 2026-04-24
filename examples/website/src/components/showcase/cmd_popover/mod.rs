use pine::{
    PineCommandEmpty, PineCommandInput, PineCommandItem, PineCommandList, PineCommandRoot,
    PinePopoverContent, PinePopoverPortal, PinePopoverRoot, PinePopoverTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "CmdPopoverDemo.poco",
    style = "cmd_popover.css",
    role = "panel",
    // RFC 049 — Composes a Command palette inside a Popover
    // (no built-in modal shell); both compounds' parts listed.
    uses = [
        PinePopoverRoot,
        PinePopoverTrigger,
        PinePopoverPortal,
        PinePopoverContent,
        PineCommandRoot,
        PineCommandInput,
        PineCommandList,
        PineCommandItem,
        PineCommandEmpty,
    ]
)]
pub struct CmdPopoverDemo {
    pub open: bool,
    pub last: String,
}

#[handlers]
impl CmdPopoverDemo {
    pub fn pcmd_new_file(&mut self) {
        self.last = "New File".into();
        self.open = false;
    }
    pub fn pcmd_open_file(&mut self) {
        self.last = "Open File".into();
        self.open = false;
    }
    pub fn pcmd_save(&mut self) {
        self.last = "Save".into();
        self.open = false;
    }
    pub fn pcmd_format(&mut self) {
        self.last = "Format Document".into();
        self.open = false;
    }
    pub fn pcmd_find_file(&mut self) {
        self.last = "Find in files".into();
        self.open = false;
    }
}
