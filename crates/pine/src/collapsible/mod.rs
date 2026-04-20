//! `<pine-collapsible-*>` — open/close region primitive.
//!
//! Radix / reka-ui compound pattern. Three parts:
//!
//! - **Root** (`pine-collapsible-root`) — owns `open: bool`,
//!   provides its typed Handle to descendants.
//! - **Trigger** (`pine-collapsible-trigger`) — renders a button
//!   that toggles Root.open. Mirrors `open` into its own scope
//!   so `aria-expanded` / `data-state` bindings fire reactively.
//! - **Content** (`pine-collapsible-content`) — its body is
//!   gated by `pp-if` on the mirrored `open`; the substrate's
//!   pp-if scope-pinning fix (see `directives/if_.rs`) keeps the
//!   inner `<slot>` resolving against Content's scope.
//!
//! ```html
//! <pine-collapsible-root pp-model:open="open">
//!   <pine-collapsible-trigger>Toggle</pine-collapsible-trigger>
//!   <pine-collapsible-content>
//!     <p>Revealed body.</p>
//!   </pine-collapsible-content>
//! </pine-collapsible-root>
//! ```

use pocopine::prelude::*;
use pocopine::{emit_model, inject, inject_key, provide};
use serde::{Deserialize, Serialize};

// Provide/inject key for the Root handle. Descendants (Trigger,
// Content) inject to call `toggle` / mirror `open`.
inject_key!(ROOT: Handle<PineCollapsibleRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleRoot.poco", role = "scope")]
pub struct PineCollapsibleRoot {
    /// Open/closed. Two-way bindable via
    /// `pp-model:open="my_open"` on the tag; Root emits
    /// `pp:update:model` whenever the value changes.
    #[prop] pub open: bool,
}

#[handlers]
impl PineCollapsibleRoot {
    pub fn on_setup(&mut self) {
        provide(&ROOT, this::<Self>());
    }

    pub fn open_self(&mut self) {
        if !self.open {
            self.open = true;
            emit_model(true);
        }
    }
    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            emit_model(false);
        }
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
        emit_model(self.open);
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleTrigger.poco", role = "interactive")]
pub struct PineCollapsibleTrigger {
    #[mirror(via = ROOT)] pub open: bool,
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineCollapsibleTrigger {
    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineCollapsibleRoot| r.toggle());
        }
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleContent.poco", role = "panel")]
pub struct PineCollapsibleContent {
    #[mirror(via = ROOT)] pub open: bool,
}

#[handlers]
impl PineCollapsibleContent {}
