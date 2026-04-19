//! Pine MVP showcase. One section per primitive, hand-authored
//! CSS in `styles.css` alongside for visual parity with something
//! a Tailwind-ish app would ship.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDemoApp.poco")]
pub struct PineDemoApp {
    pub clicks: u32,

    pub dialog_open: bool,
    pub popover_open: bool,
    pub menu_open: bool,

    pub tab: String,
    pub tabs: Vec<pine::TabDef>,

    pub dark_mode: bool,
    pub agree_state: String,
}

#[handlers]
impl PineDemoApp {
    pub fn on_mount(&mut self) {
        self.tab = "account".into();
        self.agree_state = "unchecked".into();
        self.tabs = vec![
            pine::TabDef {
                value: "account".into(),
                label: "Account".into(),
                disabled: false,
            },
            pine::TabDef {
                value: "notifications".into(),
                label: "Notifications".into(),
                disabled: false,
            },
            pine::TabDef {
                value: "billing".into(),
                label: "Billing".into(),
                disabled: false,
            },
        ];
    }

    pub fn bump(&mut self) {
        self.clicks += 1;
    }

    pub fn open_dialog(&mut self) {
        self.dialog_open = true;
    }
    pub fn close_dialog(&mut self) {
        self.dialog_open = false;
    }

    pub fn toggle_popover(&mut self) {
        self.popover_open = !self.popover_open;
    }
    pub fn toggle_menu(&mut self) {
        self.menu_open = !self.menu_open;
    }
    pub fn close_menu(&mut self) {
        self.menu_open = false;
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
    App::new().register::<PineDemoApp>().run();
}
