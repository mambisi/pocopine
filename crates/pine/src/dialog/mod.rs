//! `PineDialog` — modal dialog primitive.
//!
//! Manages: `pp-teleport` to `<body>`, `pp-if` on the `open` prop,
//! focus trap, body scroll lock, escape-to-close, overlay-click
//! dismiss. Authors fill in `<slot name="title">`, `<slot
//! name="description">`, and the default slot for body content;
//! Pine takes care of ARIA wiring.
//!
//! ```html
//! <pine-dialog pp-model="open">
//!   <template pp-slot="title">Delete file?</template>
//!   <template pp-slot="description">This can't be undone.</template>
//!   <button @click="confirm">Delete</button>
//! </pine-dialog>
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use pocopine::prelude::*;
use pocopine::{current_scope_id, focus, refs, scroll_lock, tick, ScopeId};
use serde::{Deserialize, Serialize};

thread_local! {
    /// Per-scope non-serializable runtime state. Populated on
    /// `activate`, drained on `deactivate` / `on_unmount`.
    static RUNTIME: RefCell<HashMap<ScopeId, DialogRuntime>> =
        RefCell::new(HashMap::new());
}

#[derive(Default)]
struct DialogRuntime {
    trap: Option<focus::TrapHandle>,
    saved: Option<focus::Saved>,
    locked: bool,
}

#[derive(Serialize, Deserialize)]
#[component(template = "PineDialog.poco")]
pub struct PineDialog {
    /// Controlled open / closed state. Use `pp-model="open"` on
    /// the tag to two-way bind.
    pub open: bool,
    /// Clicking the overlay (outside the content) closes the
    /// dialog. Defaults to `true`.
    pub dismiss_on_overlay: bool,
    /// Pressing Escape closes the dialog. Defaults to `true`.
    pub dismiss_on_escape: bool,
}

impl Default for PineDialog {
    fn default() -> Self {
        Self {
            open: false,
            dismiss_on_overlay: true,
            dismiss_on_escape: true,
        }
    }
}

#[handlers]
impl PineDialog {
    /// Reacts to `open` transitions. RFC-026's `#[watch]` sugar —
    /// the `#[handlers]` macro generates an auto `on_ready` that
    /// calls `watch_field::<bool, _>("open", …)` for us.
    #[watch(open)]
    fn on_open_change(&mut self, is_open: bool, prev: Option<bool>) {
        match (prev, is_open) {
            (None, true) | (Some(false), true) => {
                let Some(scope) = current_scope_id() else { return };
                // Give pp-if's teleport clone a tick to mount so
                // refs::get_on("content") resolves inside activate.
                tick::next(move || activate(scope));
            }
            (Some(true), false) => {
                if let Some(scope) = current_scope_id() {
                    deactivate(scope);
                }
            }
            _ => {}
        }
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            deactivate(scope);
        }
    }

    /// Fires when the user clicks the overlay (not the content).
    /// Templates wire via `@click.self="on_overlay_click"`.
    pub fn on_overlay_click(&mut self) {
        if self.dismiss_on_overlay {
            self.open = false;
            emit_from_host("pp:update:model", false);
        }
    }

    /// Fires on keydown.escape inside the dialog content.
    pub fn on_escape(&mut self) {
        if self.dismiss_on_escape {
            self.open = false;
            emit_from_host("pp:update:model", false);
        }
    }

    /// Imperative close. Authors can wire `@click="close"` on any
    /// button inside the dialog.
    pub fn close(&mut self) {
        self.open = false;
        emit_from_host("pp:update:model", false);
    }
}

fn activate(scope: ScopeId) {
    // Save focus + lock scroll unconditionally — both have symmetric
    // `deactivate` restoration.
    let saved = focus::save();
    scroll_lock::lock();

    // The content element is registered with `pp-ref="content"` on
    // the teleported subtree. If pp-if hasn't committed yet the
    // caller arranged `tick::next` before calling us; still tolerate
    // a None (deactivate cleanly on open→close before mount).
    let content = refs::get_on(scope, "content");

    let trap = content.as_ref().map(focus::trap);
    if let Some(el) = content.as_ref() {
        focus::auto_focus_first(el);
    }

    RUNTIME.with(|r| {
        r.borrow_mut().insert(
            scope,
            DialogRuntime {
                trap,
                saved: Some(saved),
                locked: true,
            },
        );
    });
}

fn deactivate(scope: ScopeId) {
    let Some(mut rt) = RUNTIME.with(|r| r.borrow_mut().remove(&scope)) else {
        return;
    };
    if let Some(trap) = rt.trap.take() {
        trap.release();
    }
    if rt.locked {
        scroll_lock::unlock();
    }
    // Blur first so soft keyboards dismiss on mobile even if the
    // saved element is gone; then restore.
    focus::blur();
    if let Some(saved) = rt.saved.take() {
        focus::restore(saved);
    }
}
