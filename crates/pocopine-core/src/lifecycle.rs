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
use std::collections::{HashMap, HashSet};
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
    pub el: &'a Element,
    /// This component's scope id.
    pub scope_id: ScopeId,
    /// Which lifecycle slot the mount fired this hook from.
    /// Element-dependent extractors guard on this.
    pub phase: LifecyclePhase,
    /// Per-scope hook epoch, minted with the context so repeated
    /// extractions stay stable within one hook invocation.
    mount_epoch: u64,
}

impl<'a> LifecycleContext<'a> {
    /// Internal constructor — mount mints these in `fire_*_hook`;
    /// not exposed to downstream because the type is
    /// `#[non_exhaustive]`.
    #[doc(hidden)]
    pub fn __new(el: &'a Element, scope_id: ScopeId, phase: LifecyclePhase) -> Self {
        Self {
            el,
            scope_id,
            phase,
            mount_epoch: next_mount_epoch(scope_id),
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
/// `handle.defer_update(|s| …)`, no wrapper newtype
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

thread_local! {
    /// Fallback interner for non-component roots (`pp-as`, plain HTML/SVG).
    /// Registered component tags already live in the registry as static
    /// strings, so this table is bounded by the distinct fallback tag names
    /// observed by the page rather than growing once per extraction.
    static INTERNED_TAG_NAMES: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
}

fn intern_tag_name(name: String) -> &'static str {
    // Preserve TagName's original contract (`tag_name().to_lowercase()`),
    // including SVG names whose `local_name()` may retain mixed case.
    let name = name.to_lowercase();
    if let Some(registered) = crate::registry::registered_component_tag(&name) {
        return registered;
    }

    INTERNED_TAG_NAMES.with(|names| {
        let mut names = names.borrow_mut();
        if let Some(&existing) = names.get(name.as_str()) {
            return existing;
        }
        let interned: &'static str = Box::leak(name.into_boxed_str());
        names.insert(interned);
        interned
    })
}

impl<'a> From<LifecycleContext<'a>> for TagName {
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "TagName");
        // Resolve at hook-call time: rendered-root's parent is
        // normally the custom-element tag. Registered tags reuse the
        // registry's static string; fallback roots are interned once
        // per distinct tag rather than leaked once per extraction.
        let name = ctx
            .el
            .parent_element()
            .map(|p| p.local_name())
            .unwrap_or_else(|| ctx.el.local_name());
        TagName(intern_tag_name(name))
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
            ctx.el
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
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "HostEl");
        HostEl(ctx.el.parent_element().unwrap_or_else(|| ctx.el.clone()))
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
    #[track_caller]
    fn from(ctx: LifecycleContext<'a>) -> Self {
        check_phase(ctx.phase, ELEMENT_PHASES, "TeleportHost");
        TeleportHost(crate::directives::teleport::host_of(ctx.el))
    }
}

thread_local! {
    /// Next lifecycle-hook epoch for each live scope. Entries are removed by
    /// `Scope::remove` (and the compiled-row bulk teardown).
    static MOUNT_EPOCHS: RefCell<HashMap<ScopeId, u64>> = RefCell::new(HashMap::new());
}

fn next_mount_epoch(scope: ScopeId) -> u64 {
    MOUNT_EPOCHS.with(|epochs| {
        let mut epochs = epochs.borrow_mut();
        let next = epochs.entry(scope).or_insert(0);
        let current = *next;
        *next = current.saturating_add(1);
        current
    })
}

/// Monotonic mount epoch for this scope — increments on each hook
/// firing per scope id. First fire = 0, second fire (e.g. after a
/// keyed `pp-for` resurrection) = 1, etc. Stays stable within a
/// single hook invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MountEpoch(pub u64);

impl<'a> From<LifecycleContext<'a>> for MountEpoch {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        MountEpoch(ctx.mount_epoch)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_epoch_starts_at_zero_and_advances_per_scope() {
        let first = ScopeId(u64::MAX - 10);
        let second = ScopeId(u64::MAX - 11);
        __clear_mount_epoch(first);
        __clear_mount_epoch(second);

        assert_eq!(next_mount_epoch(first), 0);
        assert_eq!(next_mount_epoch(first), 1);
        assert_eq!(next_mount_epoch(second), 0);

        __clear_mount_epoch(first);
        __clear_mount_epoch(second);
    }

    #[test]
    fn mount_epoch_advances_once_per_context_not_per_extractor() {
        let scope = ScopeId(u64::MAX - 13);
        __clear_mount_epoch(scope);
        let el: Element = wasm_bindgen::JsValue::NULL.unchecked_into();

        let first = LifecycleContext::__new(&el, scope, LifecyclePhase::Setup);
        assert_eq!(MountEpoch::from(first).0, 0);
        assert_eq!(MountEpoch::from(first).0, 0);

        let second = LifecycleContext::__new(&el, scope, LifecyclePhase::Mount);
        assert_eq!(MountEpoch::from(second).0, 1);
        assert_eq!(MountEpoch::from(second).0, 1);

        __clear_mount_epoch(scope);
    }

    #[test]
    fn clearing_mount_epoch_resets_the_scope_generation() {
        let scope = ScopeId(u64::MAX - 12);
        __clear_mount_epoch(scope);

        assert_eq!(next_mount_epoch(scope), 0);
        assert_eq!(next_mount_epoch(scope), 1);
        __clear_mount_epoch(scope);
        assert_eq!(next_mount_epoch(scope), 0);

        __clear_mount_epoch(scope);
    }

    #[test]
    fn tag_name_interner_reuses_one_fallback_allocation() {
        let first = intern_tag_name("Pocopine-Test-Fallback-Tag".to_owned());
        let second = intern_tag_name("pocopine-test-fallback-tag".to_owned());

        assert_eq!(first, "pocopine-test-fallback-tag");
        assert_eq!(first, second);
        assert!(std::ptr::eq(first.as_ptr(), second.as_ptr()));
    }
}

// Fallibility: authors impl `From<LifecycleContext>` for
// `Option<TheirType>` directly when they want the non-panicking
// variant. No catch_unwind blanket — wasm-hostile and magics too
// much.
