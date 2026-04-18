//! Reactivity primitives.
//!
//! Effects subscribe to `(ScopeId, key)` pairs when a proxy `get` fires inside
//! one. A proxy `set` queues subscribers and schedules a microtask flush.
//! Every effect rerun clears its previous dependency set so conditional reads
//! don't leak stale subscriptions.
//!
//! Signals piggyback on the same dep-map via a synthetic
//! [`SIGNAL_SCOPE`] = `ScopeId(0)`, so they share `flush`, batching, and
//! cleanup semantics with proxy-scoped effects.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use js_sys::Promise;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ScopeId(pub u64);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct EffectId(pub u64);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SignalId(pub u64);

/// Synthetic scope used by signals so `track` / `trigger` / flush work
/// unmodified. Real component scopes allocate from `1` upward.
pub const SIGNAL_SCOPE: ScopeId = ScopeId(0);

type EffectFn = Rc<dyn Fn()>;
type SchedulerFn = Rc<dyn Fn(EffectId)>;
type CleanupFn = Box<dyn FnOnce()>;

/// Runtime configuration for an effect. See [`effect_with`].
#[derive(Default, Clone)]
pub struct EffectOptions {
    /// If `true`, the effect is registered but not run until something
    /// schedules it. Useful for [`crate::computed`], which runs on demand.
    pub lazy: bool,
    /// Overrides the default "push to the queue + flush in a microtask"
    /// scheduling. When set, `trigger` hands control to this closure
    /// instead of queueing.
    pub scheduler: Option<SchedulerFn>,
}

thread_local! {
    // Start at 1 so `SIGNAL_SCOPE = ScopeId(0)` is reserved and can never
    // clash with a real scope or effect id.
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static CURRENT_EFFECT: Cell<Option<EffectId>> = const { Cell::new(None) };
    static EFFECTS: RefCell<HashMap<EffectId, EffectFn>> = RefCell::new(HashMap::new());
    static SCHEDULERS: RefCell<HashMap<EffectId, SchedulerFn>> = RefCell::new(HashMap::new());
    static DEPS: RefCell<HashMap<(ScopeId, String), HashSet<EffectId>>> = RefCell::new(HashMap::new());
    static REVERSE: RefCell<HashMap<EffectId, HashSet<(ScopeId, String)>>> = RefCell::new(HashMap::new());
    static QUEUE: RefCell<HashSet<EffectId>> = RefCell::new(HashSet::new());
    static FLUSH_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static CLEANUPS: RefCell<HashMap<EffectId, Vec<CleanupFn>>> = RefCell::new(HashMap::new());
    static BATCHING: Cell<u32> = const { Cell::new(0) };
    static AUTO_FLUSH: Cell<bool> = const { Cell::new(true) };
}

/// Toggle the automatic microtask flush. Production code leaves this at
/// `true` (the default). Tests that want deterministic control flip it to
/// `false` and drive [`flush_sync`] themselves — that side-steps
/// environments (e.g. `wasm-pack test --node`) where `spawn_local` has no
/// microtask host.
pub fn set_auto_flush(enabled: bool) {
    AUTO_FLUSH.with(|a| a.set(enabled));
}

pub fn next_scope_id() -> ScopeId {
    NEXT_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        let out = ScopeId(id);
        debug_assert_ne!(out, SIGNAL_SCOPE, "scope id collided with SIGNAL_SCOPE");
        out
    })
}

fn next_effect_id() -> EffectId {
    NEXT_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        EffectId(id)
    })
}

/// Allocate a fresh `SignalId`. Signals share the id pool with effects and
/// scopes so numeric ids are globally unique across the runtime.
pub fn next_signal_id() -> SignalId {
    NEXT_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        SignalId(id)
    })
}

/// The effect that is currently running, if any. `None` outside of an
/// effect body. Exposed so primitives like [`on_cleanup`] can associate
/// themselves with the caller.
pub fn current_effect() -> Option<EffectId> {
    CURRENT_EFFECT.with(|c| c.get())
}

/// Register and run an effect immediately. Returns its id so callers can
/// later `release` it (e.g. when the owning DOM node is removed).
pub fn effect(f: impl Fn() + 'static) -> EffectId {
    effect_with(f, EffectOptions::default())
}

/// Register an effect with explicit options. A `lazy` effect is stored but
/// not run; a `scheduler` diverts `trigger` to user code instead of the
/// default microtask flush.
pub fn effect_with(f: impl Fn() + 'static, opts: EffectOptions) -> EffectId {
    let id = next_effect_id();
    let f: EffectFn = Rc::new(f);
    EFFECTS.with(|e| e.borrow_mut().insert(id, f.clone()));
    if let Some(sched) = opts.scheduler {
        SCHEDULERS.with(|s| s.borrow_mut().insert(id, sched));
    }
    if !opts.lazy {
        run_effect(id, &f);
    }
    id
}

fn run_effect(id: EffectId, f: &EffectFn) {
    // Tear down the previous run's cleanups before we rebuild deps — a
    // cleanup registered on iteration N belongs to iteration N, not N+1.
    run_cleanups(id);
    clear_deps_for(id);
    let prev = CURRENT_EFFECT.with(|c| c.replace(Some(id)));
    f();
    CURRENT_EFFECT.with(|c| c.set(prev));
}

fn clear_deps_for(id: EffectId) {
    let keys: Option<HashSet<(ScopeId, String)>> =
        REVERSE.with(|r| r.borrow_mut().remove(&id));
    if let Some(keys) = keys {
        DEPS.with(|d| {
            let mut d = d.borrow_mut();
            for k in keys {
                if let Some(set) = d.get_mut(&k) {
                    set.remove(&id);
                    if set.is_empty() {
                        d.remove(&k);
                    }
                }
            }
        });
    }
}

fn run_cleanups(id: EffectId) {
    let pending: Option<Vec<CleanupFn>> =
        CLEANUPS.with(|c| c.borrow_mut().remove(&id));
    if let Some(pending) = pending {
        for f in pending {
            f();
        }
    }
}

/// Remove an effect entirely; all its dependency edges go with it.
pub fn release(id: EffectId) {
    // Final cleanups run first: an effect that opens a resource should
    // close it before it ceases to exist.
    run_cleanups(id);
    clear_deps_for(id);
    EFFECTS.with(|e| e.borrow_mut().remove(&id));
    SCHEDULERS.with(|s| s.borrow_mut().remove(&id));
    QUEUE.with(|q| {
        q.borrow_mut().remove(&id);
    });
}

/// Register a function to run when the enclosing effect reruns or is
/// released. No-op when called outside an effect.
pub fn on_cleanup(f: impl FnOnce() + 'static) {
    let Some(id) = current_effect() else { return };
    CLEANUPS.with(|c| c.borrow_mut().entry(id).or_default().push(Box::new(f)));
}

/// Called from a proxy `get` trap to record the currently-running effect as
/// a subscriber of `(scope_id, key)`.
pub fn track(scope_id: ScopeId, key: &str) {
    let Some(id) = current_effect() else { return };
    let dep = (scope_id, key.to_owned());
    DEPS.with(|d| {
        d.borrow_mut()
            .entry(dep.clone())
            .or_default()
            .insert(id);
    });
    REVERSE.with(|r| {
        r.borrow_mut().entry(id).or_default().insert(dep);
    });
}

/// Called from a proxy `set` trap (or an equivalent mutation path like a
/// handler invocation). Queues subscribers for the next microtask flush.
pub fn trigger(scope_id: ScopeId, key: &str) {
    let dep = (scope_id, key.to_owned());
    let subs: Option<HashSet<EffectId>> = DEPS.with(|d| d.borrow().get(&dep).cloned());
    let Some(subs) = subs else { return };
    if subs.is_empty() {
        return;
    }
    // Route through schedulers first so computeds (and similar) can
    // consume their notifications instead of the default flush.
    let mut queued: HashSet<EffectId> = HashSet::new();
    for id in subs {
        let sched = SCHEDULERS.with(|s| s.borrow().get(&id).cloned());
        match sched {
            Some(s) => s(id),
            None => {
                queued.insert(id);
            }
        }
    }
    if queued.is_empty() {
        return;
    }
    QUEUE.with(|q| q.borrow_mut().extend(queued));
    if BATCHING.with(|b| b.get()) > 0 {
        // A batch is in progress — let its exit schedule the flush.
        return;
    }
    schedule_flush();
}

/// Trigger every `(scope_id, key)` currently tracked for this scope. Used
/// after a handler invocation mutates Rust state directly without going
/// through the proxy's `set` trap.
pub fn trigger_scope(scope_id: ScopeId) {
    let keys: Vec<String> = DEPS.with(|d| {
        d.borrow()
            .keys()
            .filter(|(s, _)| *s == scope_id)
            .map(|(_, k)| k.clone())
            .collect()
    });
    for k in keys {
        trigger(scope_id, &k);
    }
}

/// Coalesce multiple `trigger`s inside `f` into a single flush. Nestable.
pub fn batch<R>(f: impl FnOnce() -> R) -> R {
    BATCHING.with(|b| b.set(b.get() + 1));
    let out = f();
    let remaining = BATCHING.with(|b| {
        let n = b.get() - 1;
        b.set(n);
        n
    });
    if remaining == 0 {
        // Only schedule the deferred flush if anything actually queued.
        let pending = QUEUE.with(|q| !q.borrow().is_empty());
        if pending {
            schedule_flush();
        }
    }
    out
}

fn schedule_flush() {
    if !AUTO_FLUSH.with(|a| a.get()) {
        // Auto-flush disabled (typically tests). Subscribers stay in the
        // queue until a caller drains them via `flush_sync`.
        return;
    }
    if FLUSH_SCHEDULED.with(|f| f.get()) {
        return;
    }
    FLUSH_SCHEDULED.with(|f| f.set(true));
    // A resolved Promise's .then callback runs as a microtask. Spawning via
    // wasm-bindgen-futures is the cleanest way to reach the microtask queue
    // without holding a long-lived Closure ourselves.
    wasm_bindgen_futures::spawn_local(async {
        let _ = JsFuture::from(Promise::resolve(&JsValue::NULL)).await;
        flush();
    });
}

fn flush() {
    FLUSH_SCHEDULED.with(|f| f.set(false));
    // Snapshot and clear so effects that re-trigger during their run land
    // in the next batch, not the current one.
    let ids: Vec<EffectId> =
        QUEUE.with(|q| q.borrow_mut().drain().collect());
    for id in ids {
        let f = EFFECTS.with(|e| e.borrow().get(&id).cloned());
        if let Some(f) = f {
            run_effect(id, &f);
        }
    }
}

/// Force-rerun a specific effect right now. Used by primitives like
/// `computed` that drive their own scheduling via [`EffectOptions`].
pub fn run_now(id: EffectId) {
    let f = EFFECTS.with(|e| e.borrow().get(&id).cloned());
    if let Some(f) = f {
        run_effect(id, &f);
    }
}

/// Drain the queue right now. Exposed so tests can drive the effect loop
/// without spinning the JS event loop; production code should rely on
/// [`trigger`]'s automatic microtask flush.
pub fn flush_sync() {
    flush();
}

#[cfg(debug_assertions)]
pub fn stats() -> (usize, usize) {
    (
        EFFECTS.with(|e| e.borrow().len()),
        DEPS.with(|d| d.borrow().len()),
    )
}

// Keep unused-import noise away when `wasm-bindgen-futures` features drift.
#[allow(dead_code)]
fn _unused(_: Closure<dyn FnMut()>) {}
