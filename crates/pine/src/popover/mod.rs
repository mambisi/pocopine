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

use crate::compound;
use crate::overlay;
use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id, watch_scope_field};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

const SLUG: &str = "popover";

create_context!(ROOT: Handle<PinePopoverRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PinePopoverRoot.poco", role = "scope")]
// RFC 049 — Popover's Root holds exactly Trigger + Portal.
// Popover has no Overlay (non-modal by default) so Portal's
// sole child is Content.
#[slot(default, only = [PinePopoverTrigger, PinePopoverPortal])]
pub struct PinePopoverRoot {
    #[model]
    pub open: bool,
    /// Non-modal by default — matches reka-ui. Set `true` to get
    /// a focus trap + scroll lock via the shared overlay helper.
    #[prop]
    pub modal: bool,
    #[prop]
    pub dismiss_on_outside: bool,
    #[prop]
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
        ROOT.provide(this::<Self>());
    }

    pub fn open_popover(&mut self) {
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
#[component(template = "PinePopoverTrigger.poco", role = "interactive")]
pub struct PinePopoverTrigger {
    #[observe(ROOT)]
    pub open: bool,
}

#[handlers]
impl PinePopoverTrigger {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        // Stamp the button so Content's pp-anchor can target it
        // uniquely, mirroring DropdownMenu's auto-anchor scheme.
        if let Some(btn) = refs.get("trigger") {
            compound::stamp_trigger(&btn, root.scope_id(), SLUG);
        }
    }

    pub fn toggle(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PinePopoverRoot| r.toggle());
        }
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePopoverPortal.poco", role = "scope")]
// RFC 049 — Portal wraps the positioned Content panel; no
// overlay layer since Popover is non-modal by default.
#[slot(default, only = [PinePopoverContent])]
pub struct PinePopoverPortal {
    #[observe(ROOT)]
    pub open: bool,
}

#[handlers]
impl PinePopoverPortal {}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(
    template = "PinePopoverContent.poco",
    role = "panel",
    transition = "slide-down"
)]
// RFC 049 — Popover content is author-driven (form, text,
// nested components); the only semantic pocopine part is
// Close. `accepts` so arbitrary wrapping HTML still works.
#[slot(default, accepts = [PinePopoverClose])]
pub struct PinePopoverContent {
    /// Computed in `on_setup` from the injected root scope id —
    /// per-instance selector for anchor. Matches the
    /// `data-pine-popover-trigger="N"` stamp Trigger adds.
    pub anchor: String,
    #[prop]
    pub side: String,
    #[prop]
    pub align: String,
    #[prop]
    pub side_offset: f64,
}

impl Default for PinePopoverContent {
    fn default() -> Self {
        Self {
            anchor: String::new(),
            side: "bottom".into(),
            align: "start".into(),
            side_offset: 4.0,
        }
    }
}

#[handlers]
impl PinePopoverContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = ROOT.inject() {
            self.anchor = compound::trigger_selector(root.scope_id(), SLUG);
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs, scope: ScopeId) {
        let Some(content) = refs.get("content") else {
            return;
        };
        let modal = ROOT
            .inject()
            .map(|r| r.with(|root| root.modal))
            .unwrap_or(false);
        overlay::activate(scope, &content, modal);

        // Exempt our own trigger from the `@click.outside` check
        // so reclicking an open popover's trigger closes cleanly
        // instead of racing between outside-close (capture) and
        // trigger-toggle (bubble). See
        // `directives/on.rs` `data-pp-outside-exempt` handling.
        if let Some(root) = ROOT.inject() {
            let _ = content.set_attribute(
                "data-pp-outside-exempt",
                &compound::trigger_selector(root.scope_id(), SLUG),
            );
        }

        // Program-install the anchor so side/align/side_offset
        // props flow through (pp-anchor's modifier syntax is
        // parsed statically at bind time).
        if let Ok(floater) = content.clone().dyn_into::<web_sys::HtmlElement>() {
            if let Some(root) = ROOT.inject() {
                compound::install_anchor_to_trigger(
                    &floater,
                    root.scope_id(),
                    SLUG,
                    &self.side,
                    &self.align,
                    self.side_offset,
                    true,
                );
            }
        }

        // Content is inside a teleported subtree; see the dialog
        // equivalent. Watch root.open and deactivate when it
        // flips false so focus + scroll lock release cleanly.
        if let Some(root) = ROOT.inject() {
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
        if let Some(root) = ROOT.inject() {
            let dismiss = root.with(|r| r.dismiss_on_outside);
            if dismiss {
                root.update(|r: &mut PinePopoverRoot| r.close());
            }
        }
    }

    pub fn on_escape(&mut self) {
        if let Some(root) = ROOT.inject() {
            let dismiss = root.with(|r| r.dismiss_on_escape);
            if dismiss {
                root.update(|r: &mut PinePopoverRoot| r.close());
            }
        }
    }
}

// ── Close ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinePopoverClose.poco", role = "interactive")]
pub struct PinePopoverClose {}

#[handlers]
impl PinePopoverClose {
    pub fn click(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PinePopoverRoot| r.close());
        }
    }
}
