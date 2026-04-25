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

use pocopine::create_context;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

// Provide/inject key for the Root handle. Descendants (Trigger,
// Content) inject to call `toggle` / mirror `open`.
create_context!(ROOT: Handle<PineCollapsibleRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleRoot.poco", role = "scope")]
// RFC 049 — Collapsible is exactly Trigger + Content. Same
// shape as a single Accordion item at the top level.
#[slot(default, only = [PineCollapsibleTrigger, PineCollapsibleContent])]
pub struct PineCollapsibleRoot {
    /// Open/closed. Two-way bindable via
    /// `pp-model:open="my_open"` on the tag; Root emits
    /// `pp:update:model` whenever the value changes.
    #[model]
    pub open: bool,
}

#[handlers]
impl PineCollapsibleRoot {
    fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
    }

    pub fn open_self(&mut self) {
        if !self.open {
            self.open = true;
        }
    }
    pub fn close(&mut self) {
        if self.open {
            self.open = false;
        }
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleTrigger.poco", role = "interactive")]
pub struct PineCollapsibleTrigger {
    #[observe(ROOT)]
    pub open: bool,
    #[prop]
    pub disabled: bool,
}

#[handlers]
impl PineCollapsibleTrigger {
    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineCollapsibleRoot| r.toggle());
        }
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
// `transition` lives on the CLONE root inside the .poco template
// (search "pp-transition" in PineCollapsibleContent.poco), not on
// this macro arg, because the component's pp-if puts the cloned
// subtree outside the macro-stamp scope.
#[component(template = "PineCollapsibleContent.poco", role = "panel")]
pub struct PineCollapsibleContent {
    #[observe(ROOT)]
    pub open: bool,
}

#[handlers]
impl PineCollapsibleContent {}
