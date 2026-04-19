//! `PineTooltip` — hover / focus tooltip, anchored to a trigger.
//!
//! The component imperatively attaches `mouseenter` / `mouseleave`
//! / `focus` / `blur` listeners to the trigger element (resolved
//! from the `trigger` CSS selector prop). A delay timer gates
//! show; leave / blur cancels pending show and immediately hides.
//!
//! Tooltips never steal focus — no `auto_focus_first`, no trap,
//! no scroll lock.
//!
//! ```html
//! <button id="save-btn">Save</button>
//! <pine-tooltip trigger="#save-btn">Saves your work.</pine-tooltip>
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use js_sys::Reflect;
use pocopine::prelude::*;
use pocopine::{current_scope_id, tick, watch, Handle, Scope, ScopeId};
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::{Event, EventTarget};

thread_local! {
    static RUNTIME: RefCell<HashMap<ScopeId, TooltipRuntime>> =
        RefCell::new(HashMap::new());
}

#[allow(dead_code)]
struct TooltipRuntime {
    // Hold the JS closure objects so their function pointers stay
    // valid; pine's own Drop (via on_unmount) disconnects them.
    trigger: Option<web_sys::Element>,
    enter: Option<Closure<dyn FnMut(Event)>>,
    leave: Option<Closure<dyn FnMut(Event)>>,
    focus: Option<Closure<dyn FnMut(Event)>>,
    blur: Option<Closure<dyn FnMut(Event)>>,
    pending_timer: Option<i32>,
}

#[derive(Serialize, Deserialize)]
#[component(template = "PineTooltip.poco")]
pub struct PineTooltip {
    /// Internal — bound by the component. Not useful as an author
    /// prop.
    pub open: bool,
    /// CSS selector for the trigger element.
    pub trigger: String,
    /// Delay (ms) before the tooltip appears on hover / focus.
    /// Leave / blur hides instantly regardless.
    pub delay: u32,
}

impl Default for PineTooltip {
    fn default() -> Self {
        Self {
            open: false,
            trigger: String::new(),
            delay: 700,
        }
    }
}

#[handlers]
impl PineTooltip {
    pub fn on_mount(&mut self) {
        let scope = current_scope_id().expect("on_mount within scope");
        // Install listeners on the trigger + a watch for open
        // transitions, deferred so the trigger is reachable and
        // on_mount's `&mut self` borrow is released.
        tick::next(move || {
            install_trigger_listeners(scope);
            watch(
                move || read_open(scope),
                move |_is_open, _| {
                    // No side-effects beyond the template's
                    // pp-if; the listeners manipulate state
                    // directly. Kept for symmetry / future
                    // animation hooks.
                },
            );
        });
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            teardown(scope);
        }
    }
}

fn read_open(scope: ScopeId) -> bool {
    let Some(s) = Scope::find(scope) else { return false };
    let proxy = s.into_proxy();
    let v = Reflect::get(&proxy, &JsValue::from_str("open")).unwrap_or(JsValue::FALSE);
    !v.is_falsy()
}

fn install_trigger_listeners(scope: ScopeId) {
    // Resolve the trigger from the selector prop.
    let trigger = match trigger_element(scope) {
        Some(el) => el,
        None => return,
    };
    let handle: Handle<PineTooltip> =
        match Scope::find(scope).and_then(|s| s.typed::<PineTooltip>()) {
            Some(rc) => Handle::new(rc, scope),
            None => return,
        };

    let enter = {
        let handle = handle.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            let delay = handle.with(|s| s.delay);
            let handle2 = handle.clone();
            let timer_cb = Closure::once(Box::new(move || {
                handle2.update(|s| s.open = true);
            }) as Box<dyn FnOnce()>);
            let js = timer_cb.into_js_value();
            if let Some(w) = web_sys::window() {
                if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    js.unchecked_ref(),
                    delay as i32,
                ) {
                    RUNTIME.with(|r| {
                        if let Some(rt) = r.borrow_mut().get_mut(&handle.scope_id()) {
                            if let Some(prev) = rt.pending_timer.take() {
                                w.clear_timeout_with_handle(prev);
                            }
                            rt.pending_timer = Some(id);
                        }
                    });
                }
            }
        }) as Box<dyn FnMut(Event)>)
    };

    let leave = {
        let handle = handle.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            cancel_pending_and_close(&handle);
        }) as Box<dyn FnMut(Event)>)
    };
    let focus = {
        let handle = handle.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            // Focus triggers tooltip immediately (no delay) — matches
            // WAI-ARIA guidance that keyboard users shouldn't wait.
            handle.update(|s| s.open = true);
        }) as Box<dyn FnMut(Event)>)
    };
    let blur = {
        let handle = handle.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            cancel_pending_and_close(&handle);
        }) as Box<dyn FnMut(Event)>)
    };

    let target: &EventTarget = trigger.as_ref();
    let _ = target.add_event_listener_with_callback("mouseenter", enter.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("mouseleave", leave.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("focus", focus.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref());

    RUNTIME.with(|r| {
        r.borrow_mut().insert(
            scope,
            TooltipRuntime {
                trigger: Some(trigger),
                enter: Some(enter),
                leave: Some(leave),
                focus: Some(focus),
                blur: Some(blur),
                pending_timer: None,
            },
        );
    });
}

fn cancel_pending_and_close(handle: &Handle<PineTooltip>) {
    if let Some(w) = web_sys::window() {
        RUNTIME.with(|r| {
            if let Some(rt) = r.borrow_mut().get_mut(&handle.scope_id()) {
                if let Some(id) = rt.pending_timer.take() {
                    w.clear_timeout_with_handle(id);
                }
            }
        });
    }
    handle.update(|s| s.open = false);
}

fn trigger_element(scope: ScopeId) -> Option<web_sys::Element> {
    let s = Scope::find(scope)?;
    let proxy = s.into_proxy();
    let v = Reflect::get(&proxy, &JsValue::from_str("trigger")).ok()?;
    let sel = v.as_string()?;
    let sel = sel.trim();
    if sel.is_empty() {
        return None;
    }
    let doc = web_sys::window()?.document()?;
    doc.query_selector(sel).ok().flatten()
}

fn teardown(scope: ScopeId) {
    let Some(rt) = RUNTIME.with(|r| r.borrow_mut().remove(&scope)) else {
        return;
    };
    let Some(trigger) = rt.trigger.as_ref() else { return };
    let target: &EventTarget = trigger.as_ref();
    if let Some(c) = rt.enter.as_ref() {
        let _ = target.remove_event_listener_with_callback(
            "mouseenter",
            c.as_ref().unchecked_ref(),
        );
    }
    if let Some(c) = rt.leave.as_ref() {
        let _ = target.remove_event_listener_with_callback(
            "mouseleave",
            c.as_ref().unchecked_ref(),
        );
    }
    if let Some(c) = rt.focus.as_ref() {
        let _ = target.remove_event_listener_with_callback(
            "focus",
            c.as_ref().unchecked_ref(),
        );
    }
    if let Some(c) = rt.blur.as_ref() {
        let _ = target.remove_event_listener_with_callback(
            "blur",
            c.as_ref().unchecked_ref(),
        );
    }
    if let Some(w) = web_sys::window() {
        if let Some(id) = rt.pending_timer {
            w.clear_timeout_with_handle(id);
        }
    }
}
