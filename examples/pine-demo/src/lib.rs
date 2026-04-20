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
    pub alert_dialog_open: bool,
    pub popover_open: bool,

    /// Set by the alert dialog's Action button — shows the user
    /// made the destructive choice.
    pub last_action: String,

    /// Running count of menu actions fired — dropdown menus are
    /// for actions (Copy, Refresh, Sign out), not for selection.
    /// This counter goes up every time the user picks any menu
    /// action; individual handlers also have side-effects on
    /// other demo state so the action's reach is visible.
    pub actions_fired: u32,

    pub tab: String,

    pub dark_mode: bool,
    pub agree_state: String,

    /// Toggled by checkbox-items in the second dropdown menu.
    /// Bound via `pp-model:state` so the tri-state String flows
    /// both ways through the compound's pp:update:model event.
    pub show_muted: String,
    pub show_archived: String,

    /// Exclusive radio-group value. `pp-model:value` on the
    /// RadioGroup tag keeps this in sync with whichever RadioItem
    /// is currently selected.
    pub density: String,

    /// Standalone RadioGroup demo value — the selected plan tier.
    pub plan: String,

    /// Standalone Toggle — "bold" pressed-state.
    pub bold: bool,

    /// ToggleGroup single-mode (text alignment, one of left/center/right).
    pub align: String,

    /// ToggleGroup multiple-mode (any combination of format flags).
    pub format: Vec<String>,

    /// Collapsible open state — bound two-way via
    /// `pp-model:open` on `<pine-collapsible-root>`.
    pub faq_open: bool,

    /// Accordion single-mode value — which FAQ item is open
    /// (or `""` when all collapsed).
    pub faq_item: String,

    /// OTP Field demo — the verification code the user has
    /// typed so far. Bound two-way via `pp-model:value` on
    /// `<pine-otp-field>`.
    pub otp_code: String,

    /// Slider demo — current volume 0..100, bound two-way via
    /// `pp-model:value` on `<pine-slider-root>`.
    pub volume: f64,
}

#[handlers]
impl PineDemoApp {
    pub fn on_mount(&mut self) {
        self.tab = "account".into();
        self.agree_state = "unchecked".into();
        self.show_muted = "unchecked".into();
        self.show_archived = "unchecked".into();
        self.density = "comfortable".into();
        self.plan = "free".into();
        self.align = "left".into();
        self.volume = 40.0;
    }

    pub fn bump(&mut self) {
        self.clicks += 1;
    }

    /// Alert-dialog Action handler — records the destructive choice.
    pub fn confirm_destroy(&mut self) {
        self.last_action = "destroyed".into();
    }

    // ── Menu actions ─────────────────────────────────────────
    //
    // Dropdown menus are for *actions* (not selection). Each
    // handler does something real so clicking feels like a
    // command, not a state pick. All actions also close the menu.

    /// "Increment clicks" — bumps the PineButton demo counter.
    /// Pine's DropdownMenu Item closes the menu on click; these
    /// handlers only manage the author's own demo state.
    pub fn action_bump(&mut self) {
        self.clicks += 1;
        self.actions_fired += 1;
    }

    /// "Toggle dark mode" — demonstrates a menu action mutating
    /// other demo state.
    pub fn action_toggle_dark(&mut self) {
        self.dark_mode = !self.dark_mode;
        self.actions_fired += 1;
    }

    /// "Reset" — clears counters + brings everything back to
    /// defaults. Classic destructive menu action.
    pub fn action_reset(&mut self) {
        self.clicks = 0;
        self.actions_fired += 1;
        self.tab = "account".into();
        self.dark_mode = false;
        self.agree_state = "unchecked".into();
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
    App::new().register::<PineDemoApp>().run();
}
