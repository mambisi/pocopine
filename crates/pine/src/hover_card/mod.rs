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

use std::cell::Cell;
use std::rc::Rc;

use crate::compound;
use pocopine::events::{self, ev};
use pocopine::prelude::*;
use pocopine::{create_context, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

const SLUG: &str = "hover-card";

create_context!(ROOT: Handle<PineHoverCardRoot>);

// Shared open/close timer cells — Root provides one of these on
// setup; Trigger and Content both `inject()` it so their listeners
// can read and cancel the same pair of pending timers.
//
// Storing on Root (rather than per-Trigger) lets Content's
// mouseenter cancel the close timer Trigger's mouseleave started —
// the two parts share the timer because they share the "is
// hovering the combined surface?" state.
#[derive(Default)]
struct HoverState {
    pending_open: Cell<Option<i32>>,
    pending_close: Cell<Option<i32>>,
}

create_context!(STATE: Rc<HoverState>);

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
        ROOT.provide(this::<Self>());
        // Mint the shared timer cells — Trigger and Content inject
        // them to coordinate the open/close debounce across the
        // combined hover surface.
        STATE.provide(Rc::new(HoverState::default()));
    }

    pub fn on_unmount(&mut self) {
        // Listeners auto-detach with their own scopes; only the
        // outstanding setTimeout handles need clearing.
        if let Some(state) = STATE.inject() {
            cancel_open(&state);
            cancel_close(&state);
        }
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
        let Some(root) = ROOT.inject() else { return };
        let Some(state) = STATE.inject() else { return };
        let root_id = root.scope_id();
        // Stamp the trigger element with the Root's scope id so
        // Content's anchor selector resolves to it.
        if let Some(el) = refs::get_on(root_id, "trigger").or_else(|| refs.get("trigger")) {
            compound::stamp_trigger(&el, root_id, SLUG);
            install_trigger_listeners(el, root, state);
        }
    }
}

fn install_trigger_listeners(
    trigger_el: web_sys::Element,
    root: Handle<PineHoverCardRoot>,
    state: Rc<HoverState>,
) {
    let open = make_open_scheduler(root.clone(), state.clone());
    let close = make_close_scheduler(root.clone(), state.clone());

    let open_for_enter = open.clone();
    events::on_scoped(&trigger_el, ev::mouseenter, move |_e| open_for_enter());
    let close_for_leave = close.clone();
    events::on_scoped(&trigger_el, ev::mouseleave, move |_e| close_for_leave());

    // Focus opens immediately (no delay) to match keyboard-user
    // expectations — matches Radix's HoverCard behaviour.
    let focus_state = state.clone();
    let focus_root = root;
    events::on_scoped(&trigger_el, ev::focusin, move |_e| {
        cancel_close(&focus_state);
        focus_root.update(|s| s.open = true);
    });
    events::on_scoped(&trigger_el, ev::focusout, move |_e| close());
}

/// Build a callable that schedules `open = true` after `open_delay`.
/// Cancels any pending close timer first — the pointer re-entered
/// a tracked surface, so we don't want the card to close under us.
/// Wrapped in `Rc` so the same scheduler can be cloned into multiple
/// listener closures without rebuilding the timer-bookkeeping state.
fn make_open_scheduler(root: Handle<PineHoverCardRoot>, state: Rc<HoverState>) -> Rc<dyn Fn()> {
    Rc::new(move || {
        cancel_close(&state);
        // Don't restart the open timer if one is already pending —
        // a second mouseenter on the same surface during the delay
        // window should keep the original countdown.
        if state.pending_open.get().is_some() {
            return;
        }
        let delay = root.with(|r| r.open_delay);
        let root_for_timer = root.clone();
        let state_for_timer = state.clone();
        let timer_cb = Closure::once(Box::new(move || {
            state_for_timer.pending_open.set(None);
            root_for_timer.update(|s| s.open = true);
        }) as Box<dyn FnOnce()>);
        if let Some(w) = web_sys::window() {
            if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                timer_cb.into_js_value().unchecked_ref(),
                delay as i32,
            ) {
                state.pending_open.set(Some(id));
            }
        }
    })
}

/// Build a closure that schedules `open = false` after `close_delay`.
/// Cancels any pending open first — the pointer left the tracked
/// surface before the card even appeared, so don't surprise the
/// user with a delayed open.
fn make_close_scheduler(root: Handle<PineHoverCardRoot>, state: Rc<HoverState>) -> Rc<dyn Fn()> {
    Rc::new(move || {
        cancel_open(&state);
        let delay = root.with(|r| r.close_delay);
        let root_for_timer = root.clone();
        let state_for_timer = state.clone();
        let timer_cb = Closure::once(Box::new(move || {
            state_for_timer.pending_close.set(None);
            root_for_timer.update(|s| s.open = false);
        }) as Box<dyn FnOnce()>);
        if let Some(w) = web_sys::window() {
            if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                timer_cb.into_js_value().unchecked_ref(),
                delay as i32,
            ) {
                // Replace any prior pending close — last leave wins.
                if let Some(prev) = state.pending_close.replace(Some(id)) {
                    w.clear_timeout_with_handle(prev);
                }
            }
        }
    })
}

fn cancel_open(state: &HoverState) {
    if let Some(id) = state.pending_open.take() {
        if let Some(w) = web_sys::window() {
            w.clear_timeout_with_handle(id);
        }
    }
}

fn cancel_close(state: &HoverState) {
    if let Some(id) = state.pending_close.take() {
        if let Some(w) = web_sys::window() {
            w.clear_timeout_with_handle(id);
        }
    }
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
        if let Some(root) = ROOT.inject() {
            self.anchor = compound::trigger_selector(root.scope_id(), SLUG);
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(content) = refs.get("content") else {
            return;
        };
        let Some(root) = ROOT.inject() else { return };
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
        let Some(state) = STATE.inject() else { return };
        let open = make_open_scheduler(root.clone(), state.clone());
        let close = make_close_scheduler(root, state);

        events::on_scoped(&content, ev::mouseenter, move |_e| open());
        events::on_scoped(&content, ev::mouseleave, move |_e| close());
        let _ = root_id; // anchor selector already wired above
    }
}
