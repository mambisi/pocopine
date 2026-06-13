//! Runtime → devtools hook registry.
//!
//! Thin tap points the reactive core, scope mount, and router call
//! into so devtools panels can observe what's happening. The pattern:
//!
//!   - This module exposes `fire_*` functions under
//!     `#[cfg(feature = "devtools")]`. When the feature is off, the
//!     module is gone and every callsite's `#[cfg]` guard removes
//!     the call too — zero cost.
//!   - When the feature is on but `devtools::install` hasn't run,
//!     each hook is `None`; firing costs one `Option` check + one
//!     RefCell borrow.
//!   - When the feature is on AND `install` ran, the registered
//!     handlers run (timeline push, last-changed write, queue
//!     snapshot, etc).
//!
//! `Option<Rc<dyn Fn…>>` per hook — not `Vec<…>`. Devtools is the
//! sole consumer today; promote to `Vec` when a second subscriber
//! (e.g. a standalone profiler) appears. `Rc` lets the fire path
//! clone-out-of-RefCell before calling, so a handler that re-enters
//! the runtime won't trip a borrow-across-callback.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use js_sys::Array;

use crate::reactive::{EffectId, ScopeId, SignalId};

type EffectRunHook = Rc<dyn Fn(EffectId, Option<ScopeId>, Duration)>;
type HandlerInvokeHook = Rc<dyn Fn(ScopeId, &str, &Array, Duration)>;
type QueueChangeHook = Rc<dyn Fn(&[EffectId])>;
type SignalTriggerHook = Rc<dyn Fn(SignalId)>;
type RouteChangeHook = Rc<dyn Fn(&str, &HashMap<String, String>)>;

#[derive(Default)]
pub(crate) struct Hooks {
    pub on_effect_run: Option<EffectRunHook>,
    pub on_handler_invoke: Option<HandlerInvokeHook>,
    pub on_queue_change: Option<QueueChangeHook>,
    pub on_signal_trigger: Option<SignalTriggerHook>,
    pub on_route_change: Option<RouteChangeHook>,
}

thread_local! {
    pub(crate) static HOOKS: RefCell<Hooks> = RefCell::new(Hooks::default());
}

// ── subscription API ─────────────────────────────────────────────
//
// Called by `devtools::install` to wire the default devtools
// collectors (timeline ring, signal-last-changed table, queue
// snapshot, route log). Users / tests can stack more via the same
// setters — the last-registered handler wins, matching the
// single-slot design. If this changes to `Vec`, update the setters
// to push instead of replace.

pub fn set_on_effect_run(f: EffectRunHook) {
    HOOKS.with(|h| h.borrow_mut().on_effect_run = Some(f));
}

pub fn set_on_handler_invoke(f: HandlerInvokeHook) {
    HOOKS.with(|h| h.borrow_mut().on_handler_invoke = Some(f));
}

pub fn set_on_queue_change(f: QueueChangeHook) {
    HOOKS.with(|h| h.borrow_mut().on_queue_change = Some(f));
}

pub fn set_on_signal_trigger(f: SignalTriggerHook) {
    HOOKS.with(|h| h.borrow_mut().on_signal_trigger = Some(f));
}

pub fn set_on_route_change(f: RouteChangeHook) {
    HOOKS.with(|h| h.borrow_mut().on_route_change = Some(f));
}

/// Drop every registered hook. Used by tests so a teardown path
/// exists; production code never removes hooks.
#[doc(hidden)]
pub fn _reset() {
    HOOKS.with(|h| *h.borrow_mut() = Hooks::default());
}

// ── fire sites — called from the runtime ─────────────────────────

/// Fired at the end of [`crate::reactive::run_effect`]. `duration`
/// is the effect body's wall-clock time. `scope` is `None` for
/// signal-only effects.
pub(crate) fn fire_effect_run(id: EffectId, scope: Option<ScopeId>, duration: Duration) {
    // Clone out of the RefCell borrow before calling — the handler
    // may itself fire hooks (e.g. an effect scheduled inside the
    // timeline push), and a borrow-across-callback would deadlock.
    let hook = HOOKS.with(|h| h.borrow().on_effect_run.clone());
    if let Some(f) = hook {
        f(id, scope, duration);
    }
}

/// Fired at the end of [`crate::scope::Scope::invoke`]. `args` is
/// passed by reference (not cloned) — handlers that want to log
/// should `to_value`/stringify lazily, inside the hook.
pub(crate) fn fire_handler_invoke(scope: ScopeId, name: &str, args: &Array, duration: Duration) {
    let hook = HOOKS.with(|h| h.borrow().on_handler_invoke.clone());
    if let Some(f) = hook {
        f(scope, name, args, duration);
    }
}

/// Fired once per outermost `dispatch_signal`, after its worklist
/// drain has queued effects. Receives the snapshot of effect ids
/// queued across the whole drain (not the full queue) so the handler
/// doesn't have to diff.
pub(crate) fn fire_queue_change(added: &[EffectId]) {
    let hook = HOOKS.with(|h| h.borrow().on_queue_change.clone());
    if let Some(f) = hook {
        f(added);
    }
}

/// Fired from [`crate::reactive::trigger_signal`] right after the
/// signal dep table is consulted. Handlers can use this to timestamp
/// `last_changed` for a signal graph view.
pub(crate) fn fire_signal_trigger(id: SignalId) {
    let hook = HOOKS.with(|h| h.borrow().on_signal_trigger.clone());
    if let Some(f) = hook {
        f(id);
    }
}

/// Fired from the router after a route mount commits its state.
pub(crate) fn fire_route_change(path: &str, params: &HashMap<String, String>) {
    let hook = HOOKS.with(|h| h.borrow().on_route_change.clone());
    if let Some(f) = hook {
        f(path, params);
    }
}
