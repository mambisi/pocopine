//! `pine-workspace` showcase (RFC-105) — a content-heavy multi-region
//! shell: resizable rail/sidebar │ main │ a resizable detail Aside │ a
//! collapsible bottom console.
//!
//! `pine-layout` ships **zero CSS** — the whole frame is the author CSS in
//! `styles.css`, keyed off the regions' `data-*` state + the
//! `--pine-*-size` width/height custom properties. Region state is held in
//! app fields and two-way-bound with `pp-model`. Drag the splitters to
//! resize; narrow the window to watch the sidebar auto-rail.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(display = "contents")]
pub struct WorkspaceDemo {
    /// Live breakpoint (badge), bound from `<pine-workspace>`.
    pub bp: String,
    /// Sidebar collapsed-to-rail state (`pp-model:collapsed`).
    pub sidebar_collapsed: bool,
    /// Aside open (`pp-model:open`).
    pub aside_open: bool,
    /// Bottom console open (`pp-model:open`).
    pub bottom_open: bool,
}

impl Default for WorkspaceDemo {
    fn default() -> Self {
        Self {
            bp: String::new(),
            sidebar_collapsed: false,
            aside_open: true,
            bottom_open: false,
        }
    }
}

#[handlers]
impl WorkspaceDemo {
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
    }
    pub fn toggle_aside(&mut self) {
        self.aside_open = !self.aside_open;
    }
    pub fn toggle_bottom(&mut self) {
        self.bottom_open = !self.bottom_open;
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    pine_layout::register_all();
    App::new().register::<WorkspaceDemo>().run();
}
