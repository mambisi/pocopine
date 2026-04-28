//! Reactivity primitives.
//!
//! Effects subscribe to `(ScopeId, key)` pairs when a proxy `get` fires inside
//! one. A proxy `set` queues subscribers and schedules a microtask flush.
//! Every effect rerun clears its previous dependency set so conditional reads
//! don't leak stale subscriptions.
//!
//! Signals have their own dedicated dep table keyed on `SignalId` directly
//! (see `SIGNAL_DEPS` / `SIGNAL_REVERSE`). They share the effect engine,
//! queue, flush, and batching with proxy-scoped subscriptions — just not
//! the key type. The `SIGNAL_SCOPE` sentinel `ScopeId(0)` is reserved so
//! a future path could re-join the two tables without a scope-id collision.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use js_sys::Promise;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

/// Dependency-map key: the scope-scoped field name. Stored as
/// `Cow<'static, str>` so a macro-generated `&'static str` threads
/// through to the HashMap without allocation, while a dynamically-
/// built key (proxy `get`/`set` traps, dotted paths) owns its
/// string exactly once.
pub type Key = Cow<'static, str>;

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
    /// Nested layout: `DEPS[scope][key] = set<EffectId>`. The outer
    /// level is the scope id; the inner level is the per-scope key
    /// map. Two wins over the old flat `HashMap<(ScopeId, Key), _>`:
    ///
    ///   1. Key lookup uses `&str` directly via HashMap's Borrow
    ///      trait on `Cow<'static, str>: Borrow<str>`. The flat
    ///      form needed a `(ScopeId, Cow<_>)` probe which required
    ///      the string to be `'static` — forcing an allocation on
    ///      every lookup.
    ///   2. `trigger_scope` becomes O(k) in the scope's live keys
    ///      — iterate the inner map. The old flat form had to scan
    ///      every `(scope, key)` pair in the app.
    static DEPS: RefCell<HashMap<ScopeId, HashMap<Key, HashSet<EffectId>>>> =
        RefCell::new(HashMap::new());
    static REVERSE: RefCell<HashMap<EffectId, HashSet<(ScopeId, Key)>>> =
        RefCell::new(HashMap::new());

    /// Dedicated signal dependency table. Signals used to piggyback
    /// on the proxy dep map via a stringified id + `SIGNAL_SCOPE`
    /// pseudo-scope, which cost two allocations per access (one in
    /// `id.to_string()`, one in the key-type's `.to_owned()`). Keyed
    /// on `SignalId` directly the cost is zero.
    static SIGNAL_DEPS: RefCell<HashMap<SignalId, HashSet<EffectId>>> = RefCell::new(HashMap::new());
    static SIGNAL_REVERSE: RefCell<HashMap<EffectId, HashSet<SignalId>>> = RefCell::new(HashMap::new());

    static QUEUE: RefCell<HashSet<EffectId>> = RefCell::new(HashSet::new());
    static FLUSH_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static CLEANUPS: RefCell<HashMap<EffectId, Vec<CleanupFn>>> = RefCell::new(HashMap::new());
    static BATCHING: Cell<u32> = const { Cell::new(0) };
    static AUTO_FLUSH: Cell<bool> = const { Cell::new(true) };

    /// Reusable scratch buffer for `trigger`'s subscriber snapshot.
    /// `trigger` must iterate subscribers outside the `DEPS` borrow
    /// because running an effect can mutate `DEPS` (via
    /// `clear_deps_for`). A per-thread scratch `Vec` avoids the
    /// `HashSet::clone()` allocation every hot-path `trigger` used
    /// to pay.
    static TRIGGER_SCRATCH: RefCell<Vec<EffectId>> = RefCell::new(Vec::with_capacity(16));
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

/// Scope-bound counterpart to [`effect`] — installs the effect and
/// registers a cleanup against the current scope's unmount, so the
/// effect is released automatically when the component goes away.
/// Returns nothing; storage is implicit.
///
/// Same shape as `events::on_scoped` and `timers::after_scoped` —
/// the right default inside lifecycle hooks where the effect
/// should outlive the install but die with the scope.
pub fn effect_scoped(f: impl Fn() + 'static) {
    let id = effect(f);
    crate::events::on_scope_unmount(move || release(id));
}

/// Register an effect with explicit options. A `lazy` effect is stored but
/// not run; a `scheduler` diverts `trigger` to user code instead of the
/// default microtask flush.
pub fn effect_with(f: impl Fn() + 'static, opts: EffectOptions) -> EffectId {
    effect_with_dyn(Rc::new(f), opts)
}

// RFC-058 Phase 6.5 — type-erased body. The generic shim above
// performs the `Rc::new(f)` coercion to `EffectFn` (one
// monomorphization per call site, but each is just the
// `Rc::new + forward` instructions). The body that does the
// registry insertion + run lives here as a single instantiation.
// Twiggy showed `effect_with::<F>` totalling ~7 KB across pp-for
// / pp-if / pp-text / pp-bind closure types before this
// consolidation.
fn effect_with_dyn(f: EffectFn, opts: EffectOptions) -> EffectId {
    let id = next_effect_id();
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
    #[cfg(feature = "devtools")]
    let start = now_ms();
    f();
    CURRENT_EFFECT.with(|c| c.set(prev));
    // Devtools hook — fires at the END so `duration` covers the full
    // body (including track calls) but excludes cleanup + dep-clear.
    #[cfg(feature = "devtools")]
    {
        let dur = std::time::Duration::from_micros(((now_ms() - start).max(0.0) * 1000.0) as u64);
        crate::devtools::hooks::fire_effect_run(id, None, dur);
    }
}

#[cfg(feature = "devtools")]
fn now_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}

fn clear_deps_for(id: EffectId) {
    // Proxy-scope deps.
    let keys: Option<HashSet<(ScopeId, Key)>> = REVERSE.with(|r| r.borrow_mut().remove(&id));
    if let Some(keys) = keys {
        DEPS.with(|d| {
            let mut d = d.borrow_mut();
            for (scope, key) in keys {
                if let Some(inner) = d.get_mut(&scope) {
                    if let Some(set) = inner.get_mut(&key) {
                        set.remove(&id);
                        if set.is_empty() {
                            inner.remove(&key);
                        }
                    }
                    if inner.is_empty() {
                        d.remove(&scope);
                    }
                }
            }
        });
    }
    // Signal deps — own table, indexed on SignalId.
    let sig_keys: Option<HashSet<SignalId>> = SIGNAL_REVERSE.with(|r| r.borrow_mut().remove(&id));
    if let Some(sig_keys) = sig_keys {
        SIGNAL_DEPS.with(|d| {
            let mut d = d.borrow_mut();
            for sid in sig_keys {
                if let Some(set) = d.get_mut(&sid) {
                    set.remove(&id);
                    if set.is_empty() {
                        d.remove(&sid);
                    }
                }
            }
        });
    }
}

fn run_cleanups(id: EffectId) {
    let pending: Option<Vec<CleanupFn>> = CLEANUPS.with(|c| c.borrow_mut().remove(&id));
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
    // Borrow-lookup first: `HashMap<Key, _>` supports `get(&str)`
    // via `Cow<'static, str>: Borrow<str>`. When the subscription
    // already exists (the common hot-path case — an effect that
    // re-reads fields it's already subscribed to), we bail without
    // any allocation. On first track we own the string exactly
    // once and share the `Cow::Owned` across the DEPS key and the
    // REVERSE entry.
    let already_present = DEPS.with(|d| {
        d.borrow()
            .get(&scope_id)
            .and_then(|inner| inner.get(key))
            .map(|set| set.contains(&id))
            .unwrap_or(false)
    });
    if already_present {
        return;
    }
    let owned: Key = Cow::Owned(key.to_owned());
    DEPS.with(|d| {
        d.borrow_mut()
            .entry(scope_id)
            .or_default()
            .entry(owned.clone())
            .or_default()
            .insert(id);
    });
    REVERSE.with(|r| {
        r.borrow_mut()
            .entry(id)
            .or_default()
            .insert((scope_id, owned));
    });
}

/// Subscribe the currently-running effect to `signal_id`. Signals
/// keep their own dep table so there's no per-access string-id
/// conversion or allocation.
pub fn track_signal(signal_id: SignalId) {
    let Some(id) = current_effect() else { return };
    let already = SIGNAL_DEPS.with(|d| {
        d.borrow()
            .get(&signal_id)
            .map(|set| set.contains(&id))
            .unwrap_or(false)
    });
    if already {
        return;
    }
    SIGNAL_DEPS.with(|d| {
        d.borrow_mut().entry(signal_id).or_default().insert(id);
    });
    SIGNAL_REVERSE.with(|r| {
        r.borrow_mut().entry(id).or_default().insert(signal_id);
    });
}

/// Snapshot `subs` into a local buffer, route per-effect scheduler or
/// queue, schedule flush if needed. Factored out so `trigger` and
/// `trigger_signal` share the dispatch path — they diverge only on
/// how they look up subscribers.
fn dispatch_subs(subs: &HashSet<EffectId>) {
    if subs.is_empty() {
        return;
    }
    // Take ownership of the thread-local scratch so re-entrant
    // dispatch_subs calls (an inline scheduler that mutates a signal
    // and triggers another dispatch) don't clobber our buffer
    // mid-iteration. Pre-fix, the shared scratch surfaced as
    // "RefCell already borrowed" or "index out of bounds: len 0
    // index N" deep in the trigger path — both symptoms of the inner
    // call having `clear()`ed the outer call's snapshot.
    let mut local = TRIGGER_SCRATCH.with(|s| std::mem::take(&mut *s.borrow_mut()));
    local.clear();
    local.extend(subs.iter().copied());
    let ids_len = local.len();
    if ids_len == 0 {
        // Restore the (empty, possibly pre-grown) buffer for reuse.
        TRIGGER_SCRATCH.with(|s| {
            let mut current = s.borrow_mut();
            if local.capacity() > current.capacity() {
                *current = local;
            }
        });
        return;
    }
    // Drain the local buffer into dispatch. Schedulers fire inline;
    // the rest accumulate in QUEUE. No intermediate HashSet<EffectId>
    // clone, and re-entry is safe because each dispatch owns its
    // own buffer.
    let mut any_queued = false;
    // Devtools — collect the just-queued ids so the hook sees the
    // delta, not the whole queue. Empty when feature is off.
    #[cfg(feature = "devtools")]
    let mut newly_queued: Vec<EffectId> = Vec::new();
    for &eid in local.iter().take(ids_len) {
        let sched = SCHEDULERS.with(|s| s.borrow().get(&eid).cloned());
        match sched {
            Some(s) => s(eid),
            None => {
                QUEUE.with(|q| {
                    q.borrow_mut().insert(eid);
                });
                any_queued = true;
                #[cfg(feature = "devtools")]
                newly_queued.push(eid);
            }
        }
    }
    local.clear();
    // Return the buffer for the next non-reentrant call to reuse —
    // keep whichever copy has the larger capacity so we don't lose
    // the pre-grown allocation.
    TRIGGER_SCRATCH.with(|s| {
        let mut current = s.borrow_mut();
        if local.capacity() > current.capacity() {
            *current = local;
        }
    });
    #[cfg(feature = "devtools")]
    if !newly_queued.is_empty() {
        crate::devtools::hooks::fire_queue_change(&newly_queued);
    }
    if !any_queued {
        return;
    }
    if BATCHING.with(|b| b.get()) > 0 {
        // A batch is in progress — let its exit schedule the flush.
        return;
    }
    schedule_flush();
}

/// Called from a proxy `set` trap (or an equivalent mutation path like a
/// handler invocation). Queues subscribers for the next microtask flush.
pub fn trigger(scope_id: ScopeId, key: &str) {
    let subs: Option<HashSet<EffectId>> = DEPS.with(|d| {
        d.borrow()
            .get(&scope_id)
            .and_then(|inner| inner.get(key))
            .cloned()
    });
    if let Some(subs) = subs {
        dispatch_subs(&subs);
    }
}

/// Signal-targeted trigger. Skips the `(scope_id, key)` lookup path
/// entirely — signal deps live in their own table keyed on `SignalId`.
pub fn trigger_signal(signal_id: SignalId) {
    let subs: Option<HashSet<EffectId>> = SIGNAL_DEPS.with(|d| d.borrow().get(&signal_id).cloned());
    if let Some(subs) = subs {
        dispatch_subs(&subs);
    }
    // Devtools hook — fires on every signal trigger regardless of
    // whether there are subscribers, so `last_changed` still updates
    // for "unread" signals the graph panel displays.
    #[cfg(feature = "devtools")]
    crate::devtools::hooks::fire_signal_trigger(signal_id);
}

/// Drop every reactivity-side entry associated with `scope_id`.
/// Called from `Scope::remove` alongside refs/slots/id/context
/// cleanups. Effects associated with the scope are independently
/// released via `mount::release_subtree` → `release(EffectId)`;
/// this just evicts the scope's inner DEPS map.
pub fn clear_scope(scope_id: ScopeId) {
    DEPS.with(|d| {
        d.borrow_mut().remove(&scope_id);
    });
}

/// Bulk variant for the RFC 054 compiled-row bulk-clear path.
/// Drains DEPS for every targeted scope in a single
/// `thread_local::with` borrow.
pub fn clear_scopes(scope_ids: &[ScopeId]) {
    if scope_ids.is_empty() {
        return;
    }
    DEPS.with(|d| {
        let mut map = d.borrow_mut();
        for id in scope_ids {
            map.remove(id);
        }
    });
}

/// Trigger every key currently tracked for this scope. Used after a
/// handler invocation mutates Rust state directly without going
/// through the proxy's `set` trap. O(k) in the scope's tracked keys
/// via the nested `DEPS[scope]` map — not O(|DEPS|) like the old
/// flat scan.
pub fn trigger_scope(scope_id: ScopeId) {
    // Clone the inner key list out because `trigger` below will
    // mutate DEPS (via effect reruns through the scheduler), and
    // we can't hold DEPS borrowed across that. Clones of
    // `Cow::Borrowed(&'static)` are zero-cost; owned entries
    // allocate but the set is bounded by the scope's field count
    // (typically < 20).
    let keys: Vec<Key> = DEPS.with(|d| {
        d.borrow()
            .get(&scope_id)
            .map(|inner| inner.keys().cloned().collect())
            .unwrap_or_default()
    });
    for k in keys {
        trigger(scope_id, k.as_ref());
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
    let ids: Vec<EffectId> = QUEUE.with(|q| q.borrow_mut().drain().collect());
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

/// `(effect_count, dep_count)` — cheap health counters consumed by
/// the devtools memory panel. Gated to debug builds + any build with
/// the `devtools` feature on, so opt-in release devtools still
/// gets real numbers.
#[cfg(any(debug_assertions, feature = "devtools"))]
pub fn stats() -> (usize, usize) {
    let dep_count = DEPS.with(|d| d.borrow().values().map(|inner| inner.len()).sum::<usize>());
    (EFFECTS.with(|e| e.borrow().len()), dep_count)
}

// ── devtools read-only snapshots ─────────────────────────────────
//
// Cheap-to-build snapshots of internal state for the devtools
// panels (PR D onwards). Gated behind the devtools feature so they
// don't contribute to default-feature-off release binaries.

/// Snapshot of the effect ids currently queued for the next flush.
/// Order is unspecified — `QUEUE` is a `HashSet`. Consumers that
/// need deterministic ordering should sort by id at the display
/// boundary.
#[cfg(feature = "devtools")]
pub fn queue_snapshot() -> Vec<EffectId> {
    QUEUE.with(|q| q.borrow().iter().copied().collect())
}

/// Per-signal subscriber count + id. Consumed by the signal-graph
/// panel — combined with `hooks::signal_last_changed` it drives the
/// "what's reactive in this app?" view.
#[cfg(feature = "devtools")]
#[derive(Debug, Clone)]
pub struct SignalSnapshot {
    pub id: SignalId,
    pub subscribers: usize,
}

/// Every signal with at least one subscriber. Signals that nothing
/// is watching don't appear — they'd fill the panel with noise for
/// no useful information.
#[cfg(feature = "devtools")]
pub fn signal_graph_snapshot() -> Vec<SignalSnapshot> {
    SIGNAL_DEPS.with(|d| {
        d.borrow()
            .iter()
            .map(|(id, subs)| SignalSnapshot {
                id: *id,
                subscribers: subs.len(),
            })
            .collect()
    })
}

/// Does `effect_id` route through a custom scheduler? True for
/// computeds (see `computed::computed`). The flush-queue panel
/// distinguishes these because they don't land in the default
/// microtask queue.
#[cfg(feature = "devtools")]
pub fn is_scheduler_routed(id: EffectId) -> bool {
    SCHEDULERS.with(|s| s.borrow().contains_key(&id))
}

// Keep unused-import noise away when `wasm-bindgen-futures` features drift.
#[allow(dead_code)]
fn _unused(_: Closure<dyn FnMut()>) {}
