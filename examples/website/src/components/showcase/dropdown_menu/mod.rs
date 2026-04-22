use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "DropdownMenuDemo.poco", style = "dropdown_menu.css", role = "panel")]
pub struct DropdownMenuDemo {
    pub clicks: u32,
    pub actions_fired: u32,
    pub dark_mode: bool,
    pub agree_state: String,
    pub show_muted: String,
    pub show_archived: String,
    pub density: String,
}

#[handlers]
impl DropdownMenuDemo {
    pub fn on_mount(&mut self) {
        self.agree_state = "unchecked".into();
        self.show_muted = "unchecked".into();
        self.show_archived = "unchecked".into();
        self.density = "comfortable".into();
    }
    pub fn action_bump(&mut self) { self.clicks += 1; self.actions_fired += 1; }
    pub fn action_toggle_dark(&mut self) { self.dark_mode = !self.dark_mode; self.actions_fired += 1; }
    pub fn action_reset(&mut self) {
        self.clicks = 0;
        self.actions_fired += 1;
        self.dark_mode = false;
        self.agree_state = "unchecked".into();
    }
}
