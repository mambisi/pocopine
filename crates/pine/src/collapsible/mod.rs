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
use pocopine::{inject, provide, watch_scope_field};
use serde::{Deserialize, Serialize};

/// Provide/inject key for the Root handle. Descendants (Trigger,
/// Content) inject to call `toggle` / mirror `open`.
const ROOT_KEY: &str = "pine-collapsible-root";

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleRoot.poco")]
pub struct PineCollapsibleRoot {
    /// Open/closed. Two-way bindable via
    /// `pp-model:open="my_open"` on the tag; Root emits
    /// `pp:update:model` whenever the value changes.
    pub open: bool,
}

#[handlers]
impl PineCollapsibleRoot {
    pub fn on_setup(&mut self) {
        provide(ROOT_KEY, this::<Self>());
    }

    #[watch(open)]
    fn on_open_change(&mut self, is_open: bool, prev: Option<bool>) {
        // Skip the initial-read emission (prev=None) so we don't
        // clobber the parent's incoming bound value with the
        // default on mount.
        if prev.is_some() {
            emit("pp:update:model", is_open);
        }
    }

    pub fn open_self(&mut self) {
        self.open = true;
    }
    pub fn close(&mut self) {
        self.open = false;
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleTrigger.poco")]
pub struct PineCollapsibleTrigger {
    pub open: bool,
    pub disabled: bool,
}

#[handlers]
impl PineCollapsibleTrigger {
    pub fn on_setup(&mut self) {
        // Read the initial open state from the injected root so
        // the template's first bind of `:aria-expanded` /
        // `:data-state` sees the correct value.
        if let Some(root) = inject::<Handle<PineCollapsibleRoot>>(ROOT_KEY) {
            self.open = root.with(|r| r.open);
        }
    }

    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PineCollapsibleRoot>>(ROOT_KEY) else {
            return;
        };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
    }

    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        if let Some(root) = inject::<Handle<PineCollapsibleRoot>>(ROOT_KEY) {
            root.update(|r: &mut PineCollapsibleRoot| r.toggle());
        }
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleContent.poco")]
pub struct PineCollapsibleContent {
    pub open: bool,
}

#[handlers]
impl PineCollapsibleContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject::<Handle<PineCollapsibleRoot>>(ROOT_KEY) {
            self.open = root.with(|r| r.open);
        }
    }

    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PineCollapsibleRoot>>(ROOT_KEY) else {
            return;
        };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
    }
}
