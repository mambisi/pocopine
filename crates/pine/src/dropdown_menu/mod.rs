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
//!     <pine-dropdown-menu-content>
//!       <pine-dropdown-menu-group>
//!         <pine-dropdown-menu-label>Actions</pine-dropdown-menu-label>
//!         <pine-dropdown-menu-item @click="bump">Bump</pine-dropdown-menu-item>
//!         <pine-dropdown-menu-item disabled>Export</pine-dropdown-menu-item>
//!       </pine-dropdown-menu-group>
//!       <pine-dropdown-menu-separator></pine-dropdown-menu-separator>
//!       <pine-dropdown-menu-item @click="reset">Reset</pine-dropdown-menu-item>
//!     </pine-dropdown-menu-content>
//!   </pine-dropdown-menu-portal>
//! </pine-dropdown-menu-root>
//! ```
//!
//! Content auto-anchors to its Trigger via RFC-027 inject + the
//! `on_setup` lifecycle — no selector required.

use pocopine::prelude::*;
use pocopine::{current_scope_id, focus, inject, provide, refs, watch_scope_field};
use pocopine_core::scope::current_el;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{CustomEvent, CustomEventInit, Element};

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
        // Stamp the trigger's button with its root scope id. Every
        // Pine dropdown on the page gets a unique value so multiple
        // menus don't collide on the shared selector. Content mirrors
        // the same id into its own `anchor` field in `on_setup`.
        if let Some(scope) = current_scope_id() {
            if let Some(btn) = refs::get_on(scope, "trigger") {
                let _ = btn.set_attribute(
                    "data-pine-dm-trigger",
                    &format!("{}", root_scope.0),
                );
            }
        }
    }

    pub fn toggle(&mut self) {
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
    /// Computed in `on_setup` from the injected root scope id —
    /// a per-instance selector targeting this root's Trigger
    /// button. Authors never write it; Content resolves it
    /// automatically via context.
    pub anchor: String,
}

#[handlers]
impl PineDropdownMenuContent {
    /// Runs before the template walks, so pp-anchor sees the
    /// computed selector on first bind. Uses the root's scope id
    /// so every menu instance on the page has its own anchor —
    /// matching the unique `data-pine-dm-trigger="N"` Trigger
    /// stamped in its `on_ready`.
    pub fn on_setup(&mut self) {
        if let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(ROOT_KEY) {
            self.anchor = format!(
                "[data-pine-dm-trigger=\"{}\"]",
                root.scope_id().0
            );
        }
    }

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
    /// Fires on the inner `<li>`'s click. Two-step dismiss:
    ///
    /// 1. Dispatch a cancelable `pp:select` CustomEvent
    ///    synchronously on the element. Authors can listen with
    ///    `@pp:select.prevent="…"` on the tag to veto the
    ///    auto-close — the menu stays open while their action
    ///    still runs via the native click bubble.
    /// 2. If no listener called `preventDefault()`, close the
    ///    menu via the injected root.
    ///
    /// Matches reka-ui's `DropdownMenuItem` select-emits-with-
    /// preventable semantic.
    pub fn on_select(&mut self) {
        if self.disabled {
            return;
        }
        let prevented = dispatch_pp_select();
        if prevented {
            return;
        }
        if let Some(root) = inject::<Handle<PineDropdownMenuRoot>>(ROOT_KEY) {
            root.update(|r: &mut PineDropdownMenuRoot| r.close());
        }
    }
}

/// Dispatch a cancelable `pp:select` event from the current
/// directive element. Returns `true` when a listener called
/// `preventDefault` — caller should skip its default action.
fn dispatch_pp_select() -> bool {
    let Some(el) = current_el() else { return false };
    let init = CustomEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    let Ok(ev) = CustomEvent::new_with_event_init_dict("pp:select", &init) else {
        return false;
    };
    let _ = el.dispatch_event(&ev);
    ev.default_prevented()
}

// ── Separator ─────────────────────────────────────────────────────

/// Visual divider between groups of menu items. No state, no
/// focus, no interaction — pure `role="separator"` +
/// `aria-orientation`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuSeparator.poco")]
pub struct PineDropdownMenuSeparator {}

#[handlers]
impl PineDropdownMenuSeparator {}

// ── Group ─────────────────────────────────────────────────────────

/// ARIA group wrapper. Its `on_setup` mints a `label_id` from its
/// own scope id (unique per instance) and provides it to any
/// nested Label so their ids match up for `aria-labelledby`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuGroup.poco")]
pub struct PineDropdownMenuGroup {
    /// Computed — id of the Label inside this group, for
    /// `aria-labelledby` on the group's root. Populated in
    /// `on_setup`; authors never set it.
    pub label_id: String,
}

/// Provide/inject key for a Group's label id. Only meaningful
/// inside a Group's subtree.
const GROUP_LABEL_KEY: &str = "pine-dm-group-label-id";

#[handlers]
impl PineDropdownMenuGroup {
    pub fn on_setup(&mut self) {
        let Some(scope) = current_scope_id() else { return };
        let label_id = format!("pine-dm-group-label-{}", scope.0);
        self.label_id = label_id.clone();
        provide(GROUP_LABEL_KEY, label_id);
    }
}

// ── Label ─────────────────────────────────────────────────────────

/// Labelled heading for a Group. Injects the group's label id and
/// renders it as the element's `id` so the enclosing Group's
/// `aria-labelledby` resolves. Does not render a `role` — it's
/// styling-only (matches reka-ui / Radix).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenuLabel.poco")]
pub struct PineDropdownMenuLabel {
    pub label_id: String,
}

#[handlers]
impl PineDropdownMenuLabel {
    pub fn on_setup(&mut self) {
        if let Some(id) = inject::<String>(GROUP_LABEL_KEY) {
            self.label_id = id;
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
