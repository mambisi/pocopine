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

    /// Select demo — currently-selected city.
    pub selected_city: String,

    /// Combobox demo — currently-selected framework.
    pub selected_framework: String,

    /// Command palette — most recently invoked command label.
    pub last_command: String,

    /// ScrollArea demo — which `type` the user selected in the
    /// radio. Flows into the root's `type` attr so users can try
    /// `auto` / `always` / `scroll` / `hover` in place.
    pub scroll_type: String,

    /// Splitter demo — current `Vec<f64>` sizes. Bound two-way
    /// via `pp-model:sizes` on the Group so drags + keyboard
    /// steps flow back into this field and the UI shows the
    /// live layout.
    pub splitter_sizes: Vec<f64>,

    /// Tree demo — currently-selected path.
    pub tree_value: String,

    /// TagsInput demo — current list of tags. Bound two-way via
    /// `pp-model:values` on `<pine-tags-input-root>` so adds /
    /// removes / clears flow both directions.
    pub tags: Vec<String>,

    /// Second TagsInput demo — "Skills" variant with custom
    /// chip styling and a 5-tag cap.
    pub skills: Vec<String>,

    /// Third TagsInput demo — user mentions with pravatar
    /// avatars rendered inside each chip. Same primitive,
    /// the chip markup composes a <pine-avatar-root> alongside
    /// the item text.
    pub mentions: Vec<String>,

    /// Popover-triggered command palette — separate from the
    /// modal `<pine-command-root>` overlay. This field owns the
    /// popover's open state via `pp-model:open`.
    pub cmd_popover_open: bool,
    /// Last command fired from the popover palette.
    pub cmd_popover_last: String,

    /// RFC-039 perf stress fixture — 500 keyed items rendered
    /// inside a `pine-tags-input-root`. Toggleable so the demo
    /// stays light by default; click the button below to mount.
    pub stress_open: bool,
    pub stress_items: Vec<String>,
    /// Wall-clock duration of the most recent shuffle, in ms.
    /// Updated by `stress_shuffle` for visibility.
    pub stress_last_shuffle_ms: f64,
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
        self.scroll_type = "hover".into();
        self.splitter_sizes = vec![25.0, 50.0, 25.0];
        self.tree_value = String::new();
        self.tags = vec!["rust".into(), "wasm".into(), "pocopine".into()];
        self.skills = vec!["typescript".into(), "css".into()];
        self.mentions = vec!["ada".into(), "grace".into(), "linus".into()];
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

    // ── Command palette actions ──────────────────────────────

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
        self.dark_mode = !self.dark_mode;
    }
    pub fn cmd_settings(&mut self) {
        self.last_command = "Open Settings".into();
    }

    // ── Popover-command handlers ─────────────────────────────
    //
    // Each handler records the fired command and closes the
    // popover — the `<pine-command-root>` inside doesn't own
    // the outer popover's open state, so we nudge it shut
    // explicitly after a pick.
    pub fn pcmd_new_file(&mut self) {
        self.cmd_popover_last = "New File".into();
        self.cmd_popover_open = false;
    }
    pub fn pcmd_open_file(&mut self) {
        self.cmd_popover_last = "Open File".into();
        self.cmd_popover_open = false;
    }
    pub fn pcmd_save(&mut self) {
        self.cmd_popover_last = "Save".into();
        self.cmd_popover_open = false;
    }
    pub fn pcmd_format(&mut self) {
        self.cmd_popover_last = "Format Document".into();
        self.cmd_popover_open = false;
    }
    pub fn pcmd_find_file(&mut self) {
        self.cmd_popover_last = "Find in files".into();
        self.cmd_popover_open = false;
    }

    pub fn shuffle_tags(&mut self) {
        rotate(&mut self.tags);
    }
    pub fn shuffle_skills(&mut self) {
        rotate(&mut self.skills);
    }
    pub fn shuffle_mentions(&mut self) {
        rotate(&mut self.mentions);
    }

    pub fn stress_mount(&mut self) {
        if !self.stress_open {
            self.stress_items = (0..500).map(|i| format!("item-{i:03}")).collect();
            self.stress_open = true;
        } else {
            self.stress_items.clear();
            self.stress_open = false;
        }
    }

    pub fn stress_shuffle(&mut self) {
        let start = web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0);
        rotate(&mut self.stress_items);
        let end = web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0);
        self.stress_last_shuffle_ms = end - start;
    }
}

fn rotate(v: &mut Vec<String>) {
    if v.len() < 2 {
        return;
    }
    let head = v.remove(0);
    v.push(head);
}

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
    App::new().register::<PineDemoApp>().run();
}
