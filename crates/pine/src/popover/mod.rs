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
use pocopine::{current_scope_id, focus, tick, watch, Scope, ScopeId};
use serde::{Deserialize, Serialize};

use js_sys::Reflect;
use wasm_bindgen::JsValue;

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
    pub fn on_mount(&mut self) {
        let scope = current_scope_id().expect("on_mount within scope");
        tick::next(move || {
            watch(
                move || read_open(scope),
                move |is_open, prev| match (prev, *is_open) {
                    (None, true) | (Some(false), true) => activate(scope),
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

    pub fn on_outside(&mut self) {
        if self.dismiss_on_outside {
            self.open = false;
        }
    }

    pub fn on_escape(&mut self) {
        if self.dismiss_on_escape {
            self.open = false;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
    }
}

fn read_open(scope: ScopeId) -> bool {
    let Some(s) = Scope::find(scope) else { return false };
    let proxy = s.into_proxy();
    let v = Reflect::get(&proxy, &JsValue::from_str("open")).unwrap_or(JsValue::FALSE);
    !v.is_falsy()
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
