//! `<pine-toolbar-*>` — horizontal grouping of buttons / links.
//!
//! Mirrors Radix `<Toolbar>`. Root renders as `role="toolbar"`
//! with `pp-roving.both`, so Arrow keys move focus between items
//! regardless of orientation. Authors can freely compose nested
//! `<pine-toggle-group-*>`, `<pine-dropdown-menu-*>` triggers, or
//! raw buttons inside — the roving covers anything focusable.
//!
//! Four parts in v0:
//!
//! - **Root** (`pine-toolbar-root`) — container with roving focus
//!   and orientation.
//! - **Button** (`pine-toolbar-button`) — thin `<button>` wrapper
//!   (`pp-as` friendly — hoist your own `<button>` or `<pine-button>`).
//! - **Link** (`pine-toolbar-link`) — thin `<a>` wrapper.
//! - **Separator** (`pine-toolbar-separator`) — orientation-aware
//!   separator; picks up the root's orientation via inject.
//!
//! ```html
//! <pine-toolbar-root>
//!   <pine-toolbar-button @click="save">Save</pine-toolbar-button>
//!   <pine-toolbar-button @click="undo">Undo</pine-toolbar-button>
//!   <pine-toolbar-separator></pine-toolbar-separator>
//!   <pine-toggle-group-root type="multiple">
//!     <pine-toggle-group-item value="bold"><strong>B</strong></pine-toggle-group-item>
//!     <pine-toggle-group-item value="italic"><em>I</em></pine-toggle-group-item>
//!   </pine-toggle-group-root>
//! </pine-toolbar-root>
//! ```

use pocopine::prelude::*;
use pocopine::{inject, inject_key, provide};
use serde::{Deserialize, Serialize};

inject_key!(ORIENTATION: String);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineToolbarRoot.poco", role = "panel")]
pub struct PineToolbarRoot {
    /// `"horizontal"` (default) or `"vertical"`. Flows to
    /// `aria-orientation` + `data-orientation`; descendant
    /// Separator reads it via inject so it can swap its own
    /// orientation automatically.
    #[prop] pub orientation: String,
}

impl Default for PineToolbarRoot {
    fn default() -> Self {
        Self { orientation: "horizontal".into() }
    }
}

#[handlers]
impl PineToolbarRoot {
    pub fn on_setup(&mut self) {
        provide(&ORIENTATION, self.orientation.clone());
    }
}

// ── Button ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineToolbarButton.poco", role = "interactive")]
pub struct PineToolbarButton {
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineToolbarButton {}

// ── Link ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineToolbarLink.poco", role = "link")]
pub struct PineToolbarLink {
    #[prop] pub href: String,
}

#[handlers]
impl PineToolbarLink {}

// ── Separator ─────────────────────────────────────────────────────

/// Orientation-aware separator scoped to a toolbar. A toolbar
/// with `orientation="horizontal"` renders vertical separators
/// (perpendicular to the item axis) and vice versa — same rule
/// Radix uses.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineToolbarSeparator.poco", role = "panel")]
pub struct PineToolbarSeparator {
    /// Mirrored from the toolbar root's orientation (flipped).
    /// Authors don't set this; it's derived in `on_setup`.
    pub orientation: String,
}

#[handlers]
impl PineToolbarSeparator {
    pub fn on_setup(&mut self) {
        // Flip axis — horizontal toolbar → vertical separator.
        let parent = inject::<String>(&ORIENTATION).unwrap_or_else(|| "horizontal".into());
        self.orientation = if parent == "horizontal" {
            "vertical".into()
        } else {
            "horizontal".into()
        };
    }
}
