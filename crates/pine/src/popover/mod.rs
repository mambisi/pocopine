//! `<pine-popover-*>` — non-modal floating panel primitive.
//!
//! Reka-ui anatomy. Five parts:
//!
//! - **Root** (`pine-popover-root`) — owns `open` + `modal`,
//!   provides Handle.
//! - **Trigger** (`pine-popover-trigger`) — button toggles Root,
//!   stamps `data-pine-popover-trigger="{scope_id}"` so Content
//!   auto-anchors via the same mechanism as DropdownMenu.
//! - **Portal** (`pine-popover-portal`) — teleport wrapper.
//! - **Content** (`pine-popover-content`) — `role="dialog"` with
//!   `pp-anchor` targeting the Trigger, escape + click-outside
//!   dismiss.
//! - **Close** (`pine-popover-close`) — button → `Root.close()`.
//!
//! ```html
//! <pine-popover-root pp-model:open="open">
//!   <pine-popover-trigger>Open</pine-popover-trigger>
//!   <pine-popover-portal>
//!     <pine-popover-content>
//!       <p>Panel content.</p>
//!       <pine-popover-close>Close</pine-popover-close>
//!     </pine-popover-content>
//!   </pine-popover-portal>
//! </pine-popover-root>
//! ```

use crate::overlay;
use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, provide, refs, watch_scope_field};
use serde::{Deserialize, Serialize};

const ROOT_KEY: &str = "pine-popover-root";

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PinePopoverRoot.poco")]
pub struct PinePopoverRoot {
    pub open: bool,
    /// Non-modal by default — matches reka-ui. Set `true` to get
    /// a focus trap + scroll lock via the shared overlay helper.
    pub modal: bool,
    pub dismiss_on_outside: bool,
    pub dismiss_on_escape: bool,
}

impl Default for PinePopoverRoot {
    fn default() -> Self {
        Self {
            open: false,
            modal: false,
            dismiss_on_outside: true,
            dismiss_on_escape: true,
        }
    }
}

#[handlers]
impl PinePopoverRoot {
    pub fn on_setup(&mut self) {
        provide(ROOT_KEY, this::<Self>());
    }

    pub fn open_popover(&mut self) {
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

/// Emit `pp:update:model` from Root's element (via `pp-ref="root"`)
/// so the parent's pp-model listener catches it even when the
/// change was initiated from Content (teleported to `<body>`).
fn emit_from_self(open: bool) {
    let Some(scope) = current_scope_id() else { return };
    let Some(root_el) = refs::get_on(scope, "root") else { return };
    pocopine::emit_from(&root_el, "pp:update:model", open);
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePopoverTrigger.poco")]
pub struct PinePopoverTrigger {
    pub open: bool,
}

#[handlers]
impl PinePopoverTrigger {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) {
            self.open = root.with(|r| r.open);
        }
    }

    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) else { return };
        let root_scope = root.scope_id();
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root_scope, "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
        // Stamp the button so Content's pp-anchor can target it
        // uniquely, mirroring DropdownMenu's auto-anchor scheme.
        if let Some(scope) = current_scope_id() {
            if let Some(btn) = refs::get_on(scope, "trigger") {
                let _ = btn.set_attribute(
                    "data-pine-popover-trigger",
                    &format!("{}", root_scope.0),
                );
            }
        }
    }

    pub fn toggle(&mut self) {
        if let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) {
            root.update(|r: &mut PinePopoverRoot| r.toggle());
        }
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePopoverPortal.poco")]
pub struct PinePopoverPortal {
    pub open: bool,
}

#[handlers]
impl PinePopoverPortal {
    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) else { return };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePopoverContent.poco")]
pub struct PinePopoverContent {
    /// Computed in `on_setup` from the injected root scope id —
    /// per-instance selector for pp-anchor. Matches the
    /// `data-pine-popover-trigger="N"` stamp Trigger adds.
    pub anchor: String,
}

#[handlers]
impl PinePopoverContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) {
            self.anchor = format!(
                "[data-pine-popover-trigger=\"{}\"]",
                root.scope_id().0
            );
        }
    }

    pub fn on_ready(&self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(content) = refs::get_on(scope, "content") else { return };
        let modal = inject::<Handle<PinePopoverRoot>>(ROOT_KEY)
            .map(|r| r.with(|root| root.modal))
            .unwrap_or(false);
        overlay::activate(scope, &content, modal);

        // Content is inside a teleported subtree; see the dialog
        // equivalent. Watch root.open and deactivate when it
        // flips false so focus + scroll lock release cleanly.
        if let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) {
            watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, prev| {
                if prev == Some(&true) && !is_open {
                    overlay::deactivate(scope);
                }
            });
        }
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            overlay::deactivate(scope);
        }
    }

    pub fn on_outside(&mut self) {
        if let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) {
            let dismiss = root.with(|r| r.dismiss_on_outside);
            if dismiss {
                root.update(|r: &mut PinePopoverRoot| r.close());
            }
        }
    }

    pub fn on_escape(&mut self) {
        if let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) {
            let dismiss = root.with(|r| r.dismiss_on_escape);
            if dismiss {
                root.update(|r: &mut PinePopoverRoot| r.close());
            }
        }
    }
}

// ── Close ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePopoverClose.poco")]
pub struct PinePopoverClose {}

#[handlers]
impl PinePopoverClose {
    pub fn click(&mut self) {
        if let Some(root) = inject::<Handle<PinePopoverRoot>>(ROOT_KEY) {
            root.update(|r: &mut PinePopoverRoot| r.close());
        }
    }
}
