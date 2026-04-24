use pine::{
    PineCommandContent, PineCommandEmpty, PineCommandInput, PineCommandItem, PineCommandList,
    PineCommandOverlay, PineCommandPortal, PineCommandRoot,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "CommandDemo.poco",
    style = "command.css",
    role = "panel",
    // RFC 049 — Command is a palette compound: Root holds the
    // Portal, Portal wraps Overlay + Content, Content groups an
    // Input with a List of Items and an Empty fallback.
    uses = [
        PineCommandRoot,
        PineCommandPortal,
        PineCommandOverlay,
        PineCommandContent,
        PineCommandInput,
        PineCommandList,
        PineCommandItem,
        PineCommandEmpty,
    ]
)]
pub struct CommandDemo {
    pub last_command: String,
}

#[handlers]
impl CommandDemo {
    pub fn cmd_new_file(&mut self) {
        self.last_command = "New File".into();
    }
    pub fn cmd_open_file(&mut self) {
        self.last_command = "Open File".into();
    }
    pub fn cmd_save(&mut self) {
        self.last_command = "Save".into();
    }
    pub fn cmd_toggle_theme(&mut self) {
        self.last_command = "Toggle theme".into();
    }
    pub fn cmd_settings(&mut self) {
        self.last_command = "Open Settings".into();
    }
}
