use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "CmdPopoverDemo.poco", style = "cmd_popover.css", role = "panel")]
pub struct CmdPopoverDemo {
    pub open: bool,
    pub last: String,
}

#[handlers]
impl CmdPopoverDemo {
    pub fn pcmd_new_file(&mut self) { self.last = "New File".into(); self.open = false; }
    pub fn pcmd_open_file(&mut self) { self.last = "Open File".into(); self.open = false; }
    pub fn pcmd_save(&mut self) { self.last = "Save".into(); self.open = false; }
    pub fn pcmd_format(&mut self) { self.last = "Format Document".into(); self.open = false; }
    pub fn pcmd_find_file(&mut self) { self.last = "Find in files".into(); self.open = false; }
}
