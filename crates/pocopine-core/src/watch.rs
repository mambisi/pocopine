//! `watch` — imperative change listener.
//!
//! Reads `source()` inside an effect; when the returned value differs from
//! the previous run, calls `cb(new, previous)`. Returns the backing
//! [`EffectId`] so callers can `release` the watcher when they're done
//! with it.
//!
//! `watch_field` is the ergonomic sugar for the single-field-on-self
//! case (RFC-026) — reads the named field through the scope proxy so
//! the effect actually subscribes to the right dep (the common pitfall
//! with `Handle::with` is that it doesn't go through the proxy, so the
//! watch silently never fires).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde::de::DeserializeOwned;

use crate::reactive::{EffectId, ScopeId, effect, track};
use crate::scope::{Scope, current_scope_id};

/// Watch `source` and call `cb` whenever its value changes.
///
/// `cb` fires once on the initial run (with `previous = None`), then once
/// per distinct subsequent value. Equality is checked with `PartialEq`.
pub fn watch<T, S, C>(source: S, cb: C) -> EffectId
where
    T: Clone + PartialEq + 'static,
    S: Fn() -> T + 'static,
    C: Fn(&T, Option<&T>) + 'static,
{
    let prev: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
    let prev_w = prev.clone();
    effect(move || {
        let next = source();
        let last = prev_w.borrow().clone();
        if last.as_ref() != Some(&next) {
            cb(&next, last.as_ref());
            *prev_w.borrow_mut() = Some(next);
        }
    })
}

/// Reactive watcher on a single named field of the current scope.
/// RFC-026.
///
/// Reads through the scope's JS proxy so the effect subscribes
/// correctly via the `get` trap. This is the same access path
/// directives use — which is why `pp-text="open"` fires when `open`
/// changes but `Handle::with(|s| s.open)` inside a plain `watch()`
/// source silently doesn't (it bypasses the proxy).
///
/// ```ignore
/// watch_field::<bool>("open", |&is_open, prev| match (prev, is_open) {
///     (None, true) | (Some(false), true) => activate(),
///     (Some(true), false) => deactivate(),
///     _ => {}
/// });
/// ```
///
/// Must be called inside a handler or lifecycle method — it reads
/// [`current_scope_id`] at install time. Panics with a clear
/// message outside that context so a programming error surfaces
/// immediately rather than silently never subscribing.
///
/// The actual `effect` install is deferred to the next microtask
/// so the initial read doesn't clash with the caller's active
/// `&mut self` borrow (the common case — `on_mount(&mut self)`
/// calling `watch_field` while the mount still holds the mutable
/// borrow on state). The effect's `get`-trap read needs an
/// *immutable* borrow of state; without the defer, that reentry
/// trips `RefCell::borrow` and the source silently returns
/// `V::default()` — which, for `bool`, is `false`, producing the
/// exact "watch never fires correctly" bug this helper is meant
/// to eliminate.
pub fn watch_field<V, C>(field: &'static str, cb: C)
where
    V: Clone + PartialEq + Default + DeserializeOwned + 'static,
    C: Fn(&V, Option<&V>) + 'static,
{
    let scope_id =
        current_scope_id().expect("watch_field called outside a handler / lifecycle context");
    watch_scope_field(scope_id, field, cb);
}

/// Like [`watch_field`] but observes a named field on an explicit
/// scope — used by compound-component children (provide/inject per
/// RFC-027) to mirror a parent's reactive state into their own.
///
/// Reads via `track` + a direct `state.borrow().get(field)` rather
/// than constructing a proxy each rerun; the proxy path leaks its
/// closures via `.forget()` and runs with each re-evaluation.
pub fn watch_scope_field<V, C>(scope_id: ScopeId, field: &'static str, cb: C)
where
    V: Clone + PartialEq + Default + DeserializeOwned + 'static,
    C: Fn(&V, Option<&V>) + 'static,
{
    crate::tick::next(move || {
        watch_scope_field_now(scope_id, field, cb);
    });
}

fn read_scope_field<V>(scope_id: ScopeId, field: &'static str) -> V
where
    V: Clone + PartialEq + Default + DeserializeOwned + 'static,
{
    let Some(scope) = Scope::find(scope_id) else {
        return V::default();
    };
    // A delayed install may race the target scope's teardown. Do not
    // recreate an interned field signal after `Scope::remove` has purged it.
    track(scope_id, field);
    let v = scope.state.borrow().get(field);
    serde_wasm_bindgen::from_value::<V>(v).unwrap_or_default()
}

/// Synchronous variant of [`watch_scope_field`].
///
/// The watcher subscribes immediately, so the initial run sees the
/// field's current value even if it was written earlier in the same
/// mount sequence. Call this only from contexts where reading the
/// target scope immediately is borrow-safe (for example `on_ready`,
/// which runs behind an immutable borrow).
#[doc(hidden)]
pub fn watch_scope_field_now<V, C>(scope_id: ScopeId, field: &'static str, cb: C) -> EffectId
where
    V: Clone + PartialEq + Default + DeserializeOwned + 'static,
    C: Fn(&V, Option<&V>) + 'static,
{
    let id = watch(move || read_scope_field::<V>(scope_id, field), cb);
    // RFC-115 — the cycle-guard report names the watched field
    // instead of a bare effect id.
    crate::reactive::set_effect_label(id, field);
    id
}

/// RFC-115 — one payload-less subscription across several named
/// fields of one scope (`#[watch(a, b, c)]`).
///
/// One effect tracks every listed field and invokes `cb` when any of
/// them fires. Coalescing is scheduler-native: same-flush triggers on
/// several listed fields queue the effect once (RFC-098 H4 queue
/// dedup), so `cb` runs once per flush pass — and because the handler
/// runs inside the flush, the flush-cascade cycle guard bounds it
/// like any other queued effect.
///
/// The payload-less form has no `PartialEq` value gate, so three
/// guards replace it:
///
/// - **Install validation** — a listed key that isn't a state field
///   (a typo) or is a `#[computed]` key is reported on the console
///   and NOT tracked: a tracked key with no fingerprint arm is
///   conservatively re-triggered by every dirty sweep on the scope,
///   which would re-fire this handler on every unrelated update.
/// - **Provably-unchanged skip** — each run probes the listed
///   fields' quick-lens + fingerprints; when every probe proves
///   "unchanged" the callback is skipped. A `None` fingerprint
///   proves nothing, so those runs stay conservative.
/// - **Echo suppression** — the handler's own dirty sweep
///   conservatively re-triggers keys it observed; after `cb` returns
///   the effect dequeues itself, so a handler's own writes (and
///   sweep echoes) never re-fire it. Multi-field handlers react to
///   external changes only — recompute output can't re-invalidate
///   its own input set.
///
/// The initial seed runs once, deferred one tick: the install
/// typically happens behind `on_ready`'s live borrow, and the seed
/// callback usually re-enters the scope via `Handle::update`.
/// `label` feeds the cycle-guard report; the macro passes the joined
/// field list.
#[doc(hidden)]
pub fn watch_scope_fields<C>(
    scope_id: ScopeId,
    fields: &'static [&'static str],
    label: &'static str,
    cb: C,
) -> EffectId
where
    C: Fn() + 'static,
{
    let cb: Rc<dyn Fn()> = Rc::new(cb);
    // Validate once at install — reject keys the sweep can never
    // prove unchanged (see doc above). Loud, not silent: RFC-115.
    let tracked: Rc<Vec<&'static str>> = Rc::new(match Scope::find(scope_id) {
        Some(scope) => {
            let state = scope.state.borrow();
            fields
                .iter()
                .copied()
                .filter(|f| {
                    if !state.keys().contains(f) {
                        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                            "pocopine: #[watch] field `{f}` does not exist on this \
                             component — it is ignored (check the field list for typos)"
                        )));
                        false
                    } else if state.is_computed_field(f) {
                        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                            "pocopine: #[watch] field `{f}` is a #[computed] key — \
                             computed fields can't be watched; watch their inputs \
                             instead. It is ignored."
                        )));
                        false
                    } else {
                        true
                    }
                })
                .collect()
        }
        None => fields.to_vec(),
    });

    type Probes = Vec<(Option<u64>, Option<u64>)>;
    fn probe(scope_id: ScopeId, tracked: &[&'static str]) -> Option<Probes> {
        let scope = Scope::find(scope_id)?;
        let state = scope.state.borrow();
        Some(
            tracked
                .iter()
                .map(|f| (state.field_quick_len(f), state.field_fingerprint(f)))
                .collect(),
        )
    }
    fn provably_unchanged(prev: &Option<Probes>, now: &Option<Probes>) -> bool {
        match (prev, now) {
            (Some(prev), Some(now)) => {
                prev.len() == now.len()
                    && prev.iter().zip(now).all(|((pl, pf), (nl, nf))| {
                        pl == nl && matches!((pf, nf), (Some(a), Some(b)) if a == b)
                    })
            }
            _ => false,
        }
    }

    let seed_pending = Rc::new(Cell::new(true));
    let seed_ticket = Rc::new(Cell::new(0_u64));
    let self_id: Rc<Cell<Option<EffectId>>> = Rc::new(Cell::new(None));
    let prev_probes: Rc<RefCell<Option<Probes>>> = Rc::new(RefCell::new(None));

    let id = effect({
        let cb = cb.clone();
        let tracked = tracked.clone();
        let seed_pending = seed_pending.clone();
        let seed_ticket = seed_ticket.clone();
        let self_id = self_id.clone();
        let prev_probes = prev_probes.clone();
        move || {
            if Scope::find(scope_id).is_none() {
                return;
            }
            for field in tracked.iter() {
                track(scope_id, field);
            }
            let now = probe(scope_id, &tracked);
            if seed_pending.get() {
                // Coalesced initial seed, deferred one microtask —
                // the install runs behind on_ready's live borrow and
                // the seed re-enters the scope via `Handle::update`.
                // Scheduled via `spawn_local` (the same cross-host
                // microtask the flush scheduler uses) rather than
                // `tick::next`, which needs a `window` and silently
                // no-ops in windowless hosts.
                let ticket = seed_ticket.get() + 1;
                seed_ticket.set(ticket);
                let pending = seed_pending.clone();
                let tickets = seed_ticket.clone();
                let cb = cb.clone();
                let tracked = tracked.clone();
                let self_id = self_id.clone();
                let prev_probes = prev_probes.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
                        &wasm_bindgen::JsValue::NULL,
                    ))
                    .await;
                    if !pending.get() || tickets.get() != ticket {
                        return;
                    }
                    pending.set(false);
                    cb();
                    *prev_probes.borrow_mut() = probe(scope_id, &tracked);
                    if let Some(me) = self_id.get() {
                        crate::reactive::dequeue_effect(me);
                    }
                });
                return;
            }
            if provably_unchanged(&prev_probes.borrow(), &now) {
                return;
            }
            cb();
            // Re-probe after the callback so its own writes don't
            // read as an external change on the next run, and drop
            // the sweep echo our own run just queued.
            *prev_probes.borrow_mut() = probe(scope_id, &tracked);
            if let Some(me) = self_id.get() {
                crate::reactive::dequeue_effect(me);
            }
        }
    });
    self_id.set(Some(id));
    crate::reactive::set_effect_label(id, label);
    id
}

/// Scope-bound counterpart to [`watch`] — installs the watcher and
/// registers a cleanup against the current scope's unmount, so the
/// effect is released automatically when the component goes away.
/// Returns nothing; storage is implicit.
///
/// Same shape as `events::on_scoped` and `timers::after_scoped` —
/// the right default inside lifecycle hooks where the watcher
/// should outlive the install but die with the scope.
///
/// Install is deferred a microtask via [`crate::tick::next`] for the
/// same reason `watch_scope_field_scoped` defers: callers reach
/// this from `on_mount` / `on_ready`, which run behind the mount's
/// active borrow on the scope's state. The watch's first-tick
/// callback typically calls back into the same handle (e.g.
/// `handle.update(...)` to mirror the observed value into local
/// state) — which would trip `RefCell::borrow_mut` against the
/// mount's still-live borrow. Deferring the install lets the
/// surrounding lifecycle frame unwind first.
pub fn watch_scoped<T, S, C>(source: S, cb: C)
where
    T: Clone + PartialEq + 'static,
    S: Fn() -> T + 'static,
    C: Fn(&T, Option<&T>) + 'static,
{
    let owner =
        current_scope_id().expect("watch_scoped called outside a handler / lifecycle context");
    crate::tick::next(move || {
        if Scope::find(owner).is_none() {
            return;
        }
        let id = watch(source, cb);
        // Register after installation. If the initial callback unmounted the
        // owner, `on_scope_unmount_for` releases the effect immediately.
        crate::events::on_scope_unmount_for(owner, move || crate::reactive::release(id));
    });
}

/// Scope-bound counterpart to [`watch_field`].
pub fn watch_field_scoped<V, C>(field: &'static str, cb: C)
where
    V: Clone + PartialEq + Default + DeserializeOwned + 'static,
    C: Fn(&V, Option<&V>) + 'static,
{
    let scope_id = current_scope_id()
        .expect("watch_field_scoped called outside a handler / lifecycle context");
    watch_scope_field_scoped(scope_id, field, cb);
}

/// Scope-bound counterpart to [`watch_scope_field`]. Installs the
/// effect lazily on `tick::next` (same as the unsoped form) and
/// schedules a release against the current scope's unmount once
/// the install fires.
pub fn watch_scope_field_scoped<V, C>(scope_id: ScopeId, field: &'static str, cb: C)
where
    V: Clone + PartialEq + Default + DeserializeOwned + 'static,
    C: Fn(&V, Option<&V>) + 'static,
{
    let owner = current_scope_id()
        .expect("watch_scope_field_scoped called outside a handler / lifecycle context");
    crate::tick::next(move || {
        if Scope::find(owner).is_none() || Scope::find(scope_id).is_none() {
            return;
        }
        let id = watch_scope_field_now(scope_id, field, cb);
        crate::events::on_scope_unmount_for(owner, move || crate::reactive::release(id));
    });
}
