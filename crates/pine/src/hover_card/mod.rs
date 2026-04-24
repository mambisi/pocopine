//! `<pine-hover-card-*>` — hover-opened floating card (compound).
//!
//! Reka-ui / Radix anatomy. Same bones as Tooltip but with two
//! crucial differences:
//!
//! 1. **Close delay.** Mouse leaves the trigger → a close timer
//!    starts. If the mouse enters the Content before it fires,
//!    the timer cancels and the card stays open. Users can
//!    move from trigger to content without losing the card —
//!    tooltips can't do that because they re-fire their own
//!    leave on every tiny mouse gap.
//! 2. **Interactive content.** Content is a full `role="dialog"`
//!    region, not a live-region tooltip. Authors put links,
//!    buttons, avatars inside.
//!
//! Five parts:
//!
//! - **Root** (`pine-hover-card-root`) — `open: bool`,
//!   `open_delay` (default 700), `close_delay` (default 300).
//! - **Trigger** (`pine-hover-card-trigger`) — wraps the author's
//!   element; hover/focus schedule the open/close timers.
//! - **Portal** (`pine-hover-card-portal`) — teleport wrapper.
//! - **Content** (`pine-hover-card-content`) — `role="dialog"`
//!   anchored to the Trigger. Its own hover enter/leave cancels
//!   and re-schedules close, keeping the card alive while the
//!   pointer is over it.
//!
//! ```html
//! <pine-hover-card-root>
//!   <pine-hover-card-trigger>
//!     <a href="/u/ada">@ada</a>
//!   </pine-hover-card-trigger>
//!   <pine-hover-card-portal>
//!     <pine-hover-card-content>
//!       <img src="…" alt="Ada">
//!       <h3>Ada Lovelace</h3>
//!       <p>Mathematician · @ada</p>
//!     </pine-hover-card-content>
//!   </pine-hover-card-portal>
//! </pine-hover-card-root>
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use crate::compound;
use pocopine::prelude::*;
use pocopine::{inject, inject_key, provide, refs, ScopeId};
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Event, EventTarget};

const SLUG: &str = "hover-card";

inject_key!(ROOT: Handle<PineHoverCardRoot>);

// Per-Root runtime: the pending open/close timer, keyed on Root's
// scope id so both Trigger and Content can reach into the same
// slot. Storing on Root (not per-Trigger) lets Content's
// mouse-enter cancel the close timer that Trigger's mouse-leave
// started — the two parts share the timer because they share
// the "is hovering the combined surface?" state.
thread_local! {
    static RUNTIME: RefCell<HashMap<ScopeId, RootRuntime>> =
        RefCell::new(HashMap::new());
}

#[derive(Default)]
struct RootRuntime {
    pending_open: Option<i32>,
    pending_close: Option<i32>,
    /// Listener closures retained for teardown — Trigger + Content
    /// each stash theirs here so `on_unmount` can remove them.
    trigger_enter: Option<Closure<dyn FnMut(Event)>>,
    trigger_leave: Option<Closure<dyn FnMut(Event)>>,
    trigger_focus: Option<Closure<dyn FnMut(Event)>>,
    trigger_blur: Option<Closure<dyn FnMut(Event)>>,
    trigger_el: Option<web_sys::Element>,
    content_enter: Option<Closure<dyn FnMut(Event)>>,
    content_leave: Option<Closure<dyn FnMut(Event)>>,
    content_el: Option<web_sys::Element>,
}

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineHoverCardRoot.poco",
    role = "scope",
    display = "contents"
)]
// RFC 049 — HoverCard's Root holds Trigger + Portal. Same
// topology as Popover / Tooltip.
#[slot(default, only = [PineHoverCardTrigger, PineHoverCardPortal])]
pub struct PineHoverCardRoot {
    #[prop]
    pub open: bool,
    /// Delay (ms) before the card appears on hover / focus.
    #[prop]
    pub open_delay: u32,
    /// Delay (ms) before the card closes after the pointer leaves
    /// both the trigger and the content. Keeps the card alive
    /// while the user moves their mouse across the gap.
    #[prop]
    pub close_delay: u32,
}

impl Default for PineHoverCardRoot {
    fn default() -> Self {
        Self {
            open: false,
            open_delay: 700,
            close_delay: 300,
        }
    }
}

#[handlers]
impl PineHoverCardRoot {
    pub fn on_setup(&mut self) {
        provide(&ROOT, this::<Self>());
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineHoverCardTrigger.poco",
    role = "panel",
    display = "contents"
)]
pub struct PineHoverCardTrigger {}

#[handlers]
impl PineHoverCardTrigger {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = inject(&ROOT) else { return };
        let root_id = root.scope_id();
        // Stamp the trigger element with the Root's scope id so
        // Content's anchor selector resolves to it.
        if let Some(el) = refs::get_on(root_id, "trigger").or_else(|| refs.get("trigger")) {
            compound::stamp_trigger(&el, root_id, SLUG);
            install_trigger_listeners(root_id, el, root);
        }
    }

    pub fn on_unmount(&mut self) {
        if let Some(root) = inject(&ROOT) {
            teardown_trigger(root.scope_id());
        }
    }
}

fn install_trigger_listeners(
    root_id: ScopeId,
    trigger_el: web_sys::Element,
    root: Handle<PineHoverCardRoot>,
) {
    let enter = make_open_scheduler(root_id, root.clone());
    let leave = make_close_scheduler(root_id, root.clone());
    let focus = {
        // Focus opens immediately (no delay) to match keyboard-user
        // expectations — matches Radix's HoverCard behaviour.
        let root = root.clone();
        Closure::wrap(Box::new(move |_ev: Event| {
            cancel_close(root_id);
            root.update(|s| s.open = true);
        }) as Box<dyn FnMut(Event)>)
    };
    let blur = make_close_scheduler(root_id, root);

    let target: &EventTarget = trigger_el.as_ref();
    let _ = target.add_event_listener_with_callback("mouseenter", enter.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("mouseleave", leave.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("focusin", focus.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("focusout", blur.as_ref().unchecked_ref());

    RUNTIME.with(|r| {
        let mut map = r.borrow_mut();
        let entry = map.entry(root_id).or_default();
        entry.trigger_el = Some(trigger_el);
        entry.trigger_enter = Some(enter);
        entry.trigger_leave = Some(leave);
        entry.trigger_focus = Some(focus);
        entry.trigger_blur = Some(blur);
    });
}

/// Build a closure that schedules `open = true` after `open_delay`.
/// Cancels any pending close timer first — the pointer re-entered
/// a tracked surface, so we don't want the card to close under us.
fn make_open_scheduler(
    root_id: ScopeId,
    root: Handle<PineHoverCardRoot>,
) -> Closure<dyn FnMut(Event)> {
    Closure::wrap(Box::new(move |_ev: Event| {
        cancel_close(root_id);
        // Don't restart the open timer if one is already pending —
        // a second mouseenter on the same surface during the delay
        // window should keep the original countdown.
        let has_pending_open = RUNTIME.with(|r| {
            r.borrow()
                .get(&root_id)
                .and_then(|e| e.pending_open)
                .is_some()
        });
        if has_pending_open {
            return;
        }
        let delay = root.with(|r| r.open_delay);
        let root_for_timer = root.clone();
        let timer_cb = Closure::once(Box::new(move || {
            RUNTIME.with(|r| {
                if let Some(e) = r.borrow_mut().get_mut(&root_id) {
                    e.pending_open = None;
                }
            });
            root_for_timer.update(|s| s.open = true);
        }) as Box<dyn FnOnce()>);
        if let Some(w) = web_sys::window() {
            if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                timer_cb.into_js_value().unchecked_ref(),
                delay as i32,
            ) {
                RUNTIME.with(|r| {
                    if let Some(e) = r.borrow_mut().get_mut(&root_id) {
                        e.pending_open = Some(id);
                    }
                });
            }
        }
    }) as Box<dyn FnMut(Event)>)
}

/// Build a closure that schedules `open = false` after `close_delay`.
/// Cancels any pending open first — the pointer left the tracked
/// surface before the card even appeared, so don't surprise the
/// user with a delayed open.
fn make_close_scheduler(
    root_id: ScopeId,
    root: Handle<PineHoverCardRoot>,
) -> Closure<dyn FnMut(Event)> {
    Closure::wrap(Box::new(move |_ev: Event| {
        cancel_open(root_id);
        let delay = root.with(|r| r.close_delay);
        let root_for_timer = root.clone();
        let timer_cb = Closure::once(Box::new(move || {
            RUNTIME.with(|r| {
                if let Some(e) = r.borrow_mut().get_mut(&root_id) {
                    e.pending_close = None;
                }
            });
            root_for_timer.update(|s| s.open = false);
        }) as Box<dyn FnOnce()>);
        if let Some(w) = web_sys::window() {
            if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                timer_cb.into_js_value().unchecked_ref(),
                delay as i32,
            ) {
                RUNTIME.with(|r| {
                    if let Some(e) = r.borrow_mut().get_mut(&root_id) {
                        // Replace any prior pending close — last
                        // leave wins.
                        if let (Some(w), Some(prev)) = (web_sys::window(), e.pending_close.take()) {
                            w.clear_timeout_with_handle(prev);
                        }
                        e.pending_close = Some(id);
                    }
                });
            }
        }
    }) as Box<dyn FnMut(Event)>)
}

fn cancel_open(root_id: ScopeId) {
    if let Some(w) = web_sys::window() {
        RUNTIME.with(|r| {
            if let Some(e) = r.borrow_mut().get_mut(&root_id) {
                if let Some(id) = e.pending_open.take() {
                    w.clear_timeout_with_handle(id);
                }
            }
        });
    }
}

fn cancel_close(root_id: ScopeId) {
    if let Some(w) = web_sys::window() {
        RUNTIME.with(|r| {
            if let Some(e) = r.borrow_mut().get_mut(&root_id) {
                if let Some(id) = e.pending_close.take() {
                    w.clear_timeout_with_handle(id);
                }
            }
        });
    }
}

fn teardown_trigger(root_id: ScopeId) {
    let Some(el) = RUNTIME.with(|r| {
        r.borrow_mut()
            .get_mut(&root_id)
            .and_then(|e| e.trigger_el.take())
    }) else {
        return;
    };
    let target: &EventTarget = el.as_ref();
    RUNTIME.with(|r| {
        let mut map = r.borrow_mut();
        if let Some(e) = map.get_mut(&root_id) {
            if let Some(c) = e.trigger_enter.take() {
                let _ = target
                    .remove_event_listener_with_callback("mouseenter", c.as_ref().unchecked_ref());
            }
            if let Some(c) = e.trigger_leave.take() {
                let _ = target
                    .remove_event_listener_with_callback("mouseleave", c.as_ref().unchecked_ref());
            }
            if let Some(c) = e.trigger_focus.take() {
                let _ = target
                    .remove_event_listener_with_callback("focusin", c.as_ref().unchecked_ref());
            }
            if let Some(c) = e.trigger_blur.take() {
                let _ = target
                    .remove_event_listener_with_callback("focusout", c.as_ref().unchecked_ref());
            }
        }
    });
    cancel_open(root_id);
    cancel_close(root_id);
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineHoverCardPortal.poco",
    role = "scope",
    display = "contents"
)]
// RFC 049 — Portal wraps Content.
#[slot(default, only = [PineHoverCardContent])]
pub struct PineHoverCardPortal {
    #[observe(ROOT)]
    pub open: bool,
}

#[handlers]
impl PineHoverCardPortal {}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineHoverCardContent.poco",
    role = "panel",
    transition = "fade-scale"
)]
pub struct PineHoverCardContent {
    pub anchor: String,
    #[prop]
    pub side: String,
    #[prop]
    pub align: String,
    #[prop]
    pub side_offset: f64,
}

impl Default for PineHoverCardContent {
    fn default() -> Self {
        Self {
            anchor: String::new(),
            side: "bottom".into(),
            align: "center".into(),
            side_offset: 8.0,
        }
    }
}

#[handlers]
impl PineHoverCardContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            self.anchor = compound::trigger_selector(root.scope_id(), SLUG);
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(content) = refs.get("content") else {
            return;
        };
        let Some(root) = inject(&ROOT) else { return };
        let root_id = root.scope_id();

        // Anchor to the Trigger — same programmatic install path
        // as Tooltip / Popover so the side/align/side_offset props
        // flow through.
        if let Ok(floater) = content.clone().dyn_into::<web_sys::HtmlElement>() {
            compound::install_anchor_to_trigger(
                &floater,
                root_id,
                SLUG,
                &self.side,
                &self.align,
                self.side_offset,
                true,
            );
        }

        // Content also tracks hover — cancel close when pointer
        // enters it, schedule close when pointer leaves. This is
        // the thing that lets users move mouse from Trigger across
        // the gap into Content without the card vanishing.
        let enter = make_open_scheduler(root_id, root.clone());
        let leave = make_close_scheduler(root_id, root);

        let target: &EventTarget = content.as_ref();
        let _ =
            target.add_event_listener_with_callback("mouseenter", enter.as_ref().unchecked_ref());
        let _ =
            target.add_event_listener_with_callback("mouseleave", leave.as_ref().unchecked_ref());

        RUNTIME.with(|r| {
            let mut map = r.borrow_mut();
            let entry = map.entry(root_id).or_default();
            entry.content_el = Some(content);
            entry.content_enter = Some(enter);
            entry.content_leave = Some(leave);
        });
    }

    pub fn on_unmount(&mut self) {
        let Some(root) = inject(&ROOT) else { return };
        teardown_content(root.scope_id());
    }
}

fn teardown_content(root_id: ScopeId) {
    let Some(el) = RUNTIME.with(|r| {
        r.borrow_mut()
            .get_mut(&root_id)
            .and_then(|e| e.content_el.take())
    }) else {
        return;
    };
    let target: &EventTarget = el.as_ref();
    RUNTIME.with(|r| {
        let mut map = r.borrow_mut();
        if let Some(e) = map.get_mut(&root_id) {
            if let Some(c) = e.content_enter.take() {
                let _ = target
                    .remove_event_listener_with_callback("mouseenter", c.as_ref().unchecked_ref());
            }
            if let Some(c) = e.content_leave.take() {
                let _ = target
                    .remove_event_listener_with_callback("mouseleave", c.as_ref().unchecked_ref());
            }
        }
    });
}
