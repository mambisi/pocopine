//! `<pine-alert-dialog-*>` — modal alert / confirmation dialog.
//!
//! Same anatomy as `pine-dialog-*` with two semantic differences:
//!
//! - Content renders with `role="alertdialog"` instead of `"dialog"`,
//!   signalling to AT that it's an interruptive prompt.
//! - `dismiss_on_overlay` defaults to **false** — alert dialogs
//!   require an explicit choice, you can't click away from them.
//! - Two dedicated close buttons: **Action** (the confirmed choice,
//!   usually destructive) and **Cancel** (the safe abort). Both
//!   close the dialog and fire `pp:update:model` on Root.
//!
//! ```html
//! <pine-alert-dialog-root pp-model:open="delete_open">
//!   <pine-alert-dialog-trigger pp-as>
//!     <pine-button variant="destructive">Delete</pine-button>
//!   </pine-alert-dialog-trigger>
//!   <pine-alert-dialog-portal>
//!     <pine-alert-dialog-overlay></pine-alert-dialog-overlay>
//!     <pine-alert-dialog-content>
//!       <pine-alert-dialog-title>Delete project?</pine-alert-dialog-title>
//!       <pine-alert-dialog-description>
//!         This cannot be undone.
//!       </pine-alert-dialog-description>
//!       <pine-alert-dialog-cancel pp-as>
//!         <pine-button variant="ghost">Cancel</pine-button>
//!       </pine-alert-dialog-cancel>
//!       <pine-alert-dialog-action pp-as>
//!         <pine-button variant="destructive" @click="really_delete">
//!           Delete
//!         </pine-button>
//!       </pine-alert-dialog-action>
//!     </pine-alert-dialog-content>
//!   </pine-alert-dialog-portal>
//! </pine-alert-dialog-root>
//! ```

use crate::overlay;
use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, provide, refs, watch_scope_field};
use serde::{Deserialize, Serialize};

const ROOT_KEY: &str = "pine-alert-dialog-root";
const TITLE_ID_KEY: &str = "pine-alert-dialog-title-id";
const DESCRIPTION_ID_KEY: &str = "pine-alert-dialog-description-id";

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineAlertDialogRoot.poco")]
pub struct PineAlertDialogRoot {
    pub open: bool,
    /// Alert dialogs are modal by definition — kept as a prop for
    /// consistency with Dialog but changing it would undermine the
    /// "interruptive choice" semantic.
    pub modal: bool,
    /// Defaults to `false`: an alert dialog requires an explicit
    /// Action or Cancel click; clicking the overlay is a no-op.
    pub dismiss_on_overlay: bool,
    pub dismiss_on_escape: bool,
    pub title_id: String,
    pub description_id: String,
}

impl Default for PineAlertDialogRoot {
    fn default() -> Self {
        Self {
            open: false,
            modal: true,
            dismiss_on_overlay: false,
            dismiss_on_escape: true,
            title_id: String::new(),
            description_id: String::new(),
        }
    }
}

#[handlers]
impl PineAlertDialogRoot {
    pub fn on_setup(&mut self) {
        let Some(scope) = current_scope_id() else { return };
        self.title_id = format!("pine-alert-dialog-title-{}", scope.0);
        self.description_id = format!("pine-alert-dialog-description-{}", scope.0);
        provide(ROOT_KEY, this::<Self>());
        provide(TITLE_ID_KEY, self.title_id.clone());
        provide(DESCRIPTION_ID_KEY, self.description_id.clone());
    }

    pub fn open_dialog(&mut self) {
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

fn emit_from_self(open: bool) {
    let Some(scope) = current_scope_id() else { return };
    let Some(root_el) = refs::get_on(scope, "root") else { return };
    pocopine::emit_from(&root_el, "pp:update:model", open);
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAlertDialogTrigger.poco")]
pub struct PineAlertDialogTrigger {
    pub open: bool,
}

#[handlers]
impl PineAlertDialogTrigger {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) {
            self.open = root.with(|r| r.open);
        }
    }

    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) else { return };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
    }

    pub fn toggle(&mut self) {
        if let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) {
            root.update(|r: &mut PineAlertDialogRoot| r.toggle());
        }
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAlertDialogPortal.poco")]
pub struct PineAlertDialogPortal {
    pub open: bool,
}

#[handlers]
impl PineAlertDialogPortal {
    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) else { return };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
    }
}

// ── Overlay ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAlertDialogOverlay.poco")]
pub struct PineAlertDialogOverlay {}

#[handlers]
impl PineAlertDialogOverlay {
    pub fn on_click(&mut self) {
        if let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) {
            let dismiss = root.with(|r| r.dismiss_on_overlay);
            if dismiss {
                root.update(|r: &mut PineAlertDialogRoot| r.close());
            }
        }
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAlertDialogContent.poco")]
pub struct PineAlertDialogContent {
    pub title_id: String,
    pub description_id: String,
}

#[handlers]
impl PineAlertDialogContent {
    pub fn on_setup(&mut self) {
        if let Some(id) = inject::<String>(TITLE_ID_KEY) {
            self.title_id = id;
        }
        if let Some(id) = inject::<String>(DESCRIPTION_ID_KEY) {
            self.description_id = id;
        }
    }

    pub fn on_ready(&self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(content) = refs::get_on(scope, "content") else { return };
        let modal = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY)
            .map(|r| r.with(|root| root.modal))
            .unwrap_or(true);
        overlay::activate(scope, &content, modal);

        if let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) {
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
        if let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) {
            let dismiss = root.with(|r| r.dismiss_on_escape);
            if dismiss {
                root.update(|r: &mut PineAlertDialogRoot| r.close());
            }
        }
    }
}

// ── Title ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAlertDialogTitle.poco")]
pub struct PineAlertDialogTitle {
    pub title_id: String,
}

#[handlers]
impl PineAlertDialogTitle {
    pub fn on_setup(&mut self) {
        if let Some(id) = inject::<String>(TITLE_ID_KEY) {
            self.title_id = id;
        }
    }
}

// ── Description ───────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAlertDialogDescription.poco")]
pub struct PineAlertDialogDescription {
    pub description_id: String,
}

#[handlers]
impl PineAlertDialogDescription {
    pub fn on_setup(&mut self) {
        if let Some(id) = inject::<String>(DESCRIPTION_ID_KEY) {
            self.description_id = id;
        }
    }
}

// ── Action ────────────────────────────────────────────────────────

/// The confirmed-choice button — canonically the destructive path
/// (Delete, Sign out). Closes the dialog on click; authors wire the
/// actual side effect via their own `@click` on the hoisted
/// element (with `pp-as`) or on a child.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAlertDialogAction.poco")]
pub struct PineAlertDialogAction {}

#[handlers]
impl PineAlertDialogAction {
    pub fn click(&mut self) {
        if let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) {
            root.update(|r: &mut PineAlertDialogRoot| r.close());
        }
    }
}

// ── Cancel ────────────────────────────────────────────────────────

/// The safe-abort button. Closes the dialog. Focus-traps prefer
/// Cancel as the initial focus target per ARIA guidance — handled
/// naturally since Cancel is typically the first focusable inside
/// Content.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAlertDialogCancel.poco")]
pub struct PineAlertDialogCancel {}

#[handlers]
impl PineAlertDialogCancel {
    pub fn click(&mut self) {
        if let Some(root) = inject::<Handle<PineAlertDialogRoot>>(ROOT_KEY) {
            root.update(|r: &mut PineAlertDialogRoot| r.close());
        }
    }
}
