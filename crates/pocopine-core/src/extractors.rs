//! Typed structural and context extractors — RFC 056 §6.5–§6.6.
//!
//! [`Parent<T>`] / [`NearestParent<T>`] express compound-component
//! ownership: the child knows it lives inside a specific root and
//! the extractor surfaces a typed handle without forcing the author
//! to plumb `ScopeId` + `watch_scope_field` by hand. [`Inject<KEY,
//! T>`] is the typed companion to [`crate::context::inject`] —
//! lifecycle hook signatures spell the dependency in their argument
//! list and the carrier resolves it.
//!
//! Each extractor implements `From<LifecycleContext>` so the
//! existing `#[handlers]` extractor pipeline (RFC 032) projects them
//! automatically. Bare extractors panic on a wrong-shape resolution
//! (programming error); `Option<...>` returns `None` for the
//! optional flavour.

use std::any::Any;
use std::marker::PhantomData;
use std::ops::Deref;

use crate::context::{ContextKey, ContextMarker, inject};
use crate::handle::Handle;
use crate::lifecycle::LifecycleContext;
use crate::reactive::{ScopeId, track};
use crate::scope::{Scope, with_current_scope_id};
use crate::watch::watch;

const PARENT_OBSERVE_KEY: &str = "__pp_parent_observe";

/// Typed handle onto the **immediate** parent scope (RFC 056 §6.6).
///
/// Resolves only when the rendered parent scope's Rust type matches
/// `T`. Use this when the relationship is part of the compound's
/// ownership contract — `Trigger` lives directly inside `Root`. If
/// the relationship is semantic instead of structural, prefer
/// [`Inject`].
pub struct Parent<T: 'static> {
    handle: Handle<T>,
}

impl<T: 'static> Parent<T> {
    /// Non-reactive read of the parent's typed state.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.handle.with(f)
    }

    /// Reactive mutation — equivalent to `self.handle().update(f)`.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.handle.update(f)
    }

    /// Subscribe to a derived value of the parent's state. `selector`
    /// runs whenever the parent's scope is triggered (any field
    /// changes); `cb` fires when the selected `V` actually moves.
    ///
    /// Mirrors `watch_field`'s shape — installs lazily on the next
    /// microtask so the caller's borrow has cleared by the time the
    /// initial read runs.
    pub fn observe<V>(
        &self,
        selector: impl Fn(&T) -> V + 'static,
        cb: impl Fn(V, Option<V>) + 'static,
    ) where
        V: Clone + PartialEq + 'static,
    {
        let parent_scope = self.handle.scope_id();
        let handle = self.handle.clone();
        crate::tick::next(move || {
            watch(
                move || {
                    track(parent_scope, PARENT_OBSERVE_KEY);
                    handle.with(|s| selector(s))
                },
                move |new, prev| cb(new.clone(), prev.cloned()),
            );
        });
    }

    /// Drop down to the underlying [`Handle<T>`] for cases where the
    /// extractor's surface isn't enough.
    pub fn handle(&self) -> Handle<T> {
        self.handle.clone()
    }
}

impl<T: 'static> Clone for Parent<T> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
        }
    }
}

/// Typed handle onto the *nearest* ancestor scope of type `T`
/// (RFC 056 §6.6). Walks the scope-parent chain (the same chain
/// `inject` traverses) and returns on the first matching ancestor.
pub struct NearestParent<T: 'static> {
    handle: Handle<T>,
}

impl<T: 'static> NearestParent<T> {
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.handle.with(f)
    }

    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.handle.update(f)
    }

    pub fn observe<V>(
        &self,
        selector: impl Fn(&T) -> V + 'static,
        cb: impl Fn(V, Option<V>) + 'static,
    ) where
        V: Clone + PartialEq + 'static,
    {
        let parent_scope = self.handle.scope_id();
        let handle = self.handle.clone();
        crate::tick::next(move || {
            watch(
                move || {
                    track(parent_scope, PARENT_OBSERVE_KEY);
                    handle.with(|s| selector(s))
                },
                move |new, prev| cb(new.clone(), prev.cloned()),
            );
        });
    }

    pub fn handle(&self) -> Handle<T> {
        self.handle.clone()
    }
}

impl<T: 'static> Clone for NearestParent<T> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
        }
    }
}

/// Resolve `Handle<T>` for the immediate parent scope of `child`. No
/// recursion — `None` when the child has no parent or the parent's
/// type doesn't match.
fn resolve_immediate_parent<T: 'static>(child: ScopeId) -> Option<Handle<T>> {
    let parent_id = crate::context::parent_of(child)?;
    let scope = Scope::find(parent_id)?;
    let rc = scope.typed::<T>()?;
    Some(Handle::new(rc, parent_id))
}

/// Walk ancestors from `child` upwards and return the first
/// `Handle<T>` whose scope matches `T`.
fn resolve_nearest_ancestor<T: 'static>(child: ScopeId) -> Option<Handle<T>> {
    let mut cur = child;
    while let Some(parent_id) = crate::context::parent_of(cur) {
        if let Some(scope) = Scope::find(parent_id)
            && let Some(rc) = scope.typed::<T>()
        {
            return Some(Handle::new(rc, parent_id));
        }
        cur = parent_id;
    }
    None
}

impl<'a, T: 'static> From<LifecycleContext<'a>> for Parent<T> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        let handle = resolve_immediate_parent::<T>(ctx.scope_id).unwrap_or_else(|| {
            panic!(
                "Parent<{}>: immediate parent scope is not of this type",
                std::any::type_name::<T>()
            )
        });
        Parent { handle }
    }
}

impl<'a, T: 'static> From<LifecycleContext<'a>> for Option<Parent<T>> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        resolve_immediate_parent::<T>(ctx.scope_id).map(|handle| Parent { handle })
    }
}

impl<'a, T: 'static> From<LifecycleContext<'a>> for NearestParent<T> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        let handle = resolve_nearest_ancestor::<T>(ctx.scope_id).unwrap_or_else(|| {
            panic!(
                "NearestParent<{}>: no ancestor scope of this type",
                std::any::type_name::<T>()
            )
        });
        NearestParent { handle }
    }
}

impl<'a, T: 'static> From<LifecycleContext<'a>> for Option<NearestParent<T>> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        resolve_nearest_ancestor::<T>(ctx.scope_id).map(|handle| NearestParent { handle })
    }
}

/// Typed extractor for keyed context (RFC 056 §6.5). `KEY` is the
/// marker type generated by [`create_context!`](crate::create_context);
/// `T` is the value the key carries. Derefs to `T` so the extracted
/// value behaves like the underlying handle.
///
/// ```ignore
/// pocopine::create_context!(ROOT: Handle<PineDialogRoot>);
///
/// #[handlers]
/// impl PineDialogTrigger {
///     pub fn on_click(&self, root: Inject<ROOT, Handle<PineDialogRoot>>) {
///         root.update(|dialog| dialog.close());
///     }
/// }
/// ```
pub struct Inject<KEY, T>
where
    KEY: ContextMarker<Value = T>,
    T: Clone + Any + 'static,
{
    value: T,
    _key: PhantomData<fn() -> KEY>,
}

impl<KEY, T> Inject<KEY, T>
where
    KEY: ContextMarker<Value = T>,
    T: Clone + Any + 'static,
{
    /// Consume the extractor and return the underlying value.
    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<KEY, T> Deref for Inject<KEY, T>
where
    KEY: ContextMarker<Value = T>,
    T: Clone + Any + 'static,
{
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<KEY, T> Clone for Inject<KEY, T>
where
    KEY: ContextMarker<Value = T>,
    T: Clone + Any + 'static,
{
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            _key: PhantomData,
        }
    }
}

fn resolve_inject<T: Clone + Any + 'static>(start: ScopeId, key: &ContextKey<T>) -> Option<T> {
    with_current_scope_id(start, || inject(key))
}

impl<'a, KEY, T> From<LifecycleContext<'a>> for Inject<KEY, T>
where
    KEY: ContextMarker<Value = T>,
    T: Clone + Any + 'static,
{
    fn from(ctx: LifecycleContext<'a>) -> Self {
        let value = resolve_inject(ctx.scope_id, KEY::key()).unwrap_or_else(|| {
            panic!(
                "Inject<{}, {}>: key not provided by any ancestor",
                std::any::type_name::<KEY>(),
                std::any::type_name::<T>()
            )
        });
        Inject {
            value,
            _key: PhantomData,
        }
    }
}

impl<'a, KEY, T> From<LifecycleContext<'a>> for Option<Inject<KEY, T>>
where
    KEY: ContextMarker<Value = T>,
    T: Clone + Any + 'static,
{
    fn from(ctx: LifecycleContext<'a>) -> Self {
        resolve_inject(ctx.scope_id, KEY::key()).map(|value| Inject {
            value,
            _key: PhantomData,
        })
    }
}
