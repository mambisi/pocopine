//! Pine MVP showcase. One section per primitive, hand-authored
//! CSS in `styles.css` alongside for visual parity with something
//! a Tailwind-ish app would ship.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::Element;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDemoApp.poco")]
pub struct PineDemoApp {
    pub clicks: u32,

    pub dialog_open: bool,
    pub popover_open: bool,
    pub menu_open: bool,

    /// Last menu item the user picked. Shown in the demo so
    /// selection actually looks like it did something.
    pub last_menu_pick: String,

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
        self.last_menu_pick = "none".into();
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

    /// Delegated click handler for the menu — reads the selected
    /// item's label off `data-label` and records it, then closes
    /// the menu.
    pub fn pick_menu(&mut self, ev: web_sys::Event) {
        let Some(t) = ev.target() else { return };
        let Ok(start) = t.dyn_into::<Element>() else { return };
        let Ok(Some(item)) = start.closest("[data-label]") else {
            self.menu_open = false;
            return;
        };
        if item.get_attribute("aria-disabled").as_deref() == Some("true") {
            return;
        }
        if let Some(label) = item.get_attribute("data-label") {
            self.last_menu_pick = label;
        }
        self.menu_open = false;
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
    App::new().register::<PineDemoApp>().run();
}
