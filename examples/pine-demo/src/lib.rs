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

    /// Running count of menu actions fired — dropdown menus are
    /// for actions (Copy, Refresh, Sign out), not for selection.
    /// This counter goes up every time the user picks any menu
    /// action; individual handlers also have side-effects on
    /// other demo state so the action's reach is visible.
    pub actions_fired: u32,

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

    // ── Menu actions ─────────────────────────────────────────
    //
    // Dropdown menus are for *actions* (not selection). Each
    // handler does something real so clicking feels like a
    // command, not a state pick. All actions also close the menu.

    /// "Increment clicks" — bumps the PineButton demo counter.
    pub fn action_bump(&mut self) {
        self.clicks += 1;
        self.actions_fired += 1;
        self.menu_open = false;
    }

    /// "Toggle dark mode" — demonstrates a menu action mutating
    /// other demo state.
    pub fn action_toggle_dark(&mut self) {
        self.dark_mode = !self.dark_mode;
        self.actions_fired += 1;
        self.menu_open = false;
    }

    /// "Reset" — clears counters + brings everything back to
    /// defaults. Classic destructive menu action.
    pub fn action_reset(&mut self) {
        self.clicks = 0;
        self.actions_fired += 1; // counted before reset? decision:
                                 // keep actions_fired visible after
                                 // Reset so the user sees the click
                                 // registered. Reset everything *else*.
        self.tab = "account".into();
        self.dark_mode = false;
        self.agree_state = "unchecked".into();
        self.menu_open = false;
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
    App::new().register::<PineDemoApp>().run();
}
