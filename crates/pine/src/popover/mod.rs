//! `PinePopover` — anchored floating panel.
//!
//! Lighter-weight than `PineDialog`: no focus trap, no scroll
//! lock. Uses `pp-anchor` for positioning (default placement
//! `bottom-start`), dismisses on outside click and Escape,
//! teleports to `<body>` so `overflow: hidden` ancestors don't
//! clip it.
//!
//! ```html
//! <button id="trigger" @click="open = !open">Open</button>
//! <pine-popover :open="open" anchor="#trigger">
//!   <p>Popover content.</p>
//! </pine-popover>
//! ```
//!
//! The `anchor` prop accepts either a CSS selector (`"#trigger"`,
//! `".btn"`) or a `pp-ref` name that exists on the Pine
//! component's own scope.

use std::cell::RefCell;
use std::collections::HashMap;

use pocopine::prelude::*;
use pocopine::{current_scope_id, focus, refs, tick, ScopeId};
use serde::{Deserialize, Serialize};

use js_sys::Reflect;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::Element;

thread_local! {
    static RUNTIME: RefCell<HashMap<ScopeId, PopoverRuntime>> =
        RefCell::new(HashMap::new());
}

#[derive(Default)]
struct PopoverRuntime {
    saved: Option<focus::Saved>,
}

#[derive(Serialize, Deserialize)]
#[component(template = "PinePopover.poco")]
pub struct PinePopover {
    /// Open / closed state. Bind with `pp-model="open"`.
    pub open: bool,
    /// CSS selector or scope-local `pp-ref` name identifying the
    /// trigger element to anchor against.
    pub anchor: String,
    /// Close when the user clicks outside the popover.
    pub dismiss_on_outside: bool,
    /// Close on Escape keypress while focus is inside.
    pub dismiss_on_escape: bool,
}

impl Default for PinePopover {
    fn default() -> Self {
        Self {
            open: false,
            anchor: String::new(),
            dismiss_on_outside: true,
            dismiss_on_escape: true,
        }
    }
}

#[handlers]
impl PinePopover {
    #[watch(open)]
    fn on_open_change(&mut self, is_open: bool, prev: Option<bool>) {
        match (prev, is_open) {
            (None, true) | (Some(false), true) => {
                if let Some(scope) = current_scope_id() {
                    activate(scope);
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

    pub fn on_outside(&mut self) {
        if self.dismiss_on_outside {
            self.open = false;
            emit_open_changed(false);
        }
    }

    pub fn on_escape(&mut self) {
        if self.dismiss_on_escape {
            self.open = false;
            emit_open_changed(false);
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        emit_open_changed(false);
    }
}

/// Fire `pp:update:model` through the `<pine-popover>` host tag so
/// `pp-model:open` on the parent picks up an internally-driven
/// close. See `PineDialog::emit_open_changed` for the rationale
/// behind deferring + walking back through the teleport origin.
fn emit_open_changed(open: bool) {
    let Some(host) = find_host_element() else { return };
    tick::next(move || {
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&JsValue::from_bool(open));
        if let Ok(ev) =
            web_sys::CustomEvent::new_with_event_init_dict("pp:update:model", &init)
        {
            let _ = host.dispatch_event(&ev);
        }
    });
}

fn find_host_element() -> Option<Element> {
    let scope = current_scope_id()?;
    let mut cur: Option<Element> = refs::get_on(scope, "content");
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


fn activate(scope: ScopeId) {
    let saved = focus::save();
    RUNTIME.with(|r| {
        r.borrow_mut()
            .insert(scope, PopoverRuntime { saved: Some(saved) });
    });
}

fn deactivate(scope: ScopeId) {
    let Some(mut rt) = RUNTIME.with(|r| r.borrow_mut().remove(&scope)) else {
        return;
    };
    // Restore focus to whatever had it before, unless focus already
    // moved outside our popover (e.g. user clicked a different
    // control to dismiss).
    if let Some(saved) = rt.saved.take() {
        focus::restore(saved);
    }
}
