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
use pocopine::{current_scope_id, inject, inject_key, provide, refs, watch_scope_field};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::Element;

/// Resolve a selector string to an `Element` — used by Content's
/// programmatic anchor install.
fn resolve_anchor(selector: &str) -> Option<Element> {
    let s = selector.trim();
    if s.is_empty() {
        return None;
    }
    web_sys::window()?.document()?.query_selector(s).ok().flatten()
}

inject_key!(ROOT: Handle<PinePopoverRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PinePopoverRoot.poco")]
pub struct PinePopoverRoot {
    #[prop] pub open: bool,
    /// Non-modal by default — matches reka-ui. Set `true` to get
    /// a focus trap + scroll lock via the shared overlay helper.
    #[prop] pub modal: bool,
    #[prop] pub dismiss_on_outside: bool,
    #[prop] pub dismiss_on_escape: bool,
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
        provide(&ROOT, this::<Self>());
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
        if let Some(root) = inject(&ROOT) {
            self.open = root.with(|r| r.open);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        let Some(root) = inject(&ROOT) else { return };
        let root_scope = root.scope_id();
        watch_scope_field::<bool, _>(root_scope, "open", move |&is_open, _| {
            handle.update(|s| s.open = is_open);
        });
        // Stamp the button so Content's pp-anchor can target it
        // uniquely, mirroring DropdownMenu's auto-anchor scheme.
        if let Some(btn) = refs.get("trigger") {
            let _ = btn.set_attribute(
                "data-pine-popover-trigger",
                &format!("{}", root_scope.0),
            );
        }
    }

    pub fn toggle(&mut self) {
        if let Some(root) = inject(&ROOT) {
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
    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject(&ROOT) else { return };
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            handle.update(|s| s.open = is_open);
        });
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PinePopoverContent.poco")]
pub struct PinePopoverContent {
    /// Computed in `on_setup` from the injected root scope id —
    /// per-instance selector for anchor. Matches the
    /// `data-pine-popover-trigger="N"` stamp Trigger adds.
    pub anchor: String,
    #[prop] pub side: String,
    #[prop] pub align: String,
    #[prop] pub side_offset: f64,
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
        if let Some(root) = inject(&ROOT) {
            self.anchor = format!(
                "[data-pine-popover-trigger=\"{}\"]",
                root.scope_id().0
            );
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs, scope: ScopeId) {
        let Some(content) = refs.get("content") else { return };
        let modal = inject(&ROOT)
            .map(|r| r.with(|root| root.modal))
            .unwrap_or(false);
        overlay::activate(scope, &content, modal);

        // Program-install the anchor so side/align/side_offset
        // props flow through (pp-anchor's modifier syntax is
        // parsed statically at bind time).
        if let Some(anchor_el) = resolve_anchor(&self.anchor) {
            if let Ok(floater) = content.clone().dyn_into::<web_sys::HtmlElement>() {
                let placement = pocopine_core::directives::anchor::Placement {
                    side: pocopine_core::directives::anchor::Side::parse(&self.side),
                    align: pocopine_core::directives::anchor::Align::parse(&self.align),
                };
                pocopine_core::directives::anchor::install(
                    &floater, &anchor_el, placement, self.side_offset, true,
                );
            }
        }

        // Content is inside a teleported subtree; see the dialog
        // equivalent. Watch root.open and deactivate when it
        // flips false so focus + scroll lock release cleanly.
        if let Some(root) = inject(&ROOT) {
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
        if let Some(root) = inject(&ROOT) {
            let dismiss = root.with(|r| r.dismiss_on_outside);
            if dismiss {
                root.update(|r: &mut PinePopoverRoot| r.close());
            }
        }
    }

    pub fn on_escape(&mut self) {
        if let Some(root) = inject(&ROOT) {
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
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PinePopoverRoot| r.close());
        }
    }
}
