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
/// calling `watch_field` while the walker still holds the mutable
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
    let scope_id = current_scope_id()
        .expect("watch_field called outside a handler / lifecycle context");
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
        watch(
            move || {
                track(scope_id, field);
                let Some(scope) = Scope::find(scope_id) else {
                    return V::default();
                };
                let v = scope.state.borrow().get(field);
                serde_wasm_bindgen::from_value::<V>(v).unwrap_or_default()
            },
            cb,
        );
    });
}
