//! RFC-032 — `LifecycleContext` carrier + built-in extractors.
//!
//! `on_mount` and `on_ready` handlers receive typed projections of
//! a shared `LifecycleContext` via stdlib `From`. The walker builds
//! one carrier per hook call; `#[handlers]` inspects the user's
//! method signature and emits one `.into()` per parameter. Each
//! extractor is a plain `impl From<LifecycleContext<'a>> for
//! MyType` — no new trait to learn, orphan rule works naturally
//! for author-defined types, and fallible extractors just return
//! `Option<T>`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, Window};

use crate::handle::Handle;
use crate::reactive::ScopeId;
use crate::scope::Scope;

/// Read-only carrier handed to `on_mount` / `on_ready` by the
/// walker. Authors don't construct it; built-in extractors project
/// from it into typed values (see §4.3 of RFC-032).
///
/// `#[non_exhaustive]` keeps future field additions additive —
/// adding a parent scope id, refs map, or timing info costs
/// nothing at author callsites because extractors read the
/// specific fields they need.
#[non_exhaustive]
#[derive(Clone, Copy)]
pub struct LifecycleContext<'a> {
    /// The component's rendered root element — what `SCOPE_ID_KEY`
    /// is pinned on. Template root on the normal mount path; the
    /// hoisted user element under `pp-as`.
    pub el: &'a Element,
    /// This component's scope id.
    pub scope_id: ScopeId,
}

impl<'a> LifecycleContext<'a> {
    /// Internal constructor — walker mints these in
    /// `fire_mount_hook`; not exposed to downstream because the
    /// type is `#[non_exhaustive]`.
    #[doc(hidden)]
    pub fn __new(el: &'a Element, scope_id: ScopeId) -> Self {
        Self { el, scope_id }
    }
}

// ── Tier 1 — rendered root, scope id, carrier itself ───────────────

/// Rendered root as a thin newtype over `&'a Element`. Newtype
/// (rather than a bare `&'a Element`) so `#[handlers]` has a
/// concrete, nameable type to match against in handler
/// signatures. Derefs to `Element` so authors can call any
/// `Element` method directly.
#[derive(Clone, Copy)]
pub struct El<'a>(pub &'a Element);

impl<'a> std::ops::Deref for El<'a> {
    type Target = Element;
    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<'a> From<LifecycleContext<'a>> for El<'a> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        El(ctx.el)
    }
}

impl<'a> From<LifecycleContext<'a>> for ScopeId {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        ctx.scope_id
    }
}

// `LifecycleContext<'a> -> LifecycleContext<'a>` is covered by
// stdlib's `impl<T> From<T> for T` blanket — no explicit impl here.

// ── Tier 2 — Handle to self, parent id, Window/Document/Body, tag ──

/// Extracting a `Handle<T>` directly is the RFC-032 replacement
/// for `this::<Self>()` inside hooks — write
/// `fn on_ready(&self, handle: Handle<Self>)` and call
/// `handle.update(|s| …)` straight through, no wrapper newtype
/// to unpack.
///
/// Blanket `From` impl: any `T: 'static` that matches this
/// scope's concrete Rust type. Panics if the scope has been
/// evicted or if `T` doesn't match — either indicates a
/// framework bug, not an author bug.
impl<'a, T: 'static> From<LifecycleContext<'a>> for Handle<T> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        let scope = Scope::find(ctx.scope_id)
            .expect("LifecycleContext carried a scope id whose entry no longer exists");
        let rc = scope
            .typed::<T>()
            .expect("Handle<T>: `T` doesn't match this scope's Rust type");
        Handle::new(rc, ctx.scope_id)
    }
}

/// This component's parent scope id (RFC-027 inject chain), or
/// `None` when the component sits at the root of its subtree.
/// Wraps a single `context::parent_of` lookup.
#[derive(Clone, Copy)]
pub struct ParentId(pub Option<ScopeId>);

impl<'a> From<LifecycleContext<'a>> for ParentId {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        ParentId(crate::context::parent_of(ctx.scope_id))
    }
}

/// Shortcut to `web_sys::Document`. Wraps the
/// `window().unwrap().document().unwrap()` pair — fatal in
/// practice if either is missing.
#[derive(Clone)]
pub struct Doc(pub Document);

impl<'a> From<LifecycleContext<'a>> for Doc {
    fn from(_: LifecycleContext<'a>) -> Self {
        Doc(web_sys::window()
            .and_then(|w| w.document())
            .expect("Doc extractor: no document"))
    }
}

/// Shortcut to `web_sys::Window`. Named `Win` to keep it short
/// at handler callsites.
#[derive(Clone)]
pub struct Win(pub Window);

impl<'a> From<LifecycleContext<'a>> for Win {
    fn from(_: LifecycleContext<'a>) -> Self {
        Win(web_sys::window().expect("Win extractor: no window"))
    }
}

/// Document body — useful for teleport-adjacent listener installs.
/// Wraps a `HtmlElement` (already-cast from `Element`) so authors
/// can call `HtmlElement` methods directly.
#[derive(Clone)]
pub struct Body(pub HtmlElement);

impl<'a> From<LifecycleContext<'a>> for Body {
    fn from(_: LifecycleContext<'a>) -> Self {
        Body(web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.body())
            .expect("Body extractor: no document.body"))
    }
}

/// The component's registered kebab-case custom-element tag
/// (`"pine-dialog-root"`) as it appears in the DOM. Reads the
/// tag name directly off the rendered root's parent (the custom
/// element tag) — falls back to the root's own `tagName` if the
/// component is rendered without a host (rare; `pp-as` etc.).
#[derive(Clone, Copy)]
pub struct TagName(pub &'static str);

impl<'a> From<LifecycleContext<'a>> for TagName {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        // Resolve at hook-call time: rendered-root's parent is
        // normally the custom-element tag. Leak the string to get
        // a `'static str` — one per tag-name string, tiny cost,
        // matches `type_name()`'s existing lifetime story.
        let name = ctx
            .el
            .parent_element()
            .map(|p| p.tag_name().to_lowercase())
            .unwrap_or_else(|| ctx.el.tag_name().to_lowercase());
        TagName(Box::leak(name.into_boxed_str()))
    }
}

// ── Tier 3 — Refs, TypedEl, HostEl, IsTeleported ───────────────────

/// Map-like accessor over the component's `pp-ref` entries. Wraps
/// `refs::get_on` + optional typed cast.
///
/// ```ignore
/// fn on_ready(&self, refs: Refs) {
///     if let Some(menu) = refs.get_as::<HtmlUListElement>("menu") {
///         // …
///     }
/// }
/// ```
#[derive(Clone, Copy)]
pub struct Refs<'a> {
    scope_id: ScopeId,
    _m: PhantomData<&'a ()>,
}

impl<'a> Refs<'a> {
    /// Look up a named ref by its `pp-ref="name"` attribute.
    /// Returns `None` if no element has that name stamped on
    /// this component's scope.
    pub fn get(&self, name: &str) -> Option<Element> {
        crate::refs::get_on(self.scope_id, name)
    }

    /// Look up + downcast to a specific `JsCast` type. Returns
    /// `None` on either a missing ref or a failed cast.
    pub fn get_as<T: JsCast>(&self, name: &str) -> Option<T> {
        self.get(name).and_then(|el| el.dyn_into().ok())
    }
}

impl<'a> From<LifecycleContext<'a>> for Refs<'a> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        Refs {
            scope_id: ctx.scope_id,
            _m: PhantomData,
        }
    }
}

/// Rendered root pre-cast via `dyn_into::<T>()`. Panics if the
/// rendered root isn't of the expected type — author's contract,
/// caught during development. Use `Option<TypedEl<T>>` for the
/// fallible form.
///
/// ```ignore
/// fn on_ready(&self, el: TypedEl<HtmlButtonElement>) {
///     let _ = el.0.focus();
/// }
/// ```
pub struct TypedEl<T: JsCast>(pub T);

impl<'a, T: JsCast + 'static> From<LifecycleContext<'a>> for TypedEl<T> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        TypedEl(
            ctx.el
                .clone()
                .dyn_into::<T>()
                .expect("TypedEl<T>: rendered root doesn't cast to T"),
        )
    }
}

impl<'a, T: JsCast + 'static> From<LifecycleContext<'a>> for Option<TypedEl<T>> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        ctx.el.clone().dyn_into::<T>().ok().map(TypedEl)
    }
}

/// The custom-element tag parent of the rendered root — the DOM
/// ancestor whose tag name matches the component's registered
/// name. Useful when events need to dispatch from the tag rather
/// than the template's inner root (`pp-model` listens on the tag,
/// not the template root).
#[derive(Clone)]
pub struct HostEl(pub Element);

impl<'a> From<LifecycleContext<'a>> for HostEl {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        HostEl(ctx.el.parent_element().unwrap_or_else(|| ctx.el.clone()))
    }
}

/// Whether the rendered root lives inside a teleported subtree
/// (an `pp-teleport` clone rehomed to `<body>` or another target).
/// Walks ancestors checking the `__pp_teleport_origin` back-link.
#[derive(Clone, Copy)]
pub struct IsTeleported(pub bool);

impl<'a> From<LifecycleContext<'a>> for IsTeleported {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        IsTeleported(crate::directives::teleport::host_of(ctx.el).is_some())
    }
}

// ── Tier 4 — scope path, teleport host, mount epoch, slots, elapsed

/// Full scope chain from the current scope up to the
/// first-without-parent — useful for devtools or hierarchical
/// lookups that bypass inject.
#[derive(Clone)]
pub struct ScopePath(pub Vec<ScopeId>);

impl<'a> From<LifecycleContext<'a>> for ScopePath {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        let mut out = vec![ctx.scope_id];
        let mut cur = ctx.scope_id;
        while let Some(p) = crate::context::parent_of(cur) {
            out.push(p);
            cur = p;
        }
        ScopePath(out)
    }
}

/// Original host of a teleport — the parent of the
/// `<template pp-teleport>` whose body was cloned. Returns `None`
/// when the rendered root isn't inside a teleported subtree.
#[derive(Clone)]
pub struct TeleportHost(pub Option<Element>);

impl<'a> From<LifecycleContext<'a>> for TeleportHost {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        TeleportHost(crate::directives::teleport::host_of(ctx.el))
    }
}

thread_local! {
    /// Monotonic counter bumped by the walker for each scope's
    /// first hook firing. `MountEpoch` exposes it so authors can
    /// tell a re-walk apart from the original mount.
    static MOUNT_EPOCH_COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static MOUNT_EPOCHS: RefCell<HashMap<ScopeId, u64>> = RefCell::new(HashMap::new());
}

/// Monotonic mount epoch for this scope — increments on each hook
/// firing per scope id. First fire = 0, second fire (e.g. after a
/// keyed `pp-for` resurrection) = 1, etc. Stays stable within a
/// single hook invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MountEpoch(pub u64);

impl<'a> From<LifecycleContext<'a>> for MountEpoch {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        MountEpoch(MOUNT_EPOCHS.with(|m| {
            let mut map = m.borrow_mut();
            *map.entry(ctx.scope_id).or_insert_with(|| {
                MOUNT_EPOCH_COUNTER.with(|c| {
                    let v = c.get();
                    c.set(v + 1);
                    v
                })
            })
        }))
    }
}

/// Clean up a scope's `MountEpoch` entry. Called by
/// `Scope::remove` via the standard per-scope cleanup pass.
#[doc(hidden)]
pub fn __clear_mount_epoch(scope: ScopeId) {
    MOUNT_EPOCHS.with(|m| {
        m.borrow_mut().remove(&scope);
    });
}

/// Snapshot of this component's captured slot content — the
/// fragments the parent passed in named slots, keyed by slot name.
/// Rarely needed directly; templates already handle slot
/// materialisation. Exposed for devtools / advanced cases.
#[derive(Clone)]
pub struct Slots(pub Vec<String>);

impl<'a> From<LifecycleContext<'a>> for Slots {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        Slots(crate::slots::names_for(ctx.scope_id))
    }
}

/// Millisecond timestamp (via `Date::now()`) at the moment the
/// extractor ran. Useful for scope-level timing — pair with
/// `on_unmount` (which takes no ctx) using an author-stored
/// field if you need end-to-end duration. Uses `Date::now` rather
/// than `performance.now` to avoid the web-sys `Performance`
/// feature gate; monotonicity is not guaranteed across system
/// clock changes, good enough for framework-level timing.
#[derive(Clone, Copy)]
pub struct Elapsed(pub f64);

impl<'a> From<LifecycleContext<'a>> for Elapsed {
    fn from(_: LifecycleContext<'a>) -> Self {
        Elapsed(js_sys::Date::now())
    }
}

// Fallibility: authors impl `From<LifecycleContext>` for
// `Option<TheirType>` directly when they want the non-panicking
// variant. No catch_unwind blanket — wasm-hostile and magics too
// much.
