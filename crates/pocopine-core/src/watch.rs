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

use std::cell::RefCell;
use std::rc::Rc;

use serde::de::DeserializeOwned;

use crate::reactive::{effect, track, EffectId, ScopeId};
use crate::scope::{current_scope_id, Scope};

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
    track(scope_id, field);
    let Some(scope) = Scope::find(scope_id) else {
        return V::default();
    };
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
    watch(move || read_scope_field::<V>(scope_id, field), cb)
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
    use std::cell::Cell;
    use std::rc::Rc;
    let pending: Rc<Cell<Option<EffectId>>> = Rc::new(Cell::new(None));
    let pending_for_install = pending.clone();
    crate::tick::next(move || {
        let id = watch(source, cb);
        pending_for_install.set(Some(id));
    });
    crate::events::on_scope_unmount(move || {
        if let Some(id) = pending.take() {
            crate::reactive::release(id);
        }
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
    use std::cell::Cell;
    use std::rc::Rc;
    // The install is deferred a tick (same as `watch_scope_field`),
    // so we have to capture the eventually-minted EffectId in a
    // shared cell that the unmount closure can read.
    let pending: Rc<Cell<Option<EffectId>>> = Rc::new(Cell::new(None));
    let pending_for_install = pending.clone();
    crate::tick::next(move || {
        let id = watch_scope_field_now(scope_id, field, cb);
        pending_for_install.set(Some(id));
    });
    crate::events::on_scope_unmount(move || {
        if let Some(id) = pending.take() {
            crate::reactive::release(id);
        }
    });
}
