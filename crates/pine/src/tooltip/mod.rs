//! `<pine-tooltip-*>` — hover / focus tooltip (compound).
//!
//! Reka-ui anatomy, minus Provider + Arrow for v0:
//!
//! - **Root** (`pine-tooltip-root`) — `open: bool`,
//!   `delay_duration` (default 700 ms). Provides Handle.
//! - **Trigger** (`pine-tooltip-trigger`) — wraps the author's
//!   target element. `on_ready` installs `mouseenter` /
//!   `mouseleave` / `focus` / `blur` listeners that drive
//!   Root.open with the Root-configured delay.
//! - **Portal** (`pine-tooltip-portal`) — teleport wrapper.
//! - **Content** (`pine-tooltip-content`) — `role="tooltip"`,
//!   auto-anchors to Trigger (same stamp scheme as DropdownMenu
//!   / Popover). No focus trap — tooltips never steal focus.
//!
//! ```html
//! <pine-tooltip-root>
//!   <pine-tooltip-trigger>
//!     <button>Save</button>
//!   </pine-tooltip-trigger>
//!   <pine-tooltip-portal>
//!     <pine-tooltip-content>Saves your work.</pine-tooltip-content>
//!   </pine-tooltip-portal>
//! </pine-tooltip-root>
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, provide, refs, watch_scope_field, ScopeId};
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Event, EventTarget};

const ROOT_KEY: &str = "pine-tooltip-root";

thread_local! {
    /// Per-Trigger-scope runtime: holds the listener closures +
    /// pending show timer. Keyed on the Trigger's scope id so
    /// teardown (watch callback when Trigger unmounts, or
    /// Root.open goes false-and-stays-false) finds its entry.
    static RUNTIME: RefCell<HashMap<ScopeId, TriggerRuntime>> =
        RefCell::new(HashMap::new());
}

#[allow(dead_code)]
struct TriggerRuntime {
    trigger_el: Option<web_sys::Element>,
    enter: Option<Closure<dyn FnMut(Event)>>,
    leave: Option<Closure<dyn FnMut(Event)>>,
    focus: Option<Closure<dyn FnMut(Event)>>,
    blur: Option<Closure<dyn FnMut(Event)>>,
    pending_timer: Option<i32>,
}

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineTooltipRoot.poco")]
pub struct PineTooltipRoot {
    pub open: bool,
    /// Delay (ms) before the tooltip appears on hover. Focus
    /// opens immediately (no delay — matches WAI-ARIA).
    pub delay_duration: u32,
}

impl Default for PineTooltipRoot {
    fn default() -> Self {
        Self {
            open: false,
            delay_duration: 700,
        }
    }
}

#[handlers]
impl PineTooltipRoot {
    pub fn on_setup(&mut self) {
        provide(ROOT_KEY, this::<Self>());
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTooltipTrigger.poco")]
pub struct PineTooltipTrigger {}

#[handlers]
impl PineTooltipTrigger {
    pub fn on_ready(&self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(root) = inject::<Handle<PineTooltipRoot>>(ROOT_KEY) else { return };
        // Stamp the trigger's slot root so Content's auto-anchor
        // resolves to it. `pp-ref="trigger"` points at the
        // wrapper the template renders around the author's slot.
        if let Some(el) = refs::get_on(scope, "trigger") {
            let _ = el.set_attribute(
                "data-pine-tooltip-trigger",
                &format!("{}", root.scope_id().0),
            );
            install_trigger_listeners(scope, el, root);
        }
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            teardown(scope);
        }
    }
}

fn install_trigger_listeners(
    scope: ScopeId,
    trigger_el: web_sys::Element,
    root: Handle<PineTooltipRoot>,
) {
    let enter = {
        let root = root.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            let delay = root.with(|r| r.delay_duration);
            let root_for_timer = root.clone();
            let timer_cb = Closure::once(Box::new(move || {
                root_for_timer.update(|s| s.open = true);
            }) as Box<dyn FnOnce()>);
            let js = timer_cb.into_js_value();
            if let Some(w) = web_sys::window() {
                if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    js.unchecked_ref(),
                    delay as i32,
                ) {
                    RUNTIME.with(|r| {
                        if let Some(rt) = r.borrow_mut().get_mut(&scope) {
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
        let root = root.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            cancel_pending_and_close(scope, &root);
        }) as Box<dyn FnMut(Event)>)
    };
    let focus = {
        let root = root.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            root.update(|s| s.open = true);
        }) as Box<dyn FnMut(Event)>)
    };
    let blur = {
        let root = root.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            cancel_pending_and_close(scope, &root);
        }) as Box<dyn FnMut(Event)>)
    };

    let target: &EventTarget = trigger_el.as_ref();
    let _ = target.add_event_listener_with_callback("mouseenter", enter.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("mouseleave", leave.as_ref().unchecked_ref());
    // `focusin` + `focusout` bubble, so they fire for focus on a
    // descendant (e.g. the user's <button> slotted into Trigger's
    // `<span>` wrapper). Plain `focus` / `blur` don't bubble.
    let _ = target.add_event_listener_with_callback("focusin", focus.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("focusout", blur.as_ref().unchecked_ref());

    RUNTIME.with(|r| {
        r.borrow_mut().insert(
            scope,
            TriggerRuntime {
                trigger_el: Some(trigger_el),
                enter: Some(enter),
                leave: Some(leave),
                focus: Some(focus),
                blur: Some(blur),
                pending_timer: None,
            },
        );
    });
}

fn cancel_pending_and_close(scope: ScopeId, root: &Handle<PineTooltipRoot>) {
    if let Some(w) = web_sys::window() {
        RUNTIME.with(|r| {
            if let Some(rt) = r.borrow_mut().get_mut(&scope) {
                if let Some(id) = rt.pending_timer.take() {
                    w.clear_timeout_with_handle(id);
                }
            }
        });
    }
    root.update(|s| s.open = false);
}

fn teardown(scope: ScopeId) {
    let Some(rt) = RUNTIME.with(|r| r.borrow_mut().remove(&scope)) else {
        return;
    };
    let Some(trigger_el) = rt.trigger_el.as_ref() else { return };
    let target: &EventTarget = trigger_el.as_ref();
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
            "focusin",
            c.as_ref().unchecked_ref(),
        );
    }
    if let Some(c) = rt.blur.as_ref() {
        let _ = target.remove_event_listener_with_callback(
            "focusout",
            c.as_ref().unchecked_ref(),
        );
    }
    if let Some(w) = web_sys::window() {
        if let Some(id) = rt.pending_timer {
            w.clear_timeout_with_handle(id);
        }
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTooltipPortal.poco")]
pub struct PineTooltipPortal {
    pub open: bool,
}

#[handlers]
impl PineTooltipPortal {
    pub fn on_ready(&self) {
        let Some(root) = inject::<Handle<PineTooltipRoot>>(ROOT_KEY) else { return };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(root.scope_id(), "open", move |&is_open, _| {
            me.update(|s| s.open = is_open);
        });
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTooltipContent.poco")]
pub struct PineTooltipContent {
    pub anchor: String,
}

#[handlers]
impl PineTooltipContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject::<Handle<PineTooltipRoot>>(ROOT_KEY) {
            self.anchor = format!(
                "[data-pine-tooltip-trigger=\"{}\"]",
                root.scope_id().0
            );
        }
    }
}

