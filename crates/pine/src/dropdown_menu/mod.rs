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
use pocopine::{current_scope_id, focus, refs, tick, watch, Scope, ScopeId};
use serde::{Deserialize, Serialize};

use js_sys::Reflect;
use wasm_bindgen::{JsCast, JsValue};
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
    pub fn on_mount(&mut self) {
        let scope = current_scope_id().expect("on_mount within scope");
        tick::next(move || {
            watch(
                move || read_open(scope),
                move |is_open, prev| match (prev, *is_open) {
                    (None, true) | (Some(false), true) => {
                        // Wait one tick so pp-if/pp-teleport commit
                        // the menu into <body> before we grab the
                        // ref and auto-focus its first item.
                        tick::next(move || activate(scope));
                    }
                    (Some(true), false) => deactivate(scope),
                    _ => {}
                },
            );
        });
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            deactivate(scope);
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        emit_open_changed();
    }
}

/// See `PineDialog::emit_open_changed` for rationale. DropdownMenu
/// uses `pp-ref="menu"` on the teleported root, so we walk from
/// that ref back to the host tag.
fn emit_open_changed() {
    let Some(host) = find_host_element() else { return };
    tick::next(move || {
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&JsValue::from_bool(false));
        if let Ok(ev) =
            web_sys::CustomEvent::new_with_event_init_dict("pp:update:model", &init)
        {
            let _ = host.dispatch_event(&ev);
        }
    });
}

fn find_host_element() -> Option<Element> {
    let scope = current_scope_id()?;
    let mut cur: Option<Element> = refs::get_on(scope, "menu");
    let origin_key = JsValue::from_str(
        pocopine_core::directives::teleport::TELEPORT_ORIGIN_KEY,
    );
    while let Some(el) = cur {
        let v = Reflect::get(el.as_ref(), &origin_key).ok()?;
        if !v.is_undefined() && !v.is_null() {
            if let Ok(template) = v.dyn_into::<Element>() {
                return template.parent_element();
            }
        }
        cur = el.parent_element();
    }
    None
}

fn read_open(scope: ScopeId) -> bool {
    let Some(s) = Scope::find(scope) else { return false };
    let proxy = s.into_proxy();
    let v = Reflect::get(&proxy, &JsValue::from_str("open")).unwrap_or(JsValue::FALSE);
    !v.is_falsy()
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
