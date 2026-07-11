//! Reactivity primitives.
//!
//! RFC-095 W3a — **one dependency graph.** Component fields are
//! interned as signals: the first time an effect tracks
//! `(ScopeId, key)`, the pair mints a `SignalId` (see
//! `FIELD_SIGNALS`), and from then on the field subscribes through
//! the same `SIGNAL_DEPS` forward table `Signal<T>` and
//! `Computed<T>` use. The string key is hashed once per
//! track/trigger to find the id; subscriber-list operations are
//! `u64`-keyed.
//!
//! RFC-098 — effect lifecycle state (body, scheduler, cleanups, and
//! the inverse dep set) lives in one generational `EFFECT_SLAB`
//! entry; `EffectId` packs `generation << 32 | slot`. Subscriber
//! sets and the flush `QUEUE` are insertion-ordered (`IndexSet`),
//! and dispatch is a non-reentrant trampoline.
//!
//! Effects subscribe when a tracked read fires inside one (proxy
//! `get` trap or the RFC-095 W1 scoped root reader). A write
//! queues subscribers and schedules a microtask flush. Every
//! effect rerun clears its previous dependency set so conditional
//! reads don't leak stale subscriptions.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

// RFC-098 H4 — `IndexSet` gives HashSet's API with deterministic
// (insertion-ordered) iteration, so dispatch + flush order is
// replayable from a fuzz seed.
use indexmap::IndexSet;
// RFC-098 H2 — one generational slab holds every effect's lifecycle
// state (body, scheduler, cleanups, signal deps) in a single entry.
use slab::Slab;

use js_sys::Promise;
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

type EffectFn = Rc<dyn Fn()>;
type SchedulerFn = Rc<dyn Fn(EffectId)>;
type CleanupFn = Box<dyn FnOnce()>;

/// RFC-098 H2 — all of one effect's lifecycle state in a single slab
/// entry, replacing the four parallel `EFFECTS` / `SCHEDULERS` /
/// `CLEANUPS` / `SIGNAL_REVERSE` tables that were permitted to
/// disagree. `release` now removes exactly one entry, so a
/// half-released effect is unrepresentable.
struct EffectEntry {
    /// Slot generation. The packed `EffectId` carries the generation
    /// it was minted at; a stale id (held past `release`, whose slot
    /// has been reused) mismatches and resolves to `None`.
    generation: u32,
    body: EffectFn,
    scheduler: Option<SchedulerFn>,
    cleanups: Vec<CleanupFn>,
    /// Inverse dependency set (was `SIGNAL_REVERSE[id]`): the signals
    /// this effect subscribes to, so `clear_deps_for` is O(deps).
    /// Order is never observed, so a plain `HashSet` is right.
    deps: HashSet<SignalId>,
}

/// Runtime configuration for an effect. See [`effect_with`].
#[derive(Default, Clone)]
pub struct EffectOptions {
    /// If `true`, the effect is registered but not run until something
    /// schedules it. Useful for [`crate::computed()`], which runs on demand.
    pub lazy: bool,
    /// Overrides the default "push to the queue + flush in a microtask"
    /// scheduling. When set, `trigger` hands control to this closure
    /// instead of queueing.
    pub scheduler: Option<SchedulerFn>,
}

thread_local! {
    // Scope/Signal ids start at 1 (0 stays an easy-to-spot
    // "never minted" sentinel for them). Effect ids are NOT drawn from
    // here post-RFC-098 — they're slab slots, so the first effect is
    // legitimately `EffectId(0)`.
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static CURRENT_EFFECT: Cell<Option<EffectId>> = const { Cell::new(None) };
    /// RFC-098 H2 — the one effect table. `EffectId = generation<<32 |
    /// slot`; the slab reuses freed slots, `EFFECT_GENERATIONS` bumps
    /// the per-slot generation on release so a reused slot mints a
    /// fresh id and any stale id resolves to `None`. ScopeId/SignalId
    /// keep the `NEXT_ID` counter; only effects use slot addressing.
    static EFFECT_SLAB: RefCell<Slab<EffectEntry>> = const { RefCell::new(Slab::new()) };
    /// Per-slot generation, bumped each time a slot is released so the
    /// next occupant of that slot is distinguishable from the last.
    static EFFECT_GENERATIONS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    /// RFC-095 W3a — field-signal interning. Each `(scope, field
    /// key)` pair lazily mints a `SignalId` the first time an
    /// effect tracks it; from then on the field IS that signal in
    /// the dependency graph. `Signal<T>`, `Computed<T>`, and
    /// component fields all subscribe through `SIGNAL_DEPS` — one
    /// graph, one subscriber-list shape, one teardown path. The
    /// string key is hashed exactly once per (track|trigger) to
    /// find the id; everything downstream is `u64`-keyed.
    static FIELD_SIGNALS: RefCell<HashMap<ScopeId, HashMap<Key, SignalId>>> =
        RefCell::new(HashMap::new());

    /// THE dependency table (RFC-095 W3a): subscriber lists for
    /// signals — which, post-unification, includes every tracked
    /// component field via `FIELD_SIGNALS` interning.
    // RFC-098 H4 — subscriber sets are `IndexSet` so dispatch visits
    // effects in registration order, deterministically. Removal MUST
    // use `shift_remove` (not the default swap-remove) to keep the
    // surviving order stable.
    static SIGNAL_DEPS: RefCell<HashMap<SignalId, IndexSet<EffectId>>> = RefCell::new(HashMap::new());

    // RFC-098 H4 — `IndexSet` so `flush` drains in the order effects
    // were queued; set semantics still dedupe an effect a single
    // dispatch queues twice.
    static QUEUE: RefCell<IndexSet<EffectId>> = RefCell::new(IndexSet::new());
    static FLUSH_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    static BATCHING: Cell<u32> = const { Cell::new(0) };
    static AUTO_FLUSH: Cell<bool> = const { Cell::new(true) };

    /// RFC-098 H3 — trampoline state. `dispatch_signal` is
    /// non-reentrant: the outermost call sets `DISPATCH_DEPTH` to 1 and
    /// owns the `WORKLIST` drain loop. A trigger fired by an inline
    /// scheduler re-enters with depth > 0, appends its signal to the
    /// worklist, and unwinds — dispatch never recurses, so the
    /// re-entrancy the old scratch dance defended against is
    /// structurally impossible.
    ///
    /// The worklist is a FIFO `VecDeque` (push_back / pop_front): a
    /// signal deferred earlier is dispatched earlier, so cross-branch
    /// sibling effects keep trigger order — matching the old recursive
    /// dispatch and honoring H4's registration-order invariant.
    static DISPATCH_DEPTH: Cell<u32> = const { Cell::new(0) };
    static WORKLIST: RefCell<VecDeque<SignalId>> = const { RefCell::new(VecDeque::new()) };
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
        ScopeId(id)
    })
}

// ── RFC-098 H2 — generational effect-id encoding ─────────────────
//
// `EffectId.0 = (generation as u64) << 32 | (slot as u64)`. The slot
// is the slab key (dense from 0); the generation distinguishes
// successive occupants of a reused slot. `EffectId` stays a `Copy u64`
// so the DOM-expando teardown lists and devtools are untouched.

#[inline]
fn pack_effect_id(slot: usize, generation: u32) -> EffectId {
    debug_assert!(slot <= u32::MAX as usize, "effect slot exceeds u32");
    let packed = ((generation as u64) << 32) | (slot as u64 & 0xFFFF_FFFF);
    // The teardown lists round-trip an EffectId through `f64`
    // (`mount.rs` stores `id.0 as f64`), exact only below 2^53 — which
    // holds while generation < 2^21 (~2M reuses of one slot). A hard
    // (release-mode) assert: past that bound the round-trip would lose
    // precision and corrupt DOM-expando teardown SILENTLY, so fail
    // loud instead. One `u64 < const` compare per effect mint —
    // negligible beside the slab insert.
    assert!(
        packed < (1u64 << 53),
        "EffectId {packed:#x} exceeds f64-exact range — a slab slot was reused >2^21 times (generation overflow)"
    );
    EffectId(packed)
}

#[inline]
fn unpack_effect_id(id: EffectId) -> (usize, u32) {
    ((id.0 & 0xFFFF_FFFF) as usize, (id.0 >> 32) as u32)
}

/// Run `f` against `id`'s live slab entry, or return `None` if the id
/// is stale — its slot is empty or has been reused under a newer
/// generation. `f` runs while the slab is borrowed, so it must NOT run
/// user code (body / scheduler / cleanups): clone or take what's
/// needed out and drop the borrow first.
fn with_effect<R>(id: EffectId, f: impl FnOnce(&EffectEntry) -> R) -> Option<R> {
    let (slot, generation) = unpack_effect_id(id);
    EFFECT_SLAB.with(|s| {
        s.borrow()
            .get(slot)
            .filter(|e| e.generation == generation)
            .map(f)
    })
}

/// Mutable counterpart to [`with_effect`]. Same borrow caveat: `f`
/// must not run user code.
fn with_effect_mut<R>(id: EffectId, f: impl FnOnce(&mut EffectEntry) -> R) -> Option<R> {
    let (slot, generation) = unpack_effect_id(id);
    EFFECT_SLAB.with(|s| {
        s.borrow_mut()
            .get_mut(slot)
            .filter(|e| e.generation == generation)
            .map(f)
    })
}

/// Allocate a fresh `SignalId`. Signals share the id pool with scopes
/// so numeric ids are globally unique across the runtime.
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
    let EffectOptions { lazy, scheduler } = opts;
    let id = EFFECT_SLAB.with(|slab| {
        let mut slab = slab.borrow_mut();
        let entry = slab.vacant_entry();
        let slot = entry.key();
        // Generation for this slot: 0 the first time, else one past the
        // last occupant (`EFFECT_GENERATIONS[slot]` was bumped on its
        // release). The freshly-inserted entry uses the SAME slot the
        // VacantEntry reported.
        let generation = EFFECT_GENERATIONS.with(|g| {
            let mut g = g.borrow_mut();
            if slot >= g.len() {
                g.resize(slot + 1, 0);
            }
            g[slot]
        });
        entry.insert(EffectEntry {
            generation,
            body: f.clone(),
            scheduler,
            cleanups: Vec::new(),
            deps: HashSet::new(),
        });
        pack_effect_id(slot, generation)
    });
    if !lazy {
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
    // RFC-095 W3a — one graph: component fields are interned signals,
    // so this single sweep covers both. Take the effect's dep set out
    // of its slab entry (short borrow, no user code), then unsubscribe
    // it from each signal. A stale/released id yields `None` — nothing
    // to clear.
    let sig_keys: Option<HashSet<SignalId>> = with_effect_mut(id, |e| std::mem::take(&mut e.deps));
    if let Some(sig_keys) = sig_keys {
        SIGNAL_DEPS.with(|d| {
            let mut d = d.borrow_mut();
            for sid in sig_keys {
                if let Some(set) = d.get_mut(&sid) {
                    // shift_remove (not swap_remove) keeps survivors in
                    // registration order — H4 determinism (else a teardown
                    // reorders the next dispatch).
                    set.shift_remove(&id);
                    if set.is_empty() {
                        d.remove(&sid);
                    }
                }
            }
        });
    }
}

fn run_cleanups(id: EffectId) {
    // Take the cleanups out under a short borrow, then run them with no
    // slab borrow held — a cleanup may re-enter (e.g. release another
    // effect, mutating the slab).
    let pending: Option<Vec<CleanupFn>> = with_effect_mut(id, |e| std::mem::take(&mut e.cleanups));
    if let Some(pending) = pending {
        for f in pending {
            f();
        }
    }
}

/// Remove an effect entirely; all its dependency edges go with it.
/// RFC-098 H2 — one slab `remove`, so the lifecycle is atomic; a
/// double-release or a stale id is a no-op (generation mismatch).
pub fn release(id: EffectId) {
    // Final cleanups run first: an effect that opens a resource should
    // close it before it ceases to exist. (These take from the entry,
    // so they must precede the removal below.)
    run_cleanups(id);
    clear_deps_for(id);
    let (slot, generation) = unpack_effect_id(id);
    let removed = EFFECT_SLAB.with(|s| {
        let mut s = s.borrow_mut();
        // Only remove when the id is live — guards double-release and
        // stale ids from evicting a slot's newer occupant.
        if s.get(slot).is_some_and(|e| e.generation == generation) {
            s.remove(slot);
            true
        } else {
            false
        }
    });
    if removed {
        // Bump the slot's generation so its next occupant gets a fresh
        // id and this id (now stale) resolves to None forever.
        EFFECT_GENERATIONS.with(|g| {
            if let Some(slot_gen) = g.borrow_mut().get_mut(slot) {
                *slot_gen = slot_gen.wrapping_add(1);
            }
        });
        QUEUE.with(|q| {
            // shift_remove keeps any concurrently-queued effects in order.
            q.borrow_mut().shift_remove(&id);
        });
    }
}

/// Register a function to run when the enclosing effect reruns or is
/// released. No-op when called outside an effect.
pub fn on_cleanup(f: impl FnOnce() + 'static) {
    let Some(id) = current_effect() else { return };
    with_effect_mut(id, |e| e.cleanups.push(Box::new(f)));
}

/// RFC-095 W3a — resolve (or lazily mint) the `SignalId` interned
/// for `(scope_id, key)`. The string key is hashed once here;
/// every downstream subscriber-list operation is `u64`-keyed.
/// `mint = false` callers (trigger paths) treat "never interned"
/// as "never tracked" — nothing to do.
fn field_signal(scope_id: ScopeId, key: &str, mint: bool) -> Option<SignalId> {
    FIELD_SIGNALS.with(|f| {
        let mut map = f.borrow_mut();
        if let Some(sid) = map.get(&scope_id).and_then(|inner| inner.get(key)) {
            return Some(*sid);
        }
        if !mint {
            return None;
        }
        let sid = next_signal_id();
        map.entry(scope_id)
            .or_default()
            .insert(Cow::Owned(key.to_owned()), sid);
        Some(sid)
    })
}

/// RFC-096 S3 — resolve-or-mint the `SignalId` for `(scope, key)`
/// regardless of effect context. The projection store keys on
/// signal ids, and non-effect reads still populate projections.
pub(crate) fn ensure_field_signal(scope_id: ScopeId, key: &str) -> SignalId {
    field_signal(scope_id, key, true).expect("mint=true always yields an id")
}

/// Every signal id interned for `scope_id`'s fields. Feeds the
/// blanket invalidate fallback and storage purges.
pub(crate) fn interned_signal_ids(scope_id: ScopeId) -> Vec<SignalId> {
    FIELD_SIGNALS.with(|f| {
        f.borrow()
            .get(&scope_id)
            .map(|inner| inner.values().copied().collect())
            .unwrap_or_default()
    })
}

/// Called from a proxy `get` trap (or the RFC-095 W1 scoped root
/// reader) to record the currently-running effect as a subscriber
/// of `(scope_id, key)` — which, post-W3a, means subscribing to
/// the field's interned signal.
pub fn track(scope_id: ScopeId, key: &str) {
    if current_effect().is_none() {
        return;
    }
    if let Some(sid) = field_signal(scope_id, key, true) {
        track_signal(sid);
    }
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
    // Record the inverse edge in the effect's own entry (was
    // SIGNAL_REVERSE). `current_effect()` is set only while a live
    // effect runs, so the entry exists; if it somehow doesn't (stale),
    // this no-ops — the forward edge above is harmless on its own (it
    // just dispatches to an id that resolves to None).
    with_effect_mut(id, |e| e.deps.insert(signal_id));
}

/// Dispatch signal `sid`: snapshot its subscribers under a short
/// borrow, drop the borrow, then route each — schedulers fire inline,
/// the rest accumulate in `QUEUE` for the next flush. Shared by
/// `trigger` and `trigger_signal`, which diverge only on how they find
/// `sid`.
///
/// RFC-098 H1 — ONE copy per signal (a local snapshot `Vec`, borrow
/// dropped before any effect runs); H3 — a trampoline, so dispatch
/// never recurses.
///
/// A trigger fired by an inline scheduler re-enters here with
/// `DISPATCH_DEPTH > 0`: it appends its signal to the `WORKLIST` and
/// unwinds. The outermost frame owns the drain loop, popping the
/// worklist until empty. Schedulers still run inline (computed
/// laziness needs the `dirty` mark to be synchronous); only the
/// downstream triggers they fire are deferred. Net effect: the old
/// `mem::take` scratch dance — and the two re-entrancy crashes its
/// comments commemorated — are deleted, not defended.
fn dispatch_signal(sid: SignalId) {
    if DISPATCH_DEPTH.with(|d| d.get()) > 0 {
        // Re-entrant: a scheduler triggered another signal. Enqueue and
        // unwind — the outermost frame drives the loop.
        WORKLIST.with(|w| w.borrow_mut().push_back(sid));
        return;
    }
    DISPATCH_DEPTH.with(|d| d.set(1));

    let mut any_queued = false;
    // Devtools — collect the just-queued ids across the whole drain so
    // the hook fires once per outermost dispatch. Empty when off.
    #[cfg(feature = "devtools")]
    let mut newly_queued: Vec<EffectId> = Vec::new();
    // Debug-only drain bound: a true dependency cycle (a scheduler
    // re-triggering a signal upstream of itself) would grow the
    // worklist without end — convert that hang into a loud failure.
    #[cfg(debug_assertions)]
    let mut drains: usize = 0;

    let mut cursor = sid;
    loop {
        // Snapshot `cursor`'s subscribers in insertion order (H4) under
        // a borrow that ends with this `with` — running an effect or
        // scheduler below can mutate `SIGNAL_DEPS` (via
        // `clear_deps_for`), so the borrow must not outlive the copy.
        let local: Vec<EffectId> = SIGNAL_DEPS.with(|d| {
            d.borrow()
                .get(&cursor)
                .map(|set| set.iter().copied().collect())
                .unwrap_or_default()
        });
        for eid in local {
            // Resolve the subscriber's scheduler, cloned out so the slab
            // borrow drops before it runs. Three outcomes:
            //   None         — stale id (released mid-dispatch): skip.
            //   Some(None)    — live, no scheduler: queue for flush.
            //   Some(Some(s)) — live computed/custom: run inline.
            let sched = with_effect(eid, |e| e.scheduler.clone());
            match sched {
                None => continue,
                // Inline; any trigger it fires lands in WORKLIST above.
                Some(Some(s)) => s(eid),
                Some(None) => {
                    QUEUE.with(|q| {
                        q.borrow_mut().insert(eid);
                    });
                    any_queued = true;
                    #[cfg(feature = "devtools")]
                    newly_queued.push(eid);
                }
            }
        }
        match WORKLIST.with(|w| w.borrow_mut().pop_front()) {
            Some(next) => cursor = next,
            None => break,
        }
        #[cfg(debug_assertions)]
        {
            drains += 1;
            debug_assert!(
                drains < 1_000_000,
                "RFC-098 H3: dispatch worklist exceeded 1e6 drains — likely a \
                 reactive dependency cycle (a scheduler re-triggering a signal \
                 upstream of itself)"
            );
        }
    }

    DISPATCH_DEPTH.with(|d| d.set(0));

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
/// Post-W3a this is a lookup of the field's interned signal — a key
/// nobody ever tracked has no signal and dispatches nothing. Fields
/// dispatch quietly (no devtools signal hook): they aren't user
/// `Signal`s, and the devtools timeline has its own field events.
pub fn trigger(scope_id: ScopeId, key: &str) {
    let Some(sid) = field_signal(scope_id, key, false) else {
        return;
    };
    dispatch_signal(sid);
}

/// Signal-targeted trigger. Skips the `(scope_id, key)` lookup path
/// entirely — signal deps live in their own table keyed on `SignalId`.
pub fn trigger_signal(signal_id: SignalId) {
    dispatch_signal(signal_id);
    // Devtools hook — fires on every signal trigger regardless of
    // whether there are subscribers, so `last_changed` still updates
    // for "unread" signals the graph panel displays.
    #[cfg(feature = "devtools")]
    crate::devtools::hooks::fire_signal_trigger(signal_id);
}

/// Drop every reactivity-side entry associated with `scope_id`.
/// Called from `Scope::remove` alongside refs/slots/context
/// cleanups. Effects associated with the scope are independently
/// released via `mount::release_subtree` → `release(EffectId)`;
/// this evicts the scope's interned field signals and their
/// subscriber lists. A still-living effect's inverse dep set (in its
/// slab entry) may briefly name an evicted signal; that entry just
/// won't be found at the effect's next teardown — a harmless no-op.
pub fn clear_scope(scope_id: ScopeId) {
    let sids: Vec<SignalId> = FIELD_SIGNALS.with(|f| {
        f.borrow_mut()
            .remove(&scope_id)
            .map(|inner| inner.into_values().collect())
            .unwrap_or_default()
    });
    release_field_signals(&sids);
}

/// Evict the subscriber lists (`SIGNAL_DEPS`) and projection/version
/// storage for a batch of interned field signals already removed from
/// `FIELD_SIGNALS`. Shared tail of [`clear_scope`] and [`clear_scopes`].
fn release_field_signals(sids: &[SignalId]) {
    if sids.is_empty() {
        return;
    }
    SIGNAL_DEPS.with(|d| {
        let mut d = d.borrow_mut();
        for sid in sids {
            d.remove(sid);
        }
    });
    crate::scope::purge_field_storage(sids);
}

/// Bulk variant for the RFC 054 compiled-row bulk-clear path.
/// Drains the field-signal interning (and subscriber lists) for
/// every targeted scope in a single pass per table.
pub fn clear_scopes(scope_ids: &[ScopeId]) {
    if scope_ids.is_empty() {
        return;
    }
    let sids: Vec<SignalId> = FIELD_SIGNALS.with(|f| {
        let mut map = f.borrow_mut();
        let mut out = Vec::new();
        for id in scope_ids {
            if let Some(inner) = map.remove(id) {
                out.extend(inner.into_values());
            }
        }
        out
    });
    release_field_signals(&sids);
}

/// Trigger every key currently tracked for this scope. RFC-095 W2
/// demoted this from the post-handler default to the conservative
/// FALLBACK: the per-field dirty sweep (`scope::DirtySweep`)
/// triggers only changed keys, and callers reach for this sweep
/// only when the snapshot couldn't run (re-entrant borrow, dead
/// scope). O(k) in the scope's tracked keys via the nested
/// `DEPS[scope]` map.
pub fn trigger_scope(scope_id: ScopeId) {
    for k in tracked_keys(scope_id) {
        trigger(scope_id, k.as_ref());
    }
}

/// Every key currently tracked (= having at least one subscribed
/// effect) for `scope_id`. Cloned out because callers go on to
/// mutate DEPS (trigger → effect reruns re-track); clones of
/// `Cow::Borrowed(&'static)` are zero-cost, owned entries allocate
/// but the set is bounded by the scope's field count (typically
/// < 20). RFC-095 W2 — also feeds the dirty sweep's observed-key
/// set.
pub(crate) fn tracked_keys(scope_id: ScopeId) -> Vec<Key> {
    FIELD_SIGNALS.with(|f| {
        f.borrow()
            .get(&scope_id)
            .map(|inner| inner.keys().cloned().collect())
            .unwrap_or_default()
    })
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
    let ids: Vec<EffectId> = QUEUE.with(|q| q.borrow_mut().drain(..).collect());
    for id in ids {
        // Clone the body out (Rc bump) and drop the slab borrow before
        // running it. A `None` means the effect was released after it
        // was queued — skip it.
        let body = with_effect(id, |e| e.body.clone());
        if let Some(body) = body {
            run_effect(id, &body);
        }
    }
}

/// Force-rerun a specific effect right now. Used by primitives like
/// `computed` that drive their own scheduling via [`EffectOptions`].
pub fn run_now(id: EffectId) {
    let body = with_effect(id, |e| e.body.clone());
    if let Some(body) = body {
        run_effect(id, &body);
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
    // Post-W3a: count interned field signals (the live tracked
    // surface) — same growth signal the health panel watched.
    let dep_count =
        FIELD_SIGNALS.with(|f| f.borrow().values().map(|inner| inner.len()).sum::<usize>());
    (EFFECT_SLAB.with(|s| s.borrow().len()), dep_count)
}

// ── devtools read-only snapshots ─────────────────────────────────
//
// Cheap-to-build snapshots of internal state for the devtools
// panels (PR D onwards). Gated behind the devtools feature so they
// don't contribute to default-feature-off release binaries.

/// Snapshot of the effect ids currently queued for the next flush,
/// in the order the next flush will run them — `QUEUE` is insertion-
/// ordered (RFC-098 H4), so this is deterministic and replayable.
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
    with_effect(id, |e| e.scheduler.is_some()).unwrap_or(false)
}
