//! `<pine-dropdown-menu-*>` — compound menu primitive.
//!
//! Radix-style anatomy: state owned by `Root`, rendered via named
//! sub-parts (Trigger, Portal, Content, Item) that talk to Root via
//! RFC-027 `provide`/`inject`. Every sub-part has its own scope and
//! ARIA role; Root has no DOM of its own (pure state container).
//!
//! ```html
//! <pine-dropdown-menu-root>
//!   <pine-dropdown-menu-trigger>Actions ▾</pine-dropdown-menu-trigger>
//!   <pine-dropdown-menu-portal>
//!     <pine-dropdown-menu-content anchor="[data-pine-dm-trigger]">
//!       <pine-dropdown-menu-item @click="bump">Bump</pine-dropdown-menu-item>
//!       <pine-dropdown-menu-item disabled>Export</pine-dropdown-menu-item>
//!     </pine-dropdown-menu-content>
//!   </pine-dropdown-menu-portal>
//! </pine-dropdown-menu-root>
//! ```
//!
//! Author-provided `anchor` on Content is a CSS selector (or
//! ref name). Trigger sets `data-pine-dm-trigger` on its button
//! so the author can target it via attribute selector; nothing
//! forces them to — they can also anchor to a custom element in
//! the surrounding layout.

use pocopine::prelude::*;
use pocopine::{current_scope_id, focus, inject, provide, refs, watch_scope_field};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::Element;

/// Provide/inject key for the Root handle.
const ROOT_KEY: &str = "pine-dm-root";

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuRoot.poco")]
pub struct PineDropdownMenuRoot {
    /// Open state. Two-way bindable via `pp-model:open="current"`
    /// on the tag.
    pub open: bool,
}

#[handlers]
impl PineDropdownMenuRoot {
    pub fn on_mount(&mut self) {
        provide(ROOT_KEY, this::<Self>());
    }

    pub fn open_menu(&mut self) {
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
#[component(template = "PineDropdownMenuTrigger.poco")]
pub struct PineDropdownMenuTrigger {
    /// Mirrored from Root.open so the template's `:aria-expanded`
    /// and `:data-state` bindings fire reactively.
    pub open: bool,
}

#[handlers]
impl PineDropdownMenuTrigger {
    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(ROOT_KEY) else {
            return;
        };
        let root_scope = root.scope_id();
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root_scope, "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
        if let Some(scope) = current_scope_id() {
            if let Some(btn) = refs::get_on(scope, "trigger") {
                let _ = btn.set_attribute("data-pine-dm-trigger", "");
            }
        }
    }

    pub fn toggle(&self) {
        if let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(ROOT_KEY) {
            root.update(|r: &mut PineDropdownMenuRoot| r.toggle());
        }
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuPortal.poco")]
pub struct PineDropdownMenuPortal {
    /// Mirrored from Root.open so the template's `pp-if` fires the
    /// teleport when Root opens / closes.
    pub open: bool,
}

#[handlers]
impl PineDropdownMenuPortal {
    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(ROOT_KEY) else {
            return;
        };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuContent.poco")]
pub struct PineDropdownMenuContent {
    /// CSS selector or ref name identifying the trigger element.
    /// Required — author-provided on the tag.
    pub anchor: String,
}

#[handlers]
impl PineDropdownMenuContent {
    pub fn on_ready(&self) {
        // Auto-focus the first menuitem once the teleported clone
        // has committed. Items live in the slot which only
        // materialises after Portal flips `pp-if` on, so this is
        // the first point we can see them.
        let Some(scope) = current_scope_id() else { return };
        let Some(menu) = refs::get_on(scope, "menu") else { return };
        init_roving_tabindex(&menu);
        focus::auto_focus_first(&menu);
    }

    pub fn close(&mut self) {
        if let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(ROOT_KEY) {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuItem.poco")]
pub struct PineDropdownMenuItem {
    pub disabled: bool,
}

#[handlers]
impl PineDropdownMenuItem {
    pub fn on_select(&mut self) {
        if self.disabled {
            return;
        }
        // Author's `@click` on the tag fires via native bubble;
        // our responsibility is to dismiss the menu.
        if let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(ROOT_KEY) {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────

/// Set `tabindex=-1` on every menuitem and promote the first
/// non-disabled one to `tabindex=0` — the starting cursor for
/// `pp-roving`. Runs after the slot materialises so the items
/// are in the DOM.
fn init_roving_tabindex(menu: &Element) {
    let Ok(items) = menu.query_selector_all(
        "[role=\"menuitem\"], [role=\"menuitemradio\"], [role=\"menuitemcheckbox\"]",
    ) else {
        return;
    };
    let mut first_enabled: Option<Element> = None;
    for i in 0..items.length() {
        let Some(node) = items.item(i) else { continue };
        let Ok(el) = node.dyn_into::<Element>() else { continue };
        let _ = el.set_attribute("tabindex", "-1");
        let disabled = el.get_attribute("aria-disabled").as_deref() == Some("true")
            || el.has_attribute("disabled");
        if first_enabled.is_none() && !disabled {
            first_enabled = Some(el.clone());
        }
    }
    if let Some(el) = first_enabled {
        let _ = el.set_attribute("tabindex", "0");
    }
}
