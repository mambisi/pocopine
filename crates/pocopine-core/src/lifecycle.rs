//! RFC-032 — `LifecycleContext` carrier + built-in extractors.
//!
//! `on_mount` and `on_ready` handlers receive typed projections of
//! a shared `LifecycleContext` via stdlib `From`. The mount builds
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
/// mount. Authors don't construct it; built-in extractors project
/// from it into typed values (see §4.3 of RFC-032).
///
/// `#[non_exhaustive]` keeps future field additions additive —
/// adding a parent scope id, refs map, or timing info costs
/// nothing at author callsites because extractors read the
/// specific fields they need.
/// Which lifecycle slot a [`LifecycleContext`] was minted for. Lets
/// element-dependent extractors panic with a precise message when
/// used in a phase where the rendered element doesn't yet exist
/// (setup) or may already be detaching (unmount).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LifecyclePhase {
    /// Pre-template-walk. `ctx.el` is the custom-element host, not
    /// the rendered template root. Refs aren't registered yet.
    Setup,
    /// Post-template-walk. `ctx.el` is the rendered root. Refs are
    /// fully populated. Full extractor surface available.
    Mount,
    /// One microtask after `Mount`. Same context; the user method
    /// receives `&self` so internal proxy reads don't double-borrow.
    Ready,
    /// Component teardown. `ctx.el` is the element being detached;
    /// refs may already be cleared by the time the body runs.
    Unmount,
}

#[non_exhaustive]
#[derive(Clone, Copy)]
pub struct LifecycleContext<'a> {
    /// The component's rendered root element — what `SCOPE_ID_KEY`
    /// is pinned on. Template root on the normal mount path; the
    /// hoisted user element under `pp-as`. At [`LifecyclePhase::Setup`]
    /// this is the custom-element host (template hasn't been walked).
    ///
    /// `None` only on the RFC-099 host (SSR) setup pass, where there is
    /// no DOM. That's sound because every el-dependent extractor
    /// (`El`/`TagName`/`TypedEl`/`HostEl`/`IsTeleported`/`TeleportHost`)
    /// rejects [`LifecyclePhase::Setup`] via `check_phase` before it ever
    /// reads `el`, so a Setup-phase context never observes the `None`.
    pub el: Option<&'a Element>,
    /// This component's scope id.
    pub scope_id: ScopeId,
    /// Which lifecycle slot the mount fired this hook from.
    /// Element-dependent extractors guard on this.
    pub phase: LifecyclePhase,
}

impl<'a> LifecycleContext<'a> {
    /// Internal constructor — mount mints these in `fire_*_hook`;
    /// not exposed to downstream because the type is
    /// `#[non_exhaustive]`.
    #[doc(hidden)]
    pub fn __new(el: &'a Element, scope_id: ScopeId, phase: LifecyclePhase) -> Self {
        Self {
            el: Some(el),
            scope_id,
            phase,
        }
    }

    /// RFC-099 — el-free Setup context for running `on_setup` host-side
    /// during SSR (no DOM exists). Only el-free extractors
    /// (`Handle`/`Inject`/`ScopeId`/…) resolve; el-dependent ones reject
    /// Setup via `check_phase`, and DOM/`web_sys` ones panic (the SSR
    /// caller runs setup inside `catch_unwind`).
    #[doc(hidden)]
    pub fn __new_detached_setup(scope_id: ScopeId) -> Self {
        Self {
            el: None,
            scope_id,
            phase: LifecyclePhase::Setup,
        }
    }
}

#[track_caller]
fn check_phase(ctx_phase: LifecyclePhase, allowed: &[LifecyclePhase], extractor: &str) {
    if !allowed.contains(&ctx_phase) {
        panic!(
            "{extractor} extractor is not valid in `on_{phase}` (allowed: {allowed:?}). \
             At setup the rendered template hasn't been walked yet; at unmount the element \
             may already be detaching. Reach for Handle / Inject / Parent / NearestParent \
             / ScopeId / Doc / Win / Body — those work in every phase.",
            phase = match ctx_phase {
                LifecyclePhase::Setup => "setup",
                LifecyclePhase::Mount => "mount",
                LifecyclePhase::Ready => "ready",
                LifecyclePhase::Unmount => "unmount",
            },
        );
    }
}

const ELEMENT_PHASES: &[LifecyclePhase] = &[LifecyclePhase::Mount, LifecyclePhase::Ready];

/// `ctx.el` for the el-dependent extractors. They all `check_phase` to
/// the element phases first, and `el` is only `None` on the host SSR
/// Setup pass — so by the time this is reached the element is present.
#[track_caller]
fn require_el<'a>(ctx: &LifecycleContext<'a>) -> &'a Element {
    ctx.el
        .expect("el-dependent extractor with no element (host SSR setup pass?)")
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
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "El");
        El(require_el(&ctx))
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
        Body(
            web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.body())
                .expect("Body extractor: no document.body"),
        )
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
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "TagName");
        // Resolve at hook-call time: rendered-root's parent is
        // normally the custom-element tag. Leak the string to get
        // a `'static str` — one per tag-name string, tiny cost,
        // matches `type_name()`'s existing lifetime story.
        let el = require_el(&ctx);
        let name = el
            .parent_element()
            .map(|p| p.tag_name().to_lowercase())
            .unwrap_or_else(|| el.tag_name().to_lowercase());
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

    /// Look up a child-component handle by `pp-ref="name"`
    /// (RFC 081). Returns `None` when the named ref isn't a
    /// child-component host or the registered child's Rust
    /// type doesn't match `T`. Mirrors the free-fn
    /// [`crate::refs::get_component`].
    pub fn get_component<T: 'static>(&self, name: &str) -> Option<crate::handle::Handle<T>> {
        crate::refs::get_component_on::<T>(self.scope_id, name)
    }
}

impl<'a> From<LifecycleContext<'a>> for Refs<'a> {
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "Refs");
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
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "TypedEl");
        TypedEl(
            require_el(&ctx)
                .clone()
                .dyn_into::<T>()
                .expect("TypedEl<T>: rendered root doesn't cast to T"),
        )
    }
}

impl<'a, T: JsCast + 'static> From<LifecycleContext<'a>> for Option<TypedEl<T>> {
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "Option<TypedEl>");
        require_el(&ctx).clone().dyn_into::<T>().ok().map(TypedEl)
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
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "HostEl");
        let el = require_el(&ctx);
        HostEl(el.parent_element().unwrap_or_else(|| el.clone()))
    }
}

/// Whether the rendered root lives inside a teleported subtree
/// (an `pp-teleport` clone rehomed to `<body>` or another target).
/// Walks ancestors checking the `__pp_teleport_origin` back-link.
#[derive(Clone, Copy)]
pub struct IsTeleported(pub bool);

impl<'a> From<LifecycleContext<'a>> for IsTeleported {
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "IsTeleported");
        IsTeleported(crate::directives::teleport::host_of(require_el(&ctx)).is_some())
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
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "TeleportHost");
        TeleportHost(crate::directives::teleport::host_of(require_el(&ctx)))
    }
}

thread_local! {
    /// Monotonic counter bumped by the mount for each scope's
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

/// Bulk cleanup for compiled-row teardown. Avoids one
/// `thread_local::with` borrow per row during large keyed-list
/// clears.
#[doc(hidden)]
pub fn __clear_mount_epochs(scopes: &[ScopeId]) {
    if scopes.is_empty() {
        return;
    }
    MOUNT_EPOCHS.with(|m| {
        let mut map = m.borrow_mut();
        if map.is_empty() {
            return;
        }
        for scope in scopes {
            map.remove(scope);
        }
    });
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

impl<'a, T: 'static> From<LifecycleContext<'a>> for crate::plugin::Plugin<T> {
    fn from(_: LifecycleContext<'a>) -> Self {
        crate::plugin::required_plugin::<T>()
    }
}

impl<'a, T: 'static> From<LifecycleContext<'a>> for Option<crate::plugin::Plugin<T>> {
    fn from(_: LifecycleContext<'a>) -> Self {
        crate::plugin::active_plugin::<T>()
    }
}

// Fallibility: authors impl `From<LifecycleContext>` for
// `Option<TheirType>` directly when they want the non-panicking
// variant. No catch_unwind blanket — wasm-hostile and magics too
// much.
