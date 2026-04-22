use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "AlertDialogDemo.poco", role = "panel")]
pub struct AlertDialogDemo {
    pub open: bool,
    pub last_action: String,
}

#[handlers]
impl AlertDialogDemo {
    pub fn confirm_destroy(&mut self) { self.last_action = "destroyed".into(); }
}
