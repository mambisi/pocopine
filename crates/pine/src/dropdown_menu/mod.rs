//! `PineDropdownMenu` — menu overlay.
//!
//! Composes Popover + `pp-roving` + `role="menu"`. On open,
//! auto-focuses the first menu item; arrow keys cycle; Escape
//! and click-outside close.
//!
//! ```html
//! <button pp-ref="menu-trigger" @click="open = !open">More</button>
//! <pine-dropdown-menu :open="open" anchor="#menu-trigger">
//!   <li role="menuitem" tabindex="-1" @click="copy">Copy</li>
//!   <li role="menuitem" tabindex="-1" @click="paste">Paste</li>
//!   <li role="menuitem" tabindex="-1" aria-disabled="true">Delete</li>
//! </pine-dropdown-menu>
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use pocopine::prelude::*;
use pocopine::{current_scope_id, focus, refs, tick, ScopeId};
use serde::{Deserialize, Serialize};

use wasm_bindgen::JsCast;
use web_sys::Element;

thread_local! {
    static RUNTIME: RefCell<HashMap<ScopeId, MenuRuntime>> =
        RefCell::new(HashMap::new());
}

#[derive(Default)]
struct MenuRuntime {
    saved: Option<focus::Saved>,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineDropdownMenu.poco")]
pub struct PineDropdownMenu {
    pub open: bool,
    pub anchor: String,
}

#[handlers]
impl PineDropdownMenu {
    #[watch(open)]
    fn on_open_change(&mut self, is_open: bool, prev: Option<bool>) {
        match (prev, is_open) {
            (None, true) | (Some(false), true) => {
                // Wait one tick so pp-if/pp-teleport commit the
                // menu into <body> before we auto-focus its first
                // item via `refs::get_on("menu")`.
                if let Some(scope) = current_scope_id() {
                    tick::next(move || activate(scope));
                }
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

    pub fn close(&mut self) {
        self.open = false;
        emit_from_host("pp:update:model", false);
    }
}

fn activate(scope: ScopeId) {
    let saved = focus::save();
    if let Some(menu) = refs::get_on(scope, "menu") {
        // pp-roving installs on the <ul> at bind time, but at
        // bind time the <slot> hasn't been materialised yet — so
        // the items aren't in the DOM and tabindex initialisation
        // runs on an empty list. Set up tabindex ourselves now
        // that the slot content is live.
        init_roving_tabindex(&menu);
        focus::auto_focus_first(&menu);
    }
    RUNTIME.with(|r| {
        r.borrow_mut()
            .insert(scope, MenuRuntime { saved: Some(saved) });
    });
}

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

fn deactivate(scope: ScopeId) {
    let Some(mut rt) = RUNTIME.with(|r| r.borrow_mut().remove(&scope)) else {
        return;
    };
    if let Some(saved) = rt.saved.take() {
        focus::restore(saved);
    }
}
