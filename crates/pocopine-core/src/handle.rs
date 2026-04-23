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
use crate::scope::{current_scope_id, with_current_scope_id, Scope};

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
        let out = crate::model_runtime::with_scope_write(sid, origin, || {
            with_current_scope_id(sid, || f(&mut self.inner.borrow_mut()))
        });
        trigger_scope(sid);
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
