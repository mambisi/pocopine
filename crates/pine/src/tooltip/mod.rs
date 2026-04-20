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

use crate::compound;
use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, inject_key, provide, watch_scope_field, ScopeId};
use pocopine_core::scope::Scope;
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Event, EventTarget};

const SLUG: &str = "tooltip";

inject_key!(ROOT: Handle<PineTooltipRoot>);
// Provider → descendants. Defaults + singleton policy live on the
// provider scope; tooltip Roots inside consult this handle in their
// `on_setup` and register with it when they open.
inject_key!(PROVIDER: Handle<PineTooltipProvider>);

// ── Provider ──────────────────────────────────────────────────────

/// Groups a subtree of tooltips under one policy. Optional — a
/// `<pine-tooltip-root>` used outside a Provider keeps its own
/// local delay with no singleton coordination. Inside a Provider:
///
/// - `delay_duration` on the Provider becomes the default for
///   descendants (a Tooltip Root whose own `delay_duration` is
///   left at its default 700 picks up the Provider's value).
/// - At any moment only **one** tooltip inside the Provider is
///   open; when a second opens, the first closes.
///
/// ```html
/// <pine-tooltip-provider delay_duration="400">
///   <pine-tooltip-root>
///     <pine-tooltip-trigger><button>Save</button></pine-tooltip-trigger>
///     <pine-tooltip-portal><pine-tooltip-content>Saves</pine-tooltip-content></pine-tooltip-portal>
///   </pine-tooltip-root>
///   <pine-tooltip-root>
///     <pine-tooltip-trigger><button>Undo</button></pine-tooltip-trigger>
///     <pine-tooltip-portal><pine-tooltip-content>Undoes</pine-tooltip-content></pine-tooltip-portal>
///   </pine-tooltip-root>
/// </pine-tooltip-provider>
/// ```
#[derive(Serialize, Deserialize)]
#[component(template = "PineTooltipProvider.poco", role = "scope")]
pub struct PineTooltipProvider {
    /// Default `delay_duration` inherited by descendants. Authors
    /// can still set `delay_duration` on an individual Root to
    /// override.
    #[prop] pub delay_duration: u32,
}

impl Default for PineTooltipProvider {
    fn default() -> Self {
        Self { delay_duration: 700 }
    }
}

#[handlers]
impl PineTooltipProvider {
    pub fn on_setup(&mut self) {
        provide(&PROVIDER, this::<Self>());
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            CURRENT_OPEN.with(|m| {
                m.borrow_mut().remove(&scope);
            });
        }
    }
}

// Tracks which Tooltip Root is currently open within each Provider.
// `None` entry = Provider present but no tooltip open right now.
thread_local! {
    static CURRENT_OPEN: RefCell<HashMap<ScopeId, Option<ScopeId>>> =
        RefCell::new(HashMap::new());
}

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
#[component(template = "PineTooltipRoot.poco", role = "scope")]
pub struct PineTooltipRoot {
    #[prop] pub open: bool,
    /// Delay (ms) before the tooltip appears on hover. Focus
    /// opens immediately (no delay — matches WAI-ARIA).
    #[prop] pub delay_duration: u32,
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
        provide(&ROOT, this::<Self>());
        // Inherit Provider's default delay if Root is still at its
        // own default (700). An explicit `delay_duration` on the
        // Root tag always wins because static props apply before
        // `on_setup`, so `self.delay_duration` here already holds
        // whatever the author wrote.
        if self.delay_duration == 700 {
            if let Some(prov) = inject(&PROVIDER) {
                self.delay_duration = prov.with(|p| p.delay_duration);
            }
        }
    }

    pub fn on_ready(&self, scope: ScopeId) {
        // Singleton policy — watch our own `open` field. When it
        // flips true, evict whichever tooltip is currently open
        // in this Provider and take the slot. When it flips
        // false, release the slot. Runs only when a Provider
        // ancestor exists; standalone tooltips are unaffected.
        let root_id = scope;
        let Some(prov) = inject::<Handle<PineTooltipProvider>>(&PROVIDER) else {
            return;
        };
        let prov_id = prov.scope_id();
        watch_scope_field::<bool, _>(root_id, "open", move |&is_open, _| {
            if is_open {
                singleton_take_over(root_id, prov_id);
            } else {
                singleton_release(root_id, prov_id);
            }
        });
    }
}

/// Evict whichever tooltip is currently open inside the given
/// Provider (if any, and if it isn't `root_id` itself) and take
/// the slot.
fn singleton_take_over(root_id: ScopeId, prov_id: ScopeId) {
    let prev = CURRENT_OPEN.with(|m| {
        let mut map = m.borrow_mut();
        let slot = map.entry(prov_id).or_insert(None);
        let prev = slot.take();
        *slot = Some(root_id);
        prev
    });
    if let Some(prev_id) = prev {
        if prev_id != root_id {
            if let Some(scope) = Scope::find(prev_id) {
                if let Some(rc) = scope.typed::<PineTooltipRoot>() {
                    Handle::new(rc, prev_id).update(|r: &mut PineTooltipRoot| r.open = false);
                }
            }
        }
    }
}

/// Release the slot when this tooltip closes. Guarded on identity
/// so a rapid "open A → open B" sequence doesn't let A's delayed
/// close clobber B's take-over.
fn singleton_release(root_id: ScopeId, prov_id: ScopeId) {
    CURRENT_OPEN.with(|m| {
        let mut map = m.borrow_mut();
        if let Some(slot) = map.get_mut(&prov_id) {
            if *slot == Some(root_id) {
                *slot = None;
            }
        }
    });
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTooltipTrigger.poco", role = "panel")]
pub struct PineTooltipTrigger {}

#[handlers]
impl PineTooltipTrigger {
    pub fn on_ready(&self, refs: pocopine::Refs, scope: ScopeId) {
        let Some(root) = inject(&ROOT) else { return };
        // Stamp the trigger's slot root so Content's auto-anchor
        // resolves to it. `pp-ref="trigger"` points at the
        // wrapper the template renders around the author's slot.
        if let Some(el) = refs.get("trigger") {
            compound::stamp_trigger(&el, root.scope_id(), SLUG);
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
#[component(template = "PineTooltipPortal.poco", role = "scope")]
pub struct PineTooltipPortal {
    #[mirror(via = ROOT)] pub open: bool,
}

#[handlers]
impl PineTooltipPortal {}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineTooltipContent.poco", role = "panel")]
pub struct PineTooltipContent {
    pub anchor: String,
    #[prop] pub side: String,
    #[prop] pub align: String,
    #[prop] pub side_offset: f64,
}

impl Default for PineTooltipContent {
    fn default() -> Self {
        Self {
            anchor: String::new(),
            side: "top".into(),
            align: "center".into(),
            side_offset: 6.0,
        }
    }
}

#[handlers]
impl PineTooltipContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            self.anchor = compound::trigger_selector(root.scope_id(), SLUG);
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(content) = refs.get("content") else { return };
        if let Ok(floater) = content.dyn_into::<web_sys::HtmlElement>() {
            if let Some(root) = inject::<Handle<PineTooltipRoot>>(&ROOT) {
                compound::install_anchor_to_trigger(
                    &floater,
                    root.scope_id(),
                    SLUG,
                    &self.side,
                    &self.align,
                    self.side_offset,
                    true,
                );
            }
        }
    }
}

