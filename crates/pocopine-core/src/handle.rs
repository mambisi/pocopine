//! Typed handles onto component / store scopes.
//!
//! A [`Handle<T>`] is a cheap clone of the `Rc<RefCell<T>>` behind a scope
//! plus the scope's id. Handler code uses it to mutate Rust fields
//! directly (no `JsValue`, no `Reflect::get`) and still have reactivity
//! fire automatically. The same type serves both components (via
//! [`this`]) and stores (via [`crate::store::store`]).
//!
//! Typical use from an async handler:
//!
//! ```ignore
//! pub fn init(&mut self) {
//!     self.loading = true;
//!     let me = pocopine::this::<Self>();
//!     wasm_bindgen_futures::spawn_local(async move {
//!         let post = get_post(1).await;
//!         me.update(|s| {
//!             match post {
//!                 Ok(p)  => { s.title = p.title; s.body = p.body; }
//!                 Err(e) => { s.error = e.to_string(); }
//!             }
//!             s.loading = false;
//!         });
//!     });
//! }
//! ```
//!
//! `update` triggers every effect subscribed to any of the scope's keys
//! when the closure returns — same semantics as a regular handler
//! invocation.

use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

use crate::reactive::{trigger_scope, ScopeId};
use crate::scope::{current_scope_id, invalidate_field_cache, with_current_scope_id, Scope};

/// Typed handle onto a component or store scope.
///
/// Cloneable (cheap: just bumps the internal `Rc`). Use [`Handle::update`]
/// for reactive mutations and [`Handle::with`] for non-reactive reads.
pub struct Handle<T: 'static> {
    inner: Rc<RefCell<T>>,
    scope_id: ScopeId,
}

impl<T: 'static> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Handle {
            inner: self.inner.clone(),
            scope_id: self.scope_id,
        }
    }
}

impl<T: 'static> Handle<T> {
    /// Build a handle from its pieces. Most callers don't need this —
    /// use [`this`] inside a handler, or [`crate::store::store`] for stores.
    pub fn new(inner: Rc<RefCell<T>>, scope_id: ScopeId) -> Self {
        Handle { inner, scope_id }
    }

    /// Non-reactive read. Prefer this over `borrow()` when all you want
    /// is a snapshot.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.inner.borrow())
    }

    /// Mutate the underlying `T`. After `f` returns, every subscriber of
    /// the scope's keys is notified (same as a handler invocation).
    ///
    /// `CURRENT_SCOPE_ID` is bound to this handle's scope for the
    /// duration of `f` so `dispatch!` / `this::<T>()` called from
    /// inside the closure still resolve — even when `update` is
    /// invoked from an async task outside any `Scope::invoke` chain.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let sid = self.scope_id;
        let origin = crate::model_runtime::current_write_origin();
        let total_start = crate::profiler::state_sync::start();
        let closure_start = crate::profiler::state_sync::start();
        let out = crate::model_runtime::with_scope_write(sid, origin, || {
            with_current_scope_id(sid, || f(&mut self.inner.borrow_mut()))
        });
        crate::profiler::state_sync::record_closure(closure_start);
        // RFC 054 phase A — the closure may have mutated arbitrary
        // fields directly through `&mut T`. We don't know which
        // fields were touched, so drop the whole field cache;
        // next proxy reads pick up the fresh state. Targeted
        // `patch_*` ops keep the cache valid.
        let invalidate_start = crate::profiler::state_sync::start();
        invalidate_field_cache(sid);
        crate::profiler::state_sync::record_invalidate(invalidate_start);
        let trigger_start = crate::profiler::state_sync::start();
        trigger_scope(sid);
        crate::profiler::state_sync::record_trigger(trigger_start);
        crate::profiler::state_sync::record_total(total_start);
        out
    }

    /// Lower-level borrow. Does not trigger reactivity.
    pub fn borrow(&self) -> Ref<'_, T> {
        self.inner.borrow()
    }

    /// Lower-level mutable borrow. **Does not trigger reactivity** — use
    /// [`Handle::update`] for that. Reach for this only when you need a
    /// `RefMut` that outlives a single closure.
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        self.inner.borrow_mut()
    }

    pub fn scope_id(&self) -> ScopeId {
        self.scope_id
    }

    /// Watch a string-named field on this handle's scope. The
    /// effect releases automatically when the consumer's scope
    /// (the one calling `watch_field`) unmounts — same lifetime
    /// story as `events::on_scoped` / `timers::after_scoped`.
    ///
    /// Sugar over [`crate::watch::watch_scope_field_scoped`] so
    /// compound primitives can write
    /// `root.watch_field::<bool>("open", cb)` instead of plumbing
    /// the scope id and turbofish manually.
    pub fn watch_field<V, C>(&self, field: &'static str, cb: C)
    where
        V: Clone + PartialEq + Default + serde::de::DeserializeOwned + 'static,
        C: Fn(&V, Option<&V>) + 'static,
    {
        crate::watch::watch_scope_field_scoped::<V, _>(self.scope_id, field, cb);
    }

    /// Subscribe to a derived value of this handle's typed state.
    /// `selector` runs whenever the scope is triggered (any field
    /// change); `cb` fires when the selected `V` actually moves —
    /// the same shape as `Parent<T>::observe` / `NearestParent<T>::observe`.
    ///
    /// Use when `#[watch(field)]` and `watch_field` can't express
    /// the dependency: derived expressions, multi-field reads,
    /// computed projections.
    pub fn observe<V, S, C>(&self, selector: S, cb: C)
    where
        V: Clone + PartialEq + 'static,
        S: Fn(&T) -> V + 'static,
        C: Fn(&V, Option<&V>) + 'static,
    {
        let scope = self.scope_id;
        let me = self.clone();
        crate::watch::watch_scoped(
            move || {
                // Sentinel key — `trigger_scope` fires every key
                // tracked on the scope, so any field change re-runs
                // the selector.
                crate::reactive::track(scope, "__pp_handle_observe");
                me.with(|s| selector(s))
            },
            cb,
        );
    }
}

/// Typed handle onto the component whose handler is currently executing.
///
/// Panics if called outside a handler or with a `T` that doesn't match
/// the scope's concrete struct. `T` should always be the same type the
/// surrounding `impl` block is on.
pub fn this<T: 'static>() -> Handle<T> {
    let id = current_scope_id().expect("pocopine::this called outside a handler invocation");
    let scope = Scope::find(id).expect("current scope missing from registry");
    let inner = scope.typed::<T>().unwrap_or_else(|| {
        panic!(
            "pocopine::this::<{}>() called on a scope of a different type",
            std::any::type_name::<T>()
        )
    });
    Handle::new(inner, id)
}
