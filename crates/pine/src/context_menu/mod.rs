//! `<pine-context-menu-*>` — right-click menu (compound).
//!
//! Reka-ui / Radix anatomy. Same mental model as DropdownMenu but
//! opened by a `contextmenu` event (right-click / two-finger tap /
//! Shift+F10) at the pointer's position, not by clicking a button.
//! The menu positions itself absolutely at the captured pointer
//! coordinates — no anchored element.
//!
//! Six parts ship in v0:
//!
//! - **Root** (`pine-context-menu-root`) — owns `open` plus the
//!   captured pointer `x` / `y`.
//! - **Trigger** (`pine-context-menu-trigger`) — wraps the author
//!   content; installs a `contextmenu` listener in `on_ready`,
//!   captures coords, prevents the browser's native menu, opens
//!   Root.
//! - **Portal** (`pine-context-menu-portal`) — teleport wrapper
//!   gated on Root.open.
//! - **Content** (`pine-context-menu-content`) — positioned at the
//!   captured pointer; click-outside and Escape dismiss.
//! - **Item** (`pine-context-menu-item`) — single menuitem;
//!   closes Root on click unless the author calls
//!   `event.preventDefault()` on the cancelable `pp:select`
//!   event.
//! - **Separator** (`pine-context-menu-separator`) — visual rule,
//!   `role="separator"`.
//!
//! CheckboxItem / RadioGroup / Sub follow in a later round — the
//! full DropdownMenu surface is ~13 parts; v0 ships the 6 most
//! authors need.
//!
//! ```html
//! <pine-context-menu-root>
//!   <pine-context-menu-trigger>
//!     <div class="canvas">Right-click anywhere in me.</div>
//!   </pine-context-menu-trigger>
//!   <pine-context-menu-portal>
//!     <pine-context-menu-content>
//!       <pine-context-menu-item @click="copy">Copy</pine-context-menu-item>
//!       <pine-context-menu-item @click="paste">Paste</pine-context-menu-item>
//!       <pine-context-menu-separator></pine-context-menu-separator>
//!       <pine-context-menu-item @click="delete_it">Delete</pine-context-menu-item>
//!     </pine-context-menu-content>
//!   </pine-context-menu-portal>
//! </pine-context-menu-root>
//! ```

use pocopine::prelude::*;
use pocopine::{focus, inject, inject_key, provide, watch_scope_field};
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Event, EventTarget, MouseEvent};

inject_key!(ROOT: Handle<PineContextMenuRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineContextMenuRoot.poco", role = "scope")]
pub struct PineContextMenuRoot {
    #[prop] pub open: bool,
    /// Viewport-relative pointer coordinates captured at the last
    /// `contextmenu` event. Content reads these to position
    /// itself. Stored as f64 for symmetry with `getBoundingClientRect`.
    pub pointer_x: f64,
    pub pointer_y: f64,
}

impl Default for PineContextMenuRoot {
    fn default() -> Self {
        Self {
            open: false,
            pointer_x: 0.0,
            pointer_y: 0.0,
        }
    }
}

#[handlers]
impl PineContextMenuRoot {
    pub fn on_setup(&mut self) {
        provide(&ROOT, this::<Self>());
    }

    pub fn open_at(&mut self, x: f64, y: f64) {
        self.pointer_x = x;
        self.pointer_y = y;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineContextMenuTrigger.poco", role = "scope")]
pub struct PineContextMenuTrigger {}

#[handlers]
impl PineContextMenuTrigger {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = inject(&ROOT) else { return };
        let Some(el) = refs.get("trigger") else { return };

        let root_for_closure = root.clone();
        let closure = Closure::wrap(Box::new(move |ev: Event| {
            // `contextmenu` is a MouseEvent — cast to grab client
            // coords before suppressing the browser's native menu.
            let me: MouseEvent = ev.clone().dyn_into().unwrap_or_else(|_| {
                // Shouldn't happen for a contextmenu event; fall
                // back to (0,0) so we still open.
                MouseEvent::new("contextmenu").unwrap()
            });
            let x = me.client_x() as f64;
            let y = me.client_y() as f64;
            ev.prevent_default();
            root_for_closure.update(|r: &mut PineContextMenuRoot| r.open_at(x, y));
        }) as Box<dyn FnMut(Event)>);

        let target: &EventTarget = el.as_ref();
        let _ = target.add_event_listener_with_callback(
            "contextmenu",
            closure.as_ref().unchecked_ref(),
        );
        closure.forget();
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineContextMenuPortal.poco", role = "scope")]
pub struct PineContextMenuPortal {
    pub open: bool,
}

#[handlers]
impl PineContextMenuPortal {
    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject(&ROOT) else { return };
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            handle.update(|s| s.open = is_open);
        });
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineContextMenuContent.poco", role = "list", transition = "slide-down")]
pub struct PineContextMenuContent {
    /// Mirrored from Root on open so the template's inline style
    /// positions this panel at the captured pointer. Read once at
    /// open time; not reactive to pointer motion (the menu doesn't
    /// follow the cursor after it opens).
    pub pointer_x: f64,
    pub pointer_y: f64,
}

#[handlers]
impl PineContextMenuContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            let (x, y) = root.with(|r| (r.pointer_x, r.pointer_y));
            self.pointer_x = x;
            self.pointer_y = y;
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs) {
        // Roving + auto-focus the first item once the teleported
        // clone has committed — identical path to DropdownMenu's
        // menu root, just a different ref name.
        let Some(menu) = refs.get("menu") else { return };
        init_roving_tabindex(&menu);
        focus::auto_focus_first(&menu);
    }

    pub fn close(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineContextMenuRoot| r.close());
        }
    }
}

/// Set `tabindex=0` on the first enabled `role="menuitem"` inside
/// the menu so keyboard users have a focus target as soon as the
/// menu opens. Other items stay `tabindex=-1`; `pp-roving` manages
/// the rest from there.
fn init_roving_tabindex(menu: &web_sys::Element) {
    let items = menu
        .query_selector_all("[role=\"menuitem\"]:not([data-disabled=\"true\"])")
        .ok();
    let Some(items) = items else { return };
    for i in 0..items.length() {
        if let Some(node) = items.item(i) {
            if let Ok(el) = node.dyn_into::<web_sys::Element>() {
                let tabindex = if i == 0 { "0" } else { "-1" };
                let _ = el.set_attribute("tabindex", tabindex);
            }
        }
    }
}

// ── Item ──────────────────────────────────────────────────────────

/// Dismissable menuitem — mirrors DropdownMenu's Item semantics.
/// Fires a cancelable `pp:select` event; if an author listener
/// calls `preventDefault()`, the menu stays open (useful for a
/// "keep menu open while I do an async thing" pattern).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineContextMenuItem.poco", role = "item")]
pub struct PineContextMenuItem {
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineContextMenuItem {
    pub fn on_select(&mut self) {
        if self.disabled {
            return;
        }
        let prevented = pocopine::emit_cancelable("pp:select", ());
        if prevented {
            return;
        }
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineContextMenuRoot| r.close());
        }
    }
}

// ── Separator ─────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineContextMenuSeparator.poco", role = "item")]
pub struct PineContextMenuSeparator {}

#[handlers]
impl PineContextMenuSeparator {}
