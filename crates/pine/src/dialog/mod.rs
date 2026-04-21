//! `<pine-dialog-*>` — modal dialog primitive (compound).
//!
//! Reka-ui / Radix anatomy. Eight parts:
//!
//! - **Root** (`pine-dialog-root`) — owns `open`, `modal`,
//!   generates the `title_id` + `description_id` ARIA ids.
//! - **Trigger** (`pine-dialog-trigger`) — button that toggles.
//! - **Portal** (`pine-dialog-portal`) — teleport wrapper.
//! - **Overlay** (`pine-dialog-overlay`) — backdrop; click-to-
//!   dismiss when `dismiss_on_overlay`.
//! - **Content** (`pine-dialog-content`) — `role="dialog"` panel;
//!   focus trap + scroll lock installed via `pine::overlay`.
//! - **Title** (`pine-dialog-title`) — renders `<h2>` with the
//!   Root-provided id for `aria-labelledby`.
//! - **Description** (`pine-dialog-description`) — renders `<p>`
//!   with the Root-provided id for `aria-describedby`.
//! - **Close** (`pine-dialog-close`) — button → `Root.close()`.
//!
//! ```html
//! <pine-dialog-root pp-model:open="open">
//!   <pine-dialog-trigger>Delete…</pine-dialog-trigger>
//!   <pine-dialog-portal>
//!     <pine-dialog-overlay></pine-dialog-overlay>
//!     <pine-dialog-content>
//!       <pine-dialog-title>Delete file?</pine-dialog-title>
//!       <pine-dialog-description>Cannot be undone.</pine-dialog-description>
//!       <pine-dialog-close>Cancel</pine-dialog-close>
//!     </pine-dialog-content>
//!   </pine-dialog-portal>
//! </pine-dialog-root>
//! ```

use crate::overlay;
use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, inject_key, provide, watch_scope_field};
use serde::{Deserialize, Serialize};

inject_key!(ROOT: Handle<PineDialogRoot>);
inject_key!(TITLE_ID: String);
inject_key!(DESCRIPTION_ID: String);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineDialogRoot.poco", role = "scope")]
pub struct PineDialogRoot {
    #[prop] pub open: bool,
    #[prop] pub modal: bool,
    #[prop] pub dismiss_on_overlay: bool,
    #[prop] pub dismiss_on_escape: bool,
    pub title_id: String,
    pub description_id: String,
}

impl Default for PineDialogRoot {
    fn default() -> Self {
        Self {
            open: false,
            modal: true,
            dismiss_on_overlay: true,
            dismiss_on_escape: true,
            title_id: String::new(),
            description_id: String::new(),
        }
    }
}

#[handlers]
impl PineDialogRoot {
    pub fn on_setup(&mut self) {
        let Some(scope) = current_scope_id() else { return };
        self.title_id = format!("pine-dialog-title-{}", scope.0);
        self.description_id = format!("pine-dialog-description-{}", scope.0);
        provide(&ROOT, this::<Self>());
        provide(&TITLE_ID, self.title_id.clone());
        provide(&DESCRIPTION_ID, self.description_id.clone());
    }

    // Note: emit from Root's own element (`pp-ref="root"`) via
    // `emit_from`, not plain `emit`. Plain `emit` uses
    // `current_el` which during a close initiated from Content
    // (teleported to <body>) isn't in Root's bubble path; the
    // pp-model listener on `<pine-dialog-root>` would miss the
    // event. Dispatching from Root's own element guarantees the
    // listener catches it regardless of who triggered the
    // handler (Trigger, Content.escape, Close, Overlay).
    pub fn open_dialog(&mut self) {
        if !self.open {
            self.open = true;
            pocopine::emit_model(true);
        }
    }
    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            pocopine::emit_model(false);
        }
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
        pocopine::emit_model(self.open);
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDialogTrigger.poco", role = "interactive")]
pub struct PineDialogTrigger {
    #[observe(ROOT)] pub open: bool,
}

#[handlers]
impl PineDialogTrigger {
    pub fn toggle(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineDialogRoot| r.toggle());
        }
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDialogPortal.poco", role = "scope")]
pub struct PineDialogPortal {
    #[observe(ROOT)] pub open: bool,
}

#[handlers]
impl PineDialogPortal {}

// ── Overlay ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDialogOverlay.poco", role = "panel")]
pub struct PineDialogOverlay {}

#[handlers]
impl PineDialogOverlay {
    pub fn on_click(&mut self) {
        if let Some(root) = inject(&ROOT) {
            let dismiss = root.with(|r| r.dismiss_on_overlay);
            if dismiss {
                root.update(|r: &mut PineDialogRoot| r.close());
            }
        }
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDialogContent.poco", role = "panel")]
pub struct PineDialogContent {
    pub title_id: String,
    pub description_id: String,
}

#[handlers]
impl PineDialogContent {
    pub fn on_setup(&mut self) {
        if let Some(id) = inject(&TITLE_ID) {
            self.title_id = id;
        }
        if let Some(id) = inject(&DESCRIPTION_ID) {
            self.description_id = id;
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs, scope: ScopeId) {
        // Install focus trap + scroll lock via the shared overlay
        // helper. Content is inside Portal's teleported subtree,
        // which `MutationObserver` on the mount root can't
        // observe — so on_unmount won't reliably fire when pp-if
        // yanks the portal. Instead watch root.open and
        // deactivate when it flips false; on_unmount stays as a
        // belt-and-braces backup for the non-teleport path.
        //
        // RFC-032 extractors (`Refs`, `ScopeId`) remove the scope
        // lookup + `refs::get_on` dance from the top of the hook.
        let Some(content) = refs.get("content") else { return };
        let modal = inject(&ROOT)
            .map(|r| r.with(|root| root.modal))
            .unwrap_or(true);
        overlay::activate(scope, &content, modal);

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

    pub fn on_escape(&mut self) {
        if let Some(root) = inject(&ROOT) {
            let dismiss = root.with(|r| r.dismiss_on_escape);
            if dismiss {
                root.update(|r: &mut PineDialogRoot| r.close());
            }
        }
    }
}

// ── Title ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDialogTitle.poco", role = "heading")]
pub struct PineDialogTitle {
    pub title_id: String,
}

#[handlers]
impl PineDialogTitle {
    pub fn on_setup(&mut self) {
        if let Some(id) = inject(&TITLE_ID) {
            self.title_id = id;
        }
    }
}

// ── Description ───────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDialogDescription.poco", role = "text")]
pub struct PineDialogDescription {
    pub description_id: String,
}

#[handlers]
impl PineDialogDescription {
    pub fn on_setup(&mut self) {
        if let Some(id) = inject(&DESCRIPTION_ID) {
            self.description_id = id;
        }
    }
}

// ── Close ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDialogClose.poco", role = "interactive")]
pub struct PineDialogClose {}

#[handlers]
impl PineDialogClose {
    pub fn click(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineDialogRoot| r.close());
        }
    }
}
