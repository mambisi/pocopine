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
use pocopine::{current_scope_id, inject, inject_key, provide, refs, watch_scope_field};
use serde::{Deserialize, Serialize};

// Provide/inject key for the Root handle. Descendants (Trigger,
// Content) inject to call `toggle` / mirror `open`.
inject_key!(ROOT: Handle<PineCollapsibleRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleRoot.poco")]
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
            emit_from_self(true);
        }
    }
    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            emit_from_self(false);
        }
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
        emit_from_self(self.open);
    }
}

/// Emit from Root's own element — same pattern as Dialog /
/// Popover. Collapsible doesn't teleport, so plain `emit` would
/// also work here (current_el inside Trigger.click bubbles to
/// Root's tag). Keeping the explicit emit-from-self for symmetry
/// and to stay correct if someone wraps Content in a teleport.
fn emit_from_self(open: bool) {
    let Some(scope) = current_scope_id() else { return };
    let Some(root_el) = refs::get_on(scope, "root") else { return };
    pocopine::emit_from(&root_el, "pp:update:model", open);
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCollapsibleTrigger.poco")]
pub struct PineCollapsibleTrigger {
    pub open: bool,
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineCollapsibleTrigger {
    pub fn on_setup(&mut self) {
        // Read the initial open state from the injected root so
        // the template's first bind of `:aria-expanded` /
        // `:data-state` sees the correct value.
        if let Some(root) = inject(&ROOT) {
            self.open = root.with(|r| r.open);
        }
    }

    pub fn on_ready(&self) {
        let Some(root) = inject(&ROOT) else {
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
        if let Some(root) = inject(&ROOT) {
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
        if let Some(root) = inject(&ROOT) {
            self.open = root.with(|r| r.open);
        }
    }

    pub fn on_ready(&self) {
        let Some(root) = inject(&ROOT) else {
            return;
        };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
    }
}
