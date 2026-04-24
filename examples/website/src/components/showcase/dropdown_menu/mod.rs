use pine::{
    PineDropdownMenuArrow, PineDropdownMenuCheckboxItem, PineDropdownMenuContent,
    PineDropdownMenuGroup, PineDropdownMenuItem, PineDropdownMenuItemIndicator,
    PineDropdownMenuLabel, PineDropdownMenuPortal, PineDropdownMenuRadioGroup,
    PineDropdownMenuRadioItem, PineDropdownMenuRoot, PineDropdownMenuSeparator,
    PineDropdownMenuSub, PineDropdownMenuSubContent, PineDropdownMenuSubTrigger,
    PineDropdownMenuTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "DropdownMenuDemo.poco",
    style = "dropdown_menu.css",
    role = "panel",
    // RFC 049 — the rich menu compound. Six typed parents
    // (Root, Portal, Content, SubContent, Group, RadioGroup,
    // Sub) with cascading strictness: Content accepts eight
    // kinds; Group and RadioGroup narrow to their own
    // item subsets; Sub is exactly [SubTrigger, SubContent].
    uses = [
        PineDropdownMenuRoot,
        PineDropdownMenuTrigger,
        PineDropdownMenuPortal,
        PineDropdownMenuContent,
        PineDropdownMenuItem,
        PineDropdownMenuSeparator,
        PineDropdownMenuLabel,
        PineDropdownMenuGroup,
        PineDropdownMenuCheckboxItem,
        PineDropdownMenuItemIndicator,
        PineDropdownMenuRadioGroup,
        PineDropdownMenuRadioItem,
        PineDropdownMenuSub,
        PineDropdownMenuSubTrigger,
        PineDropdownMenuSubContent,
        PineDropdownMenuArrow,
    ]
)]
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
    pub fn action_bump(&mut self) {
        self.clicks += 1;
        self.actions_fired += 1;
    }
    pub fn action_toggle_dark(&mut self) {
        self.dark_mode = !self.dark_mode;
        self.actions_fired += 1;
    }
    pub fn action_reset(&mut self) {
        self.clicks = 0;
        self.actions_fired += 1;
        self.dark_mode = false;
        self.agree_state = "unchecked".into();
    }
}
