//! Compiled-view mount runtime hooks.
//!
//! RFC-058 Phase 6.5 retired the runtime mount. The
//! `MutationObserver`, the recursive `pp-*` attribute scan, and
//! the `start` / `start_on_body` entry points are gone. The
//! directive registry has shrunk to the five typed-install
//! opaque directives the compiled plan applier uses
//! (`anchor` / `roving` / `intersect` / `resize` / `flip`).
//! There is no longer any runtime `pp-*` parsing or dispatch
//! loop. **Authoring `pp-*` / `:prop` / `@event` / `pp-text` /
//! `pp-bind` / `pp-show` / `pp-model` directly on arbitrary
//! runtime HTML is no longer a framework feature** — such
//! directives only bind when the macro processes them at
//! compile time inside a `#[component]` template.
//!
//! ## What this module ships now
//!
//! ### 1. Compiled-view mount runtime
//!
//! The surface every macro-emitted template plan calls into:
//!
//! - [`mount_component`], [`mount_child_component`],
//!   [`mount_child_component_with_slots`] — the per-component
//!   mount entry called by macro-emitted child-mount entries.
//! - Scope / proxy stamping helpers used by `pp-for` rows,
//!   `pp-if` bodies, `pp-teleport` portals, and slot fragments.
//! - Lifecycle dispatch helpers shared between `mount_component`
//!   and the plan applier (`fire_mount_post_order`,
//!   `fire_ready_next_tick`, `finalize_compiled_subtree`).
//! - Element-scoped listener and effect side tables so a
//!   subtree teardown can release everything tied to it
//!   (`track_listener_on`, `track_effect_on`, `release_subtree`).
//!
//! Runtime DOM discovery is not part of the compiled-only
//! contract. Custom tags and slot content must appear inside a
//! macro-compiled `#[component]` template so the generated plan
//! can mount children and materialise slots explicitly.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Reflect};
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{DocumentFragment, Element, Event, EventTarget, HtmlTemplateElement, Node};

use crate::reactive::{EffectId, ScopeId, release};
use crate::registry::instantiate;
use crate::scope::{Scope, StaticPropKind};
use crate::slot_scope::SlotScope;
use crate::templates::{is_registered, template_for};

pub(crate) const SCOPE_ID_KEY: &str = "__pp_scope_id";
const SCOPE_PROXY_KEY: &str = "__pp_scope_proxy";
const SCOPE_BORROWED_KEY: &str = "__pp_scope_borrowed";
/// RFC 081 — stamped on a child component's custom-element
/// host (the outer `<keep-note-body>` tag, not the inner
/// template root). Carries the child scope id so a parent's
/// `refs::get_component::<T>("name")` can resolve a typed
/// handle through the parent's `pp-ref="name"` entry. Kept
/// distinct from `SCOPE_ID_KEY` so `fire_mount_hook` /
/// `release_subtree` (which iterate every element with
/// `SCOPE_ID_KEY`) don't see the host twice and double-fire
/// `on_mount` / `on_unmount` / plugin events for the same
/// scope.
const HOST_CHILD_SCOPE_ID_KEY: &str = "__pp_host_child_scope_id";
const EFFECTS_KEY: &str = "__pp_effects";
const LISTENERS_KEY: &str = "__pp_listeners";
const BEFORE_SUBTREE_RELEASE_KEY: &str = "__pp_before_subtree_release";
const WALKED_KEY: &str = "__pp_walked";
const MOUNT_START_MS_KEY: &str = "__pp_mount_start_ms";
const COMPONENT_MOUNT_EVENT_FIRED_KEY: &str = "__pp_component_mount_event_fired";
/// Stamped on row clones whose scope + row-instance state has
/// been torn down synchronously by the RFC 054 bulk-clear path.
/// `release_subtree` checks this first and returns immediately,
/// skipping the per-element side-table sweep that would otherwise
/// pay ~10 `Reflect::get` calls per element across the row's
/// subtree on cleanup. For a 10K-row `clear` this collapses the
/// async cleanup into a no-op.
const RELEASE_SKIP_KEY: &str = "__pp_release_skip";
/// Explicit inject-chain parent for RFC-027. Stamped on
/// slot-materialised elements so their scopes chain to the slot-
/// *owning* component (the one whose template contains the
/// `<slot>`), not the *caller* that authored the slot content.
/// Needed for compound components — e.g. Radix-style DropdownMenu
/// where `<Trigger>` authored inside `<Root>` must inject from
/// `<Root>`, regardless of where the user's enclosing template
/// scope points.
pub(crate) const CTX_PARENT_KEY: &str = "__pp_ctx_parent";
const MOUNT_HOOK_FIRED_KEY: &str = "__pp_mount_hook_fired";
const SLOT_SCOPE_OWNER_KEY: &str = "__pp_slot_scope_owner";

#[derive(Clone)]
struct CapturedSlot {
    source: DocumentFragment,
    ident: String,
    owner_scope_id: ScopeId,
    owner_proxy: JsValue,
}

thread_local! {
    static LIGHT_DOM_SLOTS: RefCell<HashMap<ScopeId, HashMap<String, CapturedSlot>>> =
        RefCell::new(HashMap::new());
    /// Side-table of `ScopeId -> &'static str` populated when at least
    /// one of `ComponentMounted`, `ComponentReady`, or
    /// `ComponentUnmounted` is hooked. Lets plugin events carry a
    /// `&'static str` component name without a per-emit `Reflect::get`
    /// + `as_string` round-trip.
    static COMPONENT_NAMES: RefCell<HashMap<ScopeId, &'static str>> =
        RefCell::new(HashMap::new());
    /// Number of live top-level nodes in each scoped-slot materialization.
    /// The nodes borrow the SlotScope for evaluation, while this count gives
    /// that otherwise-ownerless scope a deterministic teardown point.
    static SLOT_SCOPE_ROOTS: RefCell<HashMap<ScopeId, usize>> =
        RefCell::new(HashMap::new());
}

type BeforeSubtreeRelease = Box<dyn FnOnce()>;

thread_local! {
    /// Hooks owned by one element that must run before `release_subtree`
    /// descends into that element's children. This is intentionally separate
    /// from component `on_unmount`: an owner of opaque child receipts must
    /// release those receipts while the descendant scopes are still live.
    static BEFORE_SUBTREE_RELEASE_NEXT_ID: std::cell::Cell<u64> =
        const { std::cell::Cell::new(1) };
    static BEFORE_SUBTREE_RELEASE: RefCell<HashMap<u64, Vec<BeforeSubtreeRelease>>> =
        RefCell::new(HashMap::new());
}

fn before_subtree_release_slot_for(el: &Element) -> u64 {
    if let Some(value) =
        get_private(el, BEFORE_SUBTREE_RELEASE_KEY).and_then(|value| value.as_f64())
    {
        return value as u64;
    }
    let id = BEFORE_SUBTREE_RELEASE_NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1).max(1));
        id
    });
    set_private(
        el,
        BEFORE_SUBTREE_RELEASE_KEY,
        &JsValue::from_f64(id as f64),
    );
    id
}

/// Run `callback` once immediately before subtree teardown descends into
/// `owner`'s children.
///
/// This is the ownership boundary for systems that mount opaque child
/// subtrees and retain their own receipts. Component `on_unmount` is too late
/// for that job because normal teardown is post-order. Callbacks are removed
/// from the side table before invocation, so recursive teardown is idempotent
/// and no callback-side borrow is held.
pub fn on_before_subtree_release(owner: &Element, callback: impl FnOnce() + 'static) {
    let slot = before_subtree_release_slot_for(owner);
    BEFORE_SUBTREE_RELEASE.with(|hooks| {
        hooks
            .borrow_mut()
            .entry(slot)
            .or_default()
            .push(Box::new(callback));
    });
}

fn run_before_subtree_release(el: &Element) {
    let Some(slot) = get_private(el, BEFORE_SUBTREE_RELEASE_KEY).and_then(|value| value.as_f64())
    else {
        return;
    };
    set_private(el, BEFORE_SUBTREE_RELEASE_KEY, &JsValue::UNDEFINED);
    let callbacks = BEFORE_SUBTREE_RELEASE.with(|hooks| hooks.borrow_mut().remove(&(slot as u64)));
    if let Some(callbacks) = callbacks {
        for callback in callbacks {
            callback();
        }
    }
}

fn bind_slot_scope_owners(scope_id: ScopeId, roots: &[Node]) {
    if roots.is_empty() {
        Scope::remove(scope_id);
        return;
    }
    SLOT_SCOPE_ROOTS.with(|owners| {
        owners.borrow_mut().insert(scope_id, roots.len());
    });
    for root in roots {
        let _ = Reflect::set(
            root.as_ref(),
            &SLOT_SCOPE_OWNER_KEY.into(),
            &JsValue::from_f64(scope_id.0 as f64),
        );
    }
}

fn release_slot_scope_owner(root: &Node) {
    let Some(scope_id) = Reflect::get(root.as_ref(), &SLOT_SCOPE_OWNER_KEY.into())
        .ok()
        .and_then(|value| value.as_f64())
        .map(|value| ScopeId(value as u64))
    else {
        return;
    };
    let _ = Reflect::set(
        root.as_ref(),
        &SLOT_SCOPE_OWNER_KEY.into(),
        &JsValue::UNDEFINED,
    );
    let remove_scope = SLOT_SCOPE_ROOTS.with(|owners| {
        let mut owners = owners.borrow_mut();
        let Some(remaining) = owners.get_mut(&scope_id) else {
            return false;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            owners.remove(&scope_id);
            true
        } else {
            false
        }
    });
    if remove_scope {
        Scope::remove(scope_id);
    }
}

/// Pin a pre-built scope onto an element so [`enclosing_scope`] resolves
/// through it. The element is assumed to **own** this scope — when the
/// element unmounts, `release_subtree` removes the scope from the
/// registry. Used by `pp-for`, which mints a fresh `LoopScope` per item.
pub fn bind_scope_to(el: &Element, scope_id: ScopeId, proxy: &JsValue) {
    set_private(el, SCOPE_ID_KEY, &JsValue::from_f64(scope_id.0 as f64));
    set_private(el, SCOPE_PROXY_KEY, proxy);
}

/// Stamp only the scope id without minting a `js_sys::Proxy`. Used by
/// the RFC 054 compiled-row fast path when the row plan is eligible
/// for proxy elision (every binding is a `FastExpr` so the proxy is
/// never read by the per-row hot path). [`enclosing_scope`] /
/// [`scope_of_element`] lazy-mint the proxy on the rare reads — most
/// commonly a delegated listener firing on user click — so the
/// 10K-row mount path skips ~24K wasm-js bridge ops (`Object::new` ×2
/// + 2 trap closures + `Proxy::new` per row).
pub fn bind_scope_id_only(el: &Element, scope_id: ScopeId) {
    set_private(el, SCOPE_ID_KEY, &JsValue::from_f64(scope_id.0 as f64));
}

/// RFC-094 — mark an element's scope OWNED, overriding a
/// `bind_borrowed_scope_to` stamp left by a body-fragment
/// builder. Owned scopes are removed by `release_subtree`;
/// without clearing the borrowed flag, a per-mount scope (e.g. a
/// pp-let `PayloadScope`) leaks on every remount.
pub(crate) fn mark_scope_owned(el: &Element) {
    set_private(el, SCOPE_BORROWED_KEY, &JsValue::FALSE);
}

/// Read just the scope id without forcing proxy lazy-mint. Used by
/// the compiled-row mount loop in `for_.rs::run_keyed`, which has
/// the proxy-or-None decision already made and shouldn't pay for a
/// proxy fetch it'll throw away.
pub fn scope_id_of_element(el: &Element) -> Option<ScopeId> {
    let id_num = get_private(el, SCOPE_ID_KEY).and_then(|v| v.as_f64())?;
    Some(ScopeId(id_num as u64))
}

/// RFC 081 — when `el` is the custom-element host of a child
/// component, returns the child scope id stamped at mount
/// time by [`mount_component`] /
/// [`try_mount_component_as`]. Returns `None` for non-host
/// elements (plain DOM, template roots — those carry
/// `SCOPE_ID_KEY` and resolve through
/// [`scope_id_of_element`] instead). The two keys are kept
/// distinct so `fire_mount_hook` and `release_subtree` (which
/// iterate every element with `SCOPE_ID_KEY`) never see the
/// host as a duplicate copy of the child's scope.
pub fn host_child_scope_id_of(el: &Element) -> Option<ScopeId> {
    let id_num = get_private(el, HOST_CHILD_SCOPE_ID_KEY).and_then(|v| v.as_f64())?;
    Some(ScopeId(id_num as u64))
}

/// RFC 081 — write the host-child stamp directly. Internal
/// helper used by [`mount_component`] and its `pp-as` sibling,
/// also exposed for tests that simulate a mounted child host
/// without going through the full mount path.
#[doc(hidden)]
pub fn bind_host_child_scope(el: &Element, scope_id: ScopeId) {
    set_private(
        el,
        HOST_CHILD_SCOPE_ID_KEY,
        &JsValue::from_f64(scope_id.0 as f64),
    );
}

/// Pin a **borrowed** scope. Same lookup semantics as `bind_scope_to`,
/// but `release_subtree` will leave the scope alone when this element
/// unmounts — the real owner is elsewhere. Used by `pp-teleport` and
/// the teleport path of `pp-if` to keep the enclosing component's
/// scope reachable from a clone that lives outside the component's
/// subtree.
pub fn bind_borrowed_scope_to(el: &Element, scope_id: ScopeId, proxy: &JsValue) {
    set_private(el, SCOPE_ID_KEY, &JsValue::from_f64(scope_id.0 as f64));
    // RFC-096 — proxy-elided owners pass UNDEFINED; stamping that
    // would make `enclosing_scope` return a junk proxy. Id +
    // borrowed flag alone keep teardown semantics; the lazy-mint
    // path covers any later dynamic need.
    if !proxy.is_undefined() {
        set_private(el, SCOPE_PROXY_KEY, proxy);
    }
    set_private(el, SCOPE_BORROWED_KEY, &JsValue::TRUE);
}

/// Fire the component-level `on_mount` lifecycle hook on elements
/// that own a (non-borrowed) scope. Runs post-order so the handler
/// sees the fully-bound subtree (refs included). Resolves the scope
/// id from the element and dispatches to the public phase helpers.
///
/// `trigger_scope` fires afterwards **only when the component
/// actually defined `on_mount`** — otherwise the hook is a no-op
/// and the sweep would cascade through the subtree for nothing. For
/// recursive component trees (e.g. `<hn-comment>` in a comment
/// thread), a blanket sweep per mount amplifies to O(depth × nodes)
/// effect re-runs during initial render.
fn fire_mount_hook(el: &Element) {
    if get_private(el, MOUNT_HOOK_FIRED_KEY)
        .map(|v| v.is_truthy())
        .unwrap_or(false)
    {
        return;
    }
    let Some(id_num) = get_private(el, SCOPE_ID_KEY).and_then(|v| v.as_f64()) else {
        return;
    };
    let borrowed = get_private(el, SCOPE_BORROWED_KEY)
        .map(|v| v.is_truthy())
        .unwrap_or(false);
    if borrowed {
        return;
    }
    set_private(el, MOUNT_HOOK_FIRED_KEY, &JsValue::TRUE);
    let id = ScopeId(id_num as u64);
    fire_mount_post_order(el, id);
    fire_ready_next_tick(el, id);
}

/// Fire the component-level `on_mount` lifecycle hook on `el`
/// using `scope_id` as the bound scope. Public so that
/// generated mount code (RFC-058 Phase 2+) can invoke it
/// directly without re-discovering the scope through the
/// element's private `SCOPE_ID_KEY`.
///
/// No-ops cleanly when the scope no longer exists or the
/// component didn't declare an `on_mount` hook (skips the
/// `trigger_scope` sweep too — see `fire_mount_hook`).
///
/// `on_mount` mutates `&mut self` directly, so this also
/// invalidates the per-scope `FIELD_CACHE` before triggering
/// subscribers — same pattern as `Scope::invoke`. Without it,
/// post-mount renders pull the pre-mutation cached `JsValue`
/// and the DOM stays at its seeded values.
pub fn fire_mount_post_order(el: &Element, scope_id: ScopeId) {
    let Some(scope) = Scope::find(scope_id) else {
        return;
    };
    let has_mount = scope.state.borrow().has_on_mount();
    if has_mount {
        let _frame = crate::ComponentCallbackFrame::for_scope(scope_id);
        let ctx = crate::lifecycle::LifecycleContext::__new(
            el,
            scope_id,
            crate::lifecycle::LifecyclePhase::Mount,
        );
        crate::scope::with_current_scope_id(scope_id, || {
            scope.state.borrow_mut().mount(ctx);
        });
        crate::scope::invalidate_field_cache(scope_id);
        crate::reactive::trigger_scope(scope_id);
    }
    // Emit the plugin `ComponentMounted` event AFTER the user's
    // `on_mount` body and the post-mount field-cache invalidation
    // / trigger sweep. Two reasons:
    //
    // 1. `duration_ms` should reflect the full user-visible cost
    //    of mounting, including any work the component does in its
    //    own `on_mount` (DB lookups, deferred rendering, focus
    //    side-effects). Emitting first would silently exclude that
    //    work from observability metrics.
    // 2. Observers reading `ComponentMounted` should be able to
    //    assume the component is fully ready — its lifecycle has
    //    actually run, not just "the framework is about to call it".
    //    Plugins that schedule follow-up work for a mounted scope
    //    (analytics tags, timer registration, etc.) need this
    //    ordering to read coherent state.
    fire_component_mounted_plugin_hooks(el, scope_id);
}

/// Schedule the component-level `on_ready` lifecycle hook for
/// `scope_id` to fire on the next microtask after `el` has been
/// fully bound. Public so generated mount code (RFC-058 Phase 2+)
/// can schedule it without rediscovering the scope through the
/// element's private `SCOPE_ID_KEY`.
///
/// RFC-026/029: deferred via `tick::next` so the surrounding
/// frame has unwound and `pp-if` / `pp-teleport` children
/// have had a chance to commit. The hook fires through an
/// **immutable** borrow on `state` — proxy reads inside the
/// callback (`watch_field`, `refs::get_on` touching the proxy,
/// `$event`) require `state.borrow()` on the proxy's `get` trap,
/// which is compatible with other immutable borrows.
///
/// No-ops cleanly when the scope no longer exists at fire time
/// or the component didn't declare an `on_ready` hook.
pub fn fire_ready_next_tick(el: &Element, scope_id: ScopeId) {
    let Some(scope) = Scope::find(scope_id) else {
        return;
    };
    let has_ready = scope.state.borrow().has_on_ready();
    let has_plugin_ready = crate::plugin::has_component_ready_hooks();
    if !has_ready && !has_plugin_ready {
        return;
    }
    let el_owned = el.clone();
    crate::tick::next(move || {
        let Some(scope) = Scope::find(scope_id) else {
            return;
        };
        let _frame = crate::ComponentCallbackFrame::for_scope(scope_id);
        if has_plugin_ready {
            crate::plugin::emit(crate::plugin::ComponentReady {
                component: component_name_for(scope_id),
                scope_id,
            });
        }
        if !has_ready {
            return;
        }
        let ctx = crate::lifecycle::LifecycleContext::__new(
            &el_owned,
            scope_id,
            crate::lifecycle::LifecyclePhase::Ready,
        );
        crate::scope::with_current_scope_id(scope_id, || {
            scope.state.borrow().on_ready(ctx);
        });
    });
}

/// Mount a registered component on `el`:
///  * capture the tag's current children as slot content,
///  * instantiate a fresh scope,
///  * apply static attribute props to the scope,
///  * clone the registered template into `el`,
///  * bind the scope to the template's root and strip its `data-pp-scope-id` marker,
///  * forward fallthrough attrs onto the template root (RFC-010),
///  * apply the registered template plan against the freshly
///    stamped subtree.
fn rollback_component_scope(host: &Element, scope: &Scope, clear_dom: bool) {
    Scope::remove(scope.id);
    if clear_dom {
        host.set_inner_html("");
    }
    clear_component_host_stamps(host);
}

type MountInitializer<'a> = dyn FnMut(&Scope) -> Result<(), crate::app::MountError> + 'a;

fn mount_component_result(
    el: &Element,
    tag: &str,
    supplied_slots: Option<(crate::slot_fragment::SlotSet, ScopeId, JsValue)>,
    mut initializer: Option<&mut MountInitializer<'_>>,
) -> Result<Option<Scope>, crate::app::MountError> {
    let typed_mount = initializer.is_some();
    if get_private(el, "__pp_mounted").is_some() {
        return if typed_mount {
            Err(crate::app::MountError::AlreadyMounted {
                component: tag.to_string(),
            })
        } else {
            Ok(None)
        };
    }

    // RFC-112 — framework mount sentinels share the custom-tag entry emitted
    // by the template compiler, but they are not registered user components.
    // Install their controller directly before registry instantiation.
    if tag == "pp-component" {
        if typed_mount {
            return Err(crate::app::MountError::UnsupportedHostMode {
                component: tag.to_string(),
                mode: "framework-sentinel",
            });
        }
        crate::dynamic_component::install(el);
        set_private(el, "__pp_mounted", &JsValue::TRUE);
        return Ok(None);
    }
    if tag == "pp-outlet" {
        if typed_mount {
            return Err(crate::app::MountError::UnsupportedHostMode {
                component: tag.to_string(),
                mode: "framework-sentinel",
            });
        }
        crate::router::register_outlet(el.clone());
        set_private(el, "__pp_mounted", &JsValue::TRUE);
        return Ok(None);
    }

    // RFC-019 — `pp-as` hoists the user's single child element as
    // the rendered root, discarding the template's wrapper. Only
    // engages when all the structural constraints hold; otherwise
    // falls through to the normal mount path.
    if el.has_attribute("pp-as") {
        if typed_mount {
            return Err(crate::app::MountError::UnsupportedHostMode {
                component: tag.to_string(),
                mode: "pp-as",
            });
        }
        if try_mount_component_as(el, tag) {
            return Ok(None);
        }
    }
    let plugin_hooks = crate::plugin::component_hook_activity();
    let mount_start_ms = plugin_hooks.needs_mount_start.then(js_sys::Date::now);

    let Some(scope) = instantiate(tag) else {
        return Err(crate::app::MountError::ConstructionFailed {
            component: tag.to_string(),
        });
    };
    // Record the parent scope for RFC-027 `inject` chain-walks.
    // Prefer the explicit `CTX_PARENT_KEY` stamp — set by slot
    // materialisation on slot-inserted elements so compound-
    // component children chain to the slot *owner* (the component
    // whose template contained the `<slot>`), not the caller that
    // authored the content. Falls back to the DOM ancestry via
    // `enclosing_inject_parent`, which in turn prefers an ancestor's
    // `CTX_PARENT_KEY` over its `SCOPE_ID_KEY` — required for tags
    // nested *inside* a slot wrapper (e.g. `<pine-dialog-close>`
    // inside a `<div class="row">` inside Content's slot).
    let ctx_parent = get_private(el, CTX_PARENT_KEY)
        .and_then(|v| v.as_f64())
        .map(|n| ScopeId(n as u64))
        .or_else(|| enclosing_inject_parent(el));
    if let Some(parent_id) = ctx_parent {
        crate::context::set_parent(scope.id, parent_id);
    }
    // Apply static props BEFORE building the proxy so trigger doesn't fire
    // before any effect subscribes.
    apply_static_props(el, &scope);

    // RFC-113 N1 — typed initializer seam. This runs with the new scope
    // current after static props, but before plugin/user setup. The closure
    // performs the concrete-state downcast and drops its `RefMut` before this
    // function continues into lifecycle hooks.
    if let Some(initialize) = initializer.as_mut() {
        let _frame = crate::ComponentCallbackFrame::for_scope(scope.id);
        if let Err(error) = crate::scope::with_current_scope_id(scope.id, || initialize(&scope)) {
            rollback_component_scope(el, &scope, false);
            return Err(error);
        }
    }

    fire_component_setup_plugin_hooks(tag, scope.id);
    // RFC-030: fire `on_setup` — the component's pre-children-walk
    // hook where fields can be initialised from injected context.
    // Runs with CURRENT_SCOPE_ID bound so `inject` / `this` resolve.
    if scope.state.borrow().has_setup() {
        let _frame = crate::ComponentCallbackFrame::for_scope(scope.id);
        let setup_ctx = crate::lifecycle::LifecycleContext::__new(
            el,
            scope.id,
            crate::lifecycle::LifecyclePhase::Setup,
        );
        crate::scope::with_current_scope_id(scope.id, || {
            crate::model_runtime::with_scope_write(
                scope.id,
                crate::model_runtime::WriteOrigin::SetupSeed,
                || scope.state.borrow_mut().setup(setup_ctx),
            );
        });
    }
    // RFC-095 W3b — plan-gated lazy proxy. When the compiled plan
    // proves nothing consults the proxy (bindings/interps/refs
    // only, all `$`-free), skip `into_proxy()` — no trap
    // closures, no `Proxy` per instance. The install body then
    // receives `UNDEFINED`, which the W1 scoped evaluators never
    // touch. Anything dynamic later (devtools, a parent prop
    // write) lazy-mints via `scope_of_element` /
    // `enclosing_scope`, the RFC-054 row contract. Unplanned
    // components conservatively keep the eager mint.
    let needs_proxy = crate::templates_plan::template_plan_for(tag)
        .map(|p| p.needs_proxy)
        .unwrap_or(true);
    let proxy = if needs_proxy {
        scope.into_proxy()
    } else {
        JsValue::UNDEFINED
    };
    crate::model_runtime::capture_emit_el(scope.id, el);

    // A proxy-free plan has no `<slot>` outlets (slot outlets flip
    // `needs_proxy`), so there is no light-DOM content to capture.
    if needs_proxy {
        let light_dom_slots = capture_light_dom_slots(el, scope.id, &proxy);
        if !light_dom_slots.is_empty() {
            LIGHT_DOM_SLOTS.with(|stores| {
                stores.borrow_mut().insert(scope.id, light_dom_slots);
            });
        }
    }
    if let Some((slots, parent_scope_id, parent_proxy)) = supplied_slots {
        crate::slot_fragment::install(scope.id, slots, parent_scope_id, parent_proxy);
    }

    // Clone the registered template in. `set_inner_html` drops the
    // tag's former children, which is the "capture" side of the old
    // flow. Prefer `template_clone_for` (parses the HTML once into a
    // cached `<template>` element, every mount clones the `.content`
    // `DocumentFragment`) over re-parsing the HTML string per mount.
    let Some(fragment) = crate::templates::template_clone_for(tag) else {
        rollback_component_scope(el, &scope, false);
        return Err(crate::app::MountError::TemplateMissing {
            component: tag.to_string(),
        });
    };
    el.set_inner_html("");
    if el.append_child(fragment.as_ref()).is_err() {
        rollback_component_scope(el, &scope, true);
        return Err(crate::app::MountError::DomOperation {
            component: tag.to_string(),
            operation: "append-template",
        });
    }

    // Bind scope to the template's root element and strip
    // data-pp-scope-id so nothing later tries to re-instantiate it.
    let Some(root) = first_element_child(el) else {
        rollback_component_scope(el, &scope, true);
        return Err(crate::app::MountError::TemplateRootMissing {
            component: tag.to_string(),
        });
    };
    {
        set_private(&root, SCOPE_ID_KEY, &JsValue::from_f64(scope.id.0 as f64));
        if needs_proxy {
            set_private(&root, SCOPE_PROXY_KEY, &proxy);
        }
        stamp_plugin_metadata(&root, tag, scope.id, plugin_hooks, mount_start_ms);
        let _ = root.remove_attribute("data-pp-scope-id");

        // RFC 081 — stamp the *custom-element host* with a
        // separate `HOST_CHILD_SCOPE_ID_KEY` so a parent's
        // `pp-ref="name"` on this tag can resolve a typed
        // [`Handle<Child>`](crate::handle::Handle) via
        // [`crate::refs::get_component`]. Distinct from
        // `SCOPE_ID_KEY` (which `fire_mount_hook` and
        // `release_subtree` iterate) so the host doesn't
        // double-fire the child's lifecycle hooks.
        set_private(
            el,
            HOST_CHILD_SCOPE_ID_KEY,
            &JsValue::from_f64(scope.id.0 as f64),
        );

        // Fallthrough (RFC-010).
        apply_fallthrough_attrs(el, &root, &scope);

        // RFC-038 — if the component declared default transition
        // presets via `#[component(transition = "…")]` (or the
        // asymmetric `transition_in` / `transition_out` split),
        // stamp them on the INNER rendered root (`root`) rather
        // than the outer custom tag (`el`). The custom tag often
        // carries `display: contents` (tags-input-item, combobox
        // items, command items, etc.), and opacity/transform on a
        // box-less element don't visually apply.
        let (tr_in, tr_out, ak) = {
            let s = scope.state.borrow();
            (
                s.transition_in_preset(),
                s.transition_out_preset(),
                s.animate_kind(),
            )
        };
        if !tr_in.is_empty() || !tr_out.is_empty() {
            let effective_in = if tr_in.is_empty() { "none" } else { tr_in };
            let effective_out = if tr_out.is_empty() { "none" } else { tr_out };
            let already_set = has_user_transition_attr(&root) || has_user_transition_attr(el);
            if !already_set {
                crate::animate::apply_preset(&root, effective_in, effective_out);
            }
        }
        // Stamp `data-pp-animate="<kind>"` on the outer custom tag
        // so pp-for's keyed reconcile can cheaply check whether to
        // FLIP each reused clone without walking the scope tree.
        if !ak.is_empty() {
            let _ = el.set_attribute("data-pp-animate", ak);
        }

        // RFC 062 — dispatch through the per-component compiled
        // mount body. StaticTemplatePlan is no longer a component
        // mount fallback; it remains only as macro/runtime IR for
        // lifted fragments.
        if let Some(mount_template) = crate::registry::mount_template_for(tag) {
            mount_template(&root, scope.id, &proxy);
        } else if typed_mount {
            release_compiled_subtree(el);
            el.set_inner_html("");
            clear_component_host_stamps(el);
            return Err(crate::app::MountError::MissingMountMetadata {
                component: tag.to_string(),
            });
        }
    }

    // Mark the tag as mounted so duplicate discovery (e.g. an outer
    // compiled root discovery after a parent already mounted it
    // via `child_mounts`) short-circuits.
    set_private(el, "__pp_mounted", &JsValue::TRUE);
    Ok(Some(scope))
}

fn mount_component(
    el: &Element,
    tag: &str,
    supplied_slots: Option<(crate::slot_fragment::SlotSet, ScopeId, JsValue)>,
) {
    // The legacy/name-driven mount surface intentionally preserves its
    // fire-and-forget behavior. The typed owned path below consumes the same
    // structured result instead of silently accepting a partial mount.
    let _ = mount_component_result(el, tag, supplied_slots, None);
}

/// Typed owned-mount entry used by [`crate::app::App::mount_subtree_with`].
///
/// The registry constructor remains the source of the scope, then this seam
/// verifies its concrete state before exposing `&mut C` to the initializer.
/// The borrow is always dropped before `mount_component_result` proceeds into
/// `on_setup`, template mounting, or any other lifecycle callback.
pub(crate) fn mount_typed_component<C, F>(
    host: &Element,
    initialize: F,
) -> Result<crate::handle::Handle<C>, crate::app::MountError>
where
    C: crate::app::MountableComponent,
    F: FnOnce(&mut C, &mut crate::app::MountSetup) -> Result<(), crate::app::MountInitError>,
{
    let mut typed_state: Option<Rc<RefCell<C>>> = None;
    let scope = {
        let mut initialize = Some(initialize);
        let mut erased = |scope: &Scope| -> Result<(), crate::app::MountError> {
            let actual = scope
                .state
                .try_borrow()
                .map(|state| state.type_name())
                .unwrap_or("<borrowed>");
            let typed =
                scope
                    .typed::<C>()
                    .ok_or_else(|| crate::app::MountError::StateTypeMismatch {
                        component: C::NAME.to_string(),
                        expected: std::any::type_name::<C>(),
                        actual,
                    })?;
            let initializer = initialize
                .take()
                .expect("typed mount initializer called more than once");
            let mut setup = crate::app::MountSetup::new(scope.id);
            let result = crate::model_runtime::with_scope_write(
                scope.id,
                crate::model_runtime::WriteOrigin::SetupSeed,
                || {
                    let mut state = typed.try_borrow_mut().map_err(|_| {
                        crate::app::MountError::StateAlreadyBorrowed {
                            component: C::NAME.to_string(),
                        }
                    })?;
                    initializer(&mut state, &mut setup).map_err(|source| {
                        crate::app::MountError::Initialization {
                            component: C::NAME.to_string(),
                            source,
                        }
                    })
                },
            );
            if result.is_ok() {
                typed_state = Some(typed);
            }
            result
        };

        mount_component_result(host, C::NAME, None, Some(&mut erased))?.ok_or_else(|| {
            crate::app::MountError::ConstructionFailed {
                component: C::NAME.to_string(),
            }
        })?
    };
    let typed = typed_state.ok_or_else(|| crate::app::MountError::StateTypeMismatch {
        component: C::NAME.to_string(),
        expected: std::any::type_name::<C>(),
        actual: scope
            .state
            .try_borrow()
            .map(|state| state.type_name())
            .unwrap_or("<borrowed>"),
    })?;
    Ok(crate::handle::Handle::new(typed, scope.id))
}

/// Mount the registered component named `name` onto `host_el`.
/// Public façade over `mount_component` for the macro-emitted
/// child-mount path.
pub fn mount_child_component(host_el: &Element, name: &str) {
    mount_component(host_el, name, None);
}

/// Variant of [`mount_child_component`] that also registers the
/// parent-supplied [`crate::slot_fragment::SlotSet`] against the
/// freshly-created child's scope before the child's template plan
/// runs. That lets compiled `<slot>` outlets pick up parent-authored
/// slot content from the fragment registry.
///
/// `parent_scope_id` + `parent_proxy` get stored alongside the set
/// so dynamic slot content (slot subtrees with `pp-text` / `@click`
/// / `pp-bind` etc.) can install bindings against the parent scope
/// when the fragment fires.
pub fn mount_child_component_with_slots(
    host_el: &Element,
    name: &str,
    slots: crate::slot_fragment::SlotSet,
    parent_scope_id: ScopeId,
    parent_proxy: &JsValue,
) {
    if slots.is_empty() {
        mount_component(host_el, name, None);
        return;
    }
    mount_component(
        host_el,
        name,
        Some((slots, parent_scope_id, parent_proxy.clone())),
    );
}

/// Attempt to mount `tag` on `el` in `pp-as` mode: hoist the tag's
/// single child element as the rendered root, merging the template
/// root's attributes onto it.
///
/// Returns `true` on success. Returns `false` when structural
/// constraints fail (not exactly one user element child, or the
/// template root isn't a simple `<tag><slot></slot></tag>` wrapper)
/// — caller falls back to the normal mount path.
fn try_mount_component_as(el: &Element, tag: &str) -> bool {
    let plugin_hooks = crate::plugin::component_hook_activity();
    let mount_start_ms = plugin_hooks.needs_mount_start.then(js_sys::Date::now);
    let user_root = match find_single_child_element_skipping_slot_templates(el) {
        Some(e) => e,
        None => {
            web_sys::console::warn_1(&JsValue::from_str(
                "pocopine: pp-as requires exactly one child element; ignoring",
            ));
            return false;
        }
    };

    let Some(scope) = instantiate(tag) else {
        return false;
    };
    let ctx_parent = get_private(el, CTX_PARENT_KEY)
        .and_then(|v| v.as_f64())
        .map(|n| ScopeId(n as u64))
        .or_else(|| enclosing_inject_parent(el));
    if let Some(parent_id) = ctx_parent {
        crate::context::set_parent(scope.id, parent_id);
    }
    apply_static_props(el, &scope);
    fire_component_setup_plugin_hooks(tag, scope.id);
    if scope.state.borrow().has_setup() {
        let _frame = crate::ComponentCallbackFrame::for_scope(scope.id);
        let setup_ctx = crate::lifecycle::LifecycleContext::__new(
            el,
            scope.id,
            crate::lifecycle::LifecyclePhase::Setup,
        );
        crate::scope::with_current_scope_id(scope.id, || {
            crate::model_runtime::with_scope_write(
                scope.id,
                crate::model_runtime::WriteOrigin::SetupSeed,
                || scope.state.borrow_mut().setup(setup_ctx),
            );
        });
    }
    // RFC-096 S4 — same plan-gated lazy proxy as the normal mount
    // path: a proxy-free pp-as plan never mints.
    let needs_proxy = crate::templates_plan::template_plan_for(tag)
        .map(|p| p.needs_proxy)
        .unwrap_or(true);
    let proxy = if needs_proxy {
        scope.into_proxy()
    } else {
        JsValue::UNDEFINED
    };
    crate::model_runtime::capture_emit_el(scope.id, el);

    let Some(html) = template_for(tag) else {
        return false;
    };
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return false;
    };
    let sandbox = match doc.create_element("div") {
        Ok(e) => e,
        Err(_) => return false,
    };
    sandbox.set_inner_html(&html);
    let tpl_root = match first_element_child(&sandbox) {
        Some(r) => r,
        None => return false,
    };

    if !is_trivial_slot_wrapper(&tpl_root) {
        web_sys::console::warn_1(&JsValue::from_str(
            "pocopine: pp-as only supports trivial <slot>-wrapping templates; ignoring",
        ));
        return false;
    }

    el.set_inner_html("");
    if el.append_child(user_root.as_ref()).is_err() {
        return false;
    }

    set_private(
        &user_root,
        SCOPE_ID_KEY,
        &JsValue::from_f64(scope.id.0 as f64),
    );
    set_private(&user_root, SCOPE_PROXY_KEY, &proxy);
    stamp_plugin_metadata(&user_root, tag, scope.id, plugin_hooks, mount_start_ms);
    let _ = user_root.remove_attribute("data-pp-scope-id");

    // RFC 081 — same host stamp as the normal mount path
    // (see `HOST_CHILD_SCOPE_ID_KEY` doc). Distinct from
    // `SCOPE_ID_KEY` so lifecycle dispatch / teardown don't
    // visit the host as if it were a second copy of the
    // child's scope.
    set_private(
        el,
        HOST_CHILD_SCOPE_ID_KEY,
        &JsValue::from_f64(scope.id.0 as f64),
    );

    let plan_root = pp_as_render_root(&user_root);

    merge_template_attrs_as(&tpl_root, &plan_root);

    apply_fallthrough_attrs(el, &plan_root, &scope);

    if let Some(plan) = crate::templates_plan::template_plan_for(tag) {
        crate::templates_plan::apply_static_pp_as_plan(&plan_root, scope.id, &proxy, plan, tag);
    }

    let _ = el.remove_attribute("pp-as");
    set_private(el, "__pp_mounted", &JsValue::TRUE);

    true
}

fn pp_as_render_root(user_root: &Element) -> Element {
    let tag = user_root.local_name();
    if is_registered(&tag) {
        // Compose one registered child layer: the outer pp-as scope
        // still drives the template plan explicitly below, while the
        // child component owns its rendered root. Do not recurse here;
        // nested component composition should be authored as another
        // explicit pp-as boundary.
        mount_component(user_root, &tag, None);
        if let Some(rendered_root) = first_element_child(user_root) {
            return rendered_root;
        }
    }
    user_root.clone()
}

/// Walk the tag's direct children. Return `Some(el)` when exactly
/// one non-slot-template element is present among them. Named-slot
/// `<template pp-slot="…">` children are silently skipped — they
/// don't compose with `pp-as`.
fn find_single_child_element_skipping_slot_templates(tag: &Element) -> Option<Element> {
    let children = tag.child_nodes();
    let mut found: Option<Element> = None;
    for i in 0..children.length() {
        let Some(node) = children.item(i) else {
            continue;
        };
        let Ok(el) = node.dyn_into::<Element>() else {
            continue;
        };
        if let Some(tpl) = el.dyn_ref::<HtmlTemplateElement>()
            && tpl.has_attribute("pp-slot")
        {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(el);
    }
    found
}

/// Template root is a trivial wrapper iff its only element child
/// is a single `<slot>`. Text / comment siblings are ignored.
fn is_trivial_slot_wrapper(tpl_root: &Element) -> bool {
    let children = tpl_root.children();
    if children.length() != 1 {
        return false;
    }
    match children.item(0) {
        Some(c) => c.local_name() == "slot",
        None => false,
    }
}

/// Copy attrs from `tpl_root` onto `user_root` per RFC-019 §4.
/// `class` / `style` join; everything else writes only when absent
/// on the user element (user wins on conflict). Internal markers
/// (`data-pp-scope-id`, `pp-as`) are dropped.
fn merge_template_attrs_as(tpl_root: &Element, user_root: &Element) {
    let attrs = tpl_root.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if name == "data-pp-scope-id" || name == "pp-as" {
            continue;
        }
        let val = a.value();
        let setter_name = setattr_safe_name(&name);
        match name.as_str() {
            "class" => {
                let existing = user_root.get_attribute("class").unwrap_or_default();
                let merged = merge_space(&existing, &val);
                let _ = user_root.set_attribute("class", &merged);
            }
            "style" => {
                let existing = user_root.get_attribute("style").unwrap_or_default();
                let merged = merge_semicolon(&existing, &val);
                let _ = user_root.set_attribute("style", &merged);
            }
            _ => {
                if !user_root.has_attribute(&setter_name) {
                    let _ = user_root.set_attribute(&setter_name, &val);
                }
            }
        }
    }
}

fn has_user_transition_attr(el: &Element) -> bool {
    let attrs = el.attributes();
    for i in 0..attrs.length() {
        let Some(attr) = attrs.item(i) else { continue };
        match attr.name().as_str() {
            "pp-transition"
            | "pp-transition:enter"
            | "pp-transition:enter-start"
            | "pp-transition:enter-end"
            | "pp-transition:leave"
            | "pp-transition:leave-start"
            | "pp-transition:leave-end"
            | "pp-transition:in"
            | "pp-transition:out" => return true,
            _ => {}
        }
    }
    false
}

/// `setAttribute` rejects names whose first character isn't a
/// Name-start (per the XML Name production the DOM standard cites).
/// `:foo` is allowed but `@foo` isn't. Convert RFC-020 `@event`
/// shorthand to `pp-on:event` long form so the call goes through
/// cleanly. Other names pass through unchanged.
fn setattr_safe_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix('@')
        && !rest.is_empty()
    {
        return format!("pp-on:{rest}");
    }
    name.to_string()
}

fn apply_fallthrough_attrs(tag: &Element, root: &Element, scope: &Scope) {
    use std::collections::HashSet;

    let declared: HashSet<String> = scope
        .state
        .borrow()
        .keys()
        .iter()
        .map(|k| (*k).to_string())
        .collect();

    let attrs = tag.attributes();
    let mut strip_class = false;
    let mut strip_style = false;
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if name.starts_with("pp-") || name.starts_with("__pp_") {
            continue;
        }
        // RFC-020 shorthand (`@event` / `:attr`) is a directive,
        // not a plain attribute. Skip — the macro lifts these into
        // the plan; fallthrough would clobber the template's own
        // listener bound in the parent's scope.
        if name.starts_with('@') || name.starts_with(':') {
            continue;
        }
        let field = normalize_prop_name(&name);
        if declared.contains(&field) {
            continue;
        }
        let val = a.value();
        match name.as_str() {
            "class" => {
                let existing = root.get_attribute("class").unwrap_or_default();
                let merged = merge_space(&existing, &val);
                let _ = root.set_attribute("class", &merged);
                strip_class = true;
            }
            "style" => {
                let existing = root.get_attribute("style").unwrap_or_default();
                let merged = merge_semicolon(&existing, &val);
                let _ = root.set_attribute("style", &merged);
                strip_style = true;
            }
            _ => {
                let _ = root.set_attribute(&name, &val);
            }
        }
    }
    if strip_class {
        let _ = tag.remove_attribute("class");
    }
    if strip_style {
        let _ = tag.remove_attribute("style");
    }
}

/// Local copy of the kebab→snake mapping the directive registry
/// used to expose. Walker removal eliminated the public helper;
/// `apply_static_props` and the fallthrough path are the only
/// remaining callers, so the mapping lives here.
fn normalize_prop_name(name: &str) -> String {
    name.replace('-', "_")
}

fn merge_space(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a} {b}"),
    }
}

fn merge_semicolon(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => {
            let trimmed = a.trim_end_matches(|c: char| c.is_whitespace() || c == ';');
            format!("{trimmed}; {b}")
        }
    }
}

fn apply_static_props(el: &Element, scope: &Scope) {
    let attrs = el.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if name.starts_with("pp-") || name.starts_with("__pp_") {
            continue;
        }
        let field = normalize_prop_name(&name);
        let prop_kind = {
            let state = scope.state.borrow();
            if !state.is_prop(&field) {
                continue;
            }
            state.static_prop_kind(&field)
        };
        let raw = a.value();
        let js = coerce_static_attr_value(&raw, prop_kind);
        crate::model_runtime::with_scope_write(
            scope.id,
            crate::model_runtime::WriteOrigin::SetupSeed,
            || scope.state.borrow_mut().set(&field, js),
        );
    }
}

fn coerce_static_attr_value(raw: &str, kind: StaticPropKind) -> JsValue {
    match kind {
        StaticPropKind::String => JsValue::from_str(raw),
        StaticPropKind::Auto | StaticPropKind::Bool | StaticPropKind::Number => {
            coerce_attr_value(raw)
        }
    }
}

fn coerce_attr_value(raw: &str) -> JsValue {
    if raw.is_empty() {
        return JsValue::TRUE;
    }
    if raw == "true" {
        return JsValue::TRUE;
    }
    if raw == "false" {
        return JsValue::FALSE;
    }
    let trimmed = raw.trim_start();
    let first = trimmed.as_bytes().first();
    if matches!(first, Some(b'{') | Some(b'[') | Some(b'"'))
        && let Ok(v) = js_sys::JSON::parse(raw)
    {
        return v;
    }
    if let Ok(n) = raw.parse::<f64>() {
        return JsValue::from_f64(n);
    }
    JsValue::from_str(raw)
}

fn first_element_child(el: &Element) -> Option<Element> {
    let children = el.children();
    children.item(0)
}

/// Compiled template-plan entry point for `<slot>` outlets.
pub(crate) fn materialize_compiled_slot_outlet(slot_el: &Element) {
    materialize_slot(slot_el);
}

/// Replace a `<slot>` element in a component template with the
/// matching user-provided content (from the parent-supplied
/// fragment registry) or the slot's own default children. Per
/// RFC-011 §5.2.
fn materialize_slot(slot_el: &Element) {
    let Some(parent) = slot_el.parent_node() else {
        return;
    };

    let slot_name = slot_el
        .get_attribute("name")
        .unwrap_or_else(|| "default".into());

    // Collect `:prop="path"` bindings.
    let mut bindings: Vec<(String, String)> = Vec::new();
    let attrs = slot_el.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if let Some(prop) = name.strip_prefix(':') {
            bindings.push((prop.to_string(), a.value()));
        }
    }

    // Resolve the enclosing scope. This is the component (or pp-for
    // loop) whose template contains the <slot>.
    let (owner_scope_id, owner_proxy) = match enclosing_scope(slot_el) {
        Some(s) => s,
        None => {
            // No owner anywhere up the tree: the outlet can't be projected
            // into, so it is dropped — observably, not silently. A
            // mis-threaded fragment (e.g. a nested outlet materialised
            // after losing its author-scope stamp) shows up here instead
            // of as vanishing content.
            web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(
                "pocopine: <slot> outlet has no enclosing scope — dropping it \
                 (its projected content will not render)",
            ));
            crate::templates_plan::record_plan_failure();
            let _ = parent.remove_child(slot_el);
            return;
        }
    };

    // RFC-058 Phase 3.5a/3.5g — parent-supplied fragment lookup.
    // For plain default + named slots (entry.scoped_let is None) the
    // fragment runs against the parent proxy directly, which requires
    // the slot to have no `:prop` bindings (those are an RFC-011
    // scoped-slot affordance, only meaningful with pp-let). For
    // scoped slots we build a [`SlotScope`] from the child's `<slot>`
    // `:prop` bindings and invoke the fragment against the slot
    // scope's proxy.
    let Some((entry, parent_scope_id, parent_proxy)) =
        crate::slot_fragment::lookup(owner_scope_id, &slot_name)
    else {
        if materialize_captured_light_dom_slot(
            slot_el,
            &parent,
            &slot_name,
            &bindings,
            owner_scope_id,
        ) {
            return;
        }
        materialize_slot_default(slot_el, &parent, &owner_scope_id, &owner_proxy);
        return;
    };
    let take_fast_path = match entry.scoped_let {
        None => bindings.is_empty(),
        Some(_) => true,
    };
    if !take_fast_path {
        materialize_slot_default(slot_el, &parent, &owner_scope_id, &owner_proxy);
        return;
    }
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let (fragment_parent_scope_id, fragment_parent_proxy, slot_scope_for_pin) =
        match entry.scoped_let {
            Some(let_ident) => {
                let slot_state = SlotScope {
                    ident: let_ident.to_string(),
                    bindings: bindings.clone(),
                    bind_source_scope_id: owner_scope_id,
                    caller_scope_id: parent_scope_id,
                };
                let slot_scope = Scope::new(Rc::new(RefCell::new(slot_state)));
                crate::context::set_parent(slot_scope.id, owner_scope_id);
                let proxy = slot_scope.into_proxy();
                (slot_scope.id, proxy, Some(slot_scope.id))
            }
            None => (parent_scope_id, parent_proxy.clone(), None),
        };
    let buffer = doc.create_document_fragment();
    (entry.fragment)(crate::slot_fragment::SlotMountCtx {
        host: &buffer,
        parent_scope_id: fragment_parent_scope_id,
        parent_proxy: &fragment_parent_proxy,
        child_scope_id: owner_scope_id,
    });
    // Re-home any effects the fragment installed against its detached
    // wrapper (bare `{{ }}` slot text) onto the live element receiving the
    // content, so they release with the subtree instead of leaking.
    if let Some(parent_el) = parent.dyn_ref::<Element>() {
        adopt_pending_effects(&buffer, parent_el);
    }
    let kids = buffer.child_nodes();
    let mut snapshot: Vec<Node> = Vec::with_capacity(kids.length() as usize);
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i) {
            snapshot.push(n);
        }
    }
    let mut inserted_nodes = Vec::with_capacity(snapshot.len());
    let mut inserted_elements = Vec::new();
    for n in snapshot {
        if parent.insert_before(&n, Some(slot_el)).is_err() {
            continue;
        }
        inserted_nodes.push(n.clone());
        if let Ok(e) = n.dyn_into::<Element>() {
            if let Some(slot_scope_id) = slot_scope_for_pin {
                bind_borrowed_scope_to(&e, slot_scope_id, &fragment_parent_proxy);
            }
            inserted_elements.push(e);
        }
    }
    if let Some(slot_scope_id) = slot_scope_for_pin {
        bind_slot_scope_owners(slot_scope_id, &inserted_nodes);
    }
    for element in inserted_elements {
        finalize_compiled_subtree(&element);
    }
    let _ = parent.remove_child(slot_el);
}

fn capture_light_dom_slots(
    el: &Element,
    fallback_scope_id: ScopeId,
    fallback_proxy: &JsValue,
) -> HashMap<String, CapturedSlot> {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return HashMap::new();
    };
    let (owner_scope_id, owner_proxy) = match enclosing_scope(el) {
        Some(s) => s,
        None => (fallback_scope_id, fallback_proxy.clone()),
    };

    let mut by_name: HashMap<String, CapturedSlot> = HashMap::new();
    let default_fragment = doc.create_document_fragment();
    let children = el.child_nodes();
    let mut snapshot: Vec<Node> = Vec::with_capacity(children.length() as usize);
    for i in 0..children.length() {
        if let Some(n) = children.item(i) {
            snapshot.push(n);
        }
    }
    for n in snapshot {
        if let Some(tpl) = n.dyn_ref::<HtmlTemplateElement>()
            && let Some(name) = tpl.get_attribute("pp-slot")
        {
            by_name.insert(
                name,
                CapturedSlot {
                    source: tpl.content(),
                    ident: tpl.get_attribute("pp-let").unwrap_or_default(),
                    owner_scope_id,
                    owner_proxy: owner_proxy.clone(),
                },
            );
            continue;
        }
        let _ = default_fragment.append_child(&n);
    }
    if default_fragment.child_nodes().length() > 0 {
        by_name
            .entry("default".to_string())
            .or_insert(CapturedSlot {
                source: default_fragment,
                ident: String::new(),
                owner_scope_id,
                owner_proxy,
            });
    }
    by_name
}

fn materialize_captured_light_dom_slot(
    slot_el: &Element,
    parent: &Node,
    slot_name: &str,
    bindings: &[(String, String)],
    owner_scope_id: ScopeId,
) -> bool {
    let captured = LIGHT_DOM_SLOTS.with(|stores| {
        stores
            .borrow()
            .get(&owner_scope_id)
            .and_then(|slots| slots.get(slot_name).cloned())
    });
    let Some(captured) = captured else {
        return false;
    };

    let source: Node = captured
        .source
        .clone_node_with_deep(true)
        .unwrap_or_else(|_| captured.source.clone().into());
    let mut snapshot: Vec<Node> = Vec::with_capacity(source.child_nodes().length() as usize);
    let kids = source.child_nodes();
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i) {
            snapshot.push(n);
        }
    }
    let mut inserted_nodes: Vec<Node> = Vec::new();
    let mut inserted: Vec<Element> = Vec::new();
    for n in snapshot {
        if parent.insert_before(&n, Some(slot_el)).is_err() {
            continue;
        }
        inserted_nodes.push(n.clone());
        if let Ok(e) = n.dyn_into::<Element>() {
            inserted.push(e);
        }
    }
    let _ = parent.remove_child(slot_el);

    if !bindings.is_empty() && !captured.ident.is_empty() {
        let slot_state = SlotScope {
            ident: captured.ident,
            bindings: bindings.to_vec(),
            bind_source_scope_id: owner_scope_id,
            caller_scope_id: captured.owner_scope_id,
        };
        let slot_scope = Scope::new(Rc::new(RefCell::new(slot_state)));
        crate::context::set_parent(slot_scope.id, owner_scope_id);
        let proxy = slot_scope.into_proxy();
        for el in &inserted {
            bind_borrowed_scope_to(el, slot_scope.id, &proxy);
        }
        bind_slot_scope_owners(slot_scope.id, &inserted_nodes);
    } else {
        for el in &inserted {
            bind_borrowed_scope_to(el, captured.owner_scope_id, &captured.owner_proxy);
            set_private(
                el,
                CTX_PARENT_KEY,
                &JsValue::from_f64(owner_scope_id.0 as f64),
            );
        }
    }

    for el in &inserted {
        mount_captured_light_dom_components(el);
    }
    for el in inserted {
        finalize_compiled_subtree(&el);
    }
    true
}

fn mount_captured_light_dom_components(root: &Element) {
    let tags = crate::templates::registered_template_names();
    if tags.is_empty() {
        return;
    }
    if tags.iter().any(|t| t == &root.local_name()) {
        if get_private(root, "__pp_mounted").is_none() {
            let tag = root.local_name();
            mount_child_component(root, &tag);
        }
        return;
    }
    let mut roots: Vec<Element> = Vec::new();
    let selector = tags.join(",");
    if let Ok(matches) = root.query_selector_all(&selector) {
        for i in 0..matches.length() {
            let Some(node) = matches.item(i) else {
                continue;
            };
            let Ok(el) = node.dyn_into::<Element>() else {
                continue;
            };
            if root.contains(Some(el.as_ref())) && get_private(&el, "__pp_mounted").is_none() {
                roots.push(el);
            }
        }
    }
    for el in roots {
        if !root.contains(Some(el.as_ref())) {
            continue;
        }
        let tag = el.local_name();
        mount_child_component(&el, &tag);
    }
}

pub(crate) fn clear_light_dom_slots(scope_id: ScopeId) {
    LIGHT_DOM_SLOTS.with(|stores| {
        stores.borrow_mut().remove(&scope_id);
    });
}

/// Splice the slot element's own default children in place of the
/// `<slot>` tag. Used when no parent-supplied fragment exists for
/// `slot_el`'s name, or when a scoped slot's binding shape doesn't
/// match the fragment's expectations.
fn materialize_slot_default(
    slot_el: &Element,
    parent: &Node,
    owner_scope_id: &ScopeId,
    owner_proxy: &JsValue,
) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let frag: DocumentFragment = doc.create_document_fragment();
    let kids = slot_el.child_nodes();
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i)
            && let Ok(clone) = n.clone_node_with_deep(true)
        {
            let _ = frag.append_child(&clone);
        }
    }
    let frag_kids = frag.child_nodes();
    let mut snapshot: Vec<Node> = Vec::with_capacity(frag_kids.length() as usize);
    for i in 0..frag_kids.length() {
        if let Some(n) = frag_kids.item(i) {
            snapshot.push(n);
        }
    }
    let mut inserted: Vec<Element> = Vec::new();
    for n in snapshot {
        let _ = parent.insert_before(&n, Some(slot_el));
        if let Ok(e) = n.dyn_into::<Element>() {
            inserted.push(e);
        }
    }
    let _ = parent.remove_child(slot_el);
    for el in &inserted {
        bind_borrowed_scope_to(el, *owner_scope_id, owner_proxy);
        set_private(
            el,
            CTX_PARENT_KEY,
            &JsValue::from_f64(owner_scope_id.0 as f64),
        );
    }
    for el in inserted {
        finalize_compiled_subtree(&el);
    }
}

/// RFC-031 — return the child component's scope id AND proxy so
/// callers can consult `is_prop` on the child's `ComponentState`.
/// Lazy-mints the proxy; prefer [`child_component_scope_id`] +
/// the scoped writer where the proxy isn't actually needed.
pub fn child_component_scope(el: &Element) -> Option<(ScopeId, JsValue)> {
    if !is_registered(&el.local_name()) {
        return None;
    }
    let root = first_element_child(el)?;
    scope_of_element(&root)
}

/// RFC-096 S1 — [`child_component_scope`] minus the proxy: reads
/// the stamped `SCOPE_ID_KEY` without lazy-minting. The write
/// mirror (`scope::write_field`) only needs the id, so callers
/// that switched to it stop forcing a proxy onto (possibly
/// W3b-elided) children.
pub fn child_component_scope_id(el: &Element) -> Option<ScopeId> {
    if !is_registered(&el.local_name()) {
        return None;
    }
    let root = first_element_child(el)?;
    let id = get_private(&root, SCOPE_ID_KEY).and_then(|v| v.as_f64())?;
    Some(ScopeId(id as u64))
}

/// Climb the parent chain until we find an element with a bound scope.
pub(crate) fn enclosing_scope_id(el: &Element) -> Option<ScopeId> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(e) = cur {
        if let Some(id_num) = get_private(&e, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
            return Some(ScopeId(id_num as u64));
        }
        cur = e.parent_element();
    }
    None
}

/// Climb the parent chain until we find an element with a bound scope.
pub fn enclosing_scope(el: &Element) -> Option<(ScopeId, JsValue)> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(e) = cur {
        if let Some(id_num) = get_private(&e, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
            let scope_id = ScopeId(id_num as u64);
            if let Some(proxy) = get_private(&e, SCOPE_PROXY_KEY) {
                return Some((scope_id, proxy));
            }
            // RFC 054 — compiled rows stamp only `SCOPE_ID_KEY` when
            // their plan is FastExpr-only. Lazy-mint here so any
            // caller that does need a proxy gets one.
            if let Some(scope) = Scope::find(scope_id) {
                let proxy = scope.into_proxy();
                set_private(&e, SCOPE_PROXY_KEY, &proxy);
                return Some((scope_id, proxy));
            }
        }
        cur = e.parent_element();
    }
    None
}

/// Read the explicit `CTX_PARENT_KEY` stamp off `el` if one was set.
/// Public so directive installers (`pp-for`, `pp-if`, `pp-teleport`)
/// can route their internal scopes' inject parents through the same
/// key the slot materialiser uses.
pub fn ctx_parent_of(el: &Element) -> Option<ScopeId> {
    get_private(el, CTX_PARENT_KEY)
        .and_then(|v| v.as_f64())
        .map(|n| ScopeId(n as u64))
}

/// Walk `el` then its element ancestors looking for the nearest
/// `CTX_PARENT_KEY` stamp.
pub fn inherited_ctx_parent_of(el: &Element) -> Option<ScopeId> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(e) = cur {
        if let Some(id) = get_private(&e, CTX_PARENT_KEY).and_then(|v| v.as_f64()) {
            return Some(ScopeId(id as u64));
        }
        cur = e.parent_element();
    }
    None
}

fn enclosing_inject_parent(el: &Element) -> Option<ScopeId> {
    let mut cur: Option<Element> = el.parent_element();
    while let Some(e) = cur {
        if let Some(id) = get_private(&e, CTX_PARENT_KEY).and_then(|v| v.as_f64()) {
            return Some(ScopeId(id as u64));
        }
        if let Some(id) = get_private(&e, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
            return Some(ScopeId(id as u64));
        }
        cur = e.parent_element();
    }
    None
}

/// RFC-095 W3b — test/diagnostic probe: has this element's scope
/// proxy been minted? Proxy-elided components answer `false`
/// until something dynamic forces a lazy mint.
#[doc(hidden)]
pub fn has_minted_proxy(el: &Element) -> bool {
    get_private(el, SCOPE_PROXY_KEY).is_some()
}

/// If `el` itself owns a scope (i.e. it's a component root), return it.
/// Used by directives (e.g. `pp-bind:`) that need to decide whether they're
/// writing to an HTML attribute or to a child-component prop.
pub fn scope_of_element(el: &Element) -> Option<(ScopeId, JsValue)> {
    let id_num = get_private(el, SCOPE_ID_KEY).and_then(|v| v.as_f64())?;
    let scope_id = ScopeId(id_num as u64);
    if let Some(proxy) = get_private(el, SCOPE_PROXY_KEY) {
        return Some((scope_id, proxy));
    }
    let scope = Scope::find(scope_id)?;
    let proxy = scope.into_proxy();
    set_private(el, SCOPE_PROXY_KEY, &proxy);
    Some((scope_id, proxy))
}

/// Find the DOM element that has `scope_id` pinned onto it. Walks
/// from `<body>` downward — O(n) in the number of elements, fine for
/// devtools hover lookups but not for hot paths.
pub fn find_element_for_scope(scope_id: ScopeId) -> Option<Element> {
    let body = web_sys::window()?.document()?.body()?;
    let root: Element = body.into();
    find_in_subtree(&root, scope_id)
}

fn find_in_subtree(root: &Element, scope_id: ScopeId) -> Option<Element> {
    if let Some(id_num) = get_private(root, SCOPE_ID_KEY).and_then(|v| v.as_f64())
        && id_num as u64 == scope_id.0
    {
        return Some(root.clone());
    }
    let children = root.children();
    for i in 0..children.length() {
        if let Some(child) = children.item(i)
            && let Some(found) = find_in_subtree(&child, scope_id)
        {
            return Some(found);
        }
    }
    None
}

/// Finish a compiled subtree without running directive discovery.
///
/// Generated fragment paths call this after generated plan code
/// has installed every known binding/listener/controller. It
/// preserves the post-order observable work — `on_mount`,
/// `on_ready`, and the re-walk guard — but intentionally does
/// not scan attributes or mount custom tags.
pub fn finalize_compiled_subtree(el: &Element) {
    if get_private(el, WALKED_KEY)
        .map(|v| v.is_truthy())
        .unwrap_or(false)
    {
        return;
    }
    let children = el.children();
    let mut snapshot: Vec<Element> = Vec::with_capacity(children.length() as usize);
    for i in 0..children.length() {
        if let Some(c) = children.item(i) {
            snapshot.push(c);
        }
    }
    for child in snapshot {
        finalize_compiled_subtree(&child);
    }
    fire_mount_hook(el);
    set_private(el, WALKED_KEY, &JsValue::TRUE);
}

/// Attach an effect id to an element so it can be released on unmount.
pub fn track_effect_on(el: &Element, id: EffectId) {
    let list = match get_private(el, EFFECTS_KEY) {
        Some(v) if v.is_object() => v.dyn_into::<Array>().ok(),
        _ => None,
    }
    .unwrap_or_else(Array::new);
    list.push(&JsValue::from_f64(id.0 as f64));
    set_private(el, EFFECTS_KEY, &list);
}

/// Where a detached-install wrapper's effect ids wait for a live owner (see
/// [`stash_wrapper_effects`] / [`adopt_pending_effects`]).
const PENDING_EFFECTS_KEY: &str = "__pp_pending_effects";

/// Move the effect ids tracked on `wrapper` — a temporary install host that
/// never enters the DOM (e.g. `stamp_dynamic_slot_with`'s `<div>`) — onto the
/// buffer `host`, so the splice site can re-home them on a live element via
/// [`adopt_pending_effects`]. Without this, an effect resolved against the
/// wrapper itself (a bare top-level `{{ }}` slot interpolation) is never
/// released: `release_subtree` walks live elements only.
pub(crate) fn stash_wrapper_effects(wrapper: &Element, host: &web_sys::DocumentFragment) {
    if let Some(v) = get_private(wrapper, EFFECTS_KEY)
        && v.is_object()
    {
        let _ = Reflect::set(host.as_ref(), &PENDING_EFFECTS_KEY.into(), &v);
        set_private(wrapper, EFFECTS_KEY, &JsValue::UNDEFINED);
    }
}

/// Re-home effect ids stashed on a fragment buffer by
/// [`stash_wrapper_effects`] onto `owner` — the live element that contains
/// the spliced content, so the effects release exactly when that subtree
/// does.
pub(crate) fn adopt_pending_effects(host: &web_sys::DocumentFragment, owner: &Element) {
    let Ok(v) = Reflect::get(host.as_ref(), &PENDING_EFFECTS_KEY.into()) else {
        return;
    };
    let Ok(arr) = v.dyn_into::<Array>() else {
        return;
    };
    for i in 0..arr.length() {
        if let Some(n) = arr.get(i).as_f64() {
            track_effect_on(owner, EffectId(n as u64));
        }
    }
    let _ = Reflect::set(
        host.as_ref(),
        &PENDING_EFFECTS_KEY.into(),
        &JsValue::UNDEFINED,
    );
}

// ── Element-scoped listener side-table ────────────────────────────
//
// `pp-on` / `pp-model` / `pp-route` previously called
// `closure.forget()`, which leaks the Rust `Box<dyn FnMut>` for the
// listener's lifetime AND — for `.window` / `.document` / `.outside`
// variants whose target is not the element itself — keeps the
// listener firing past unmount.
//
// The fix: every listener the runtime registers goes through
// `track_listener_on`. That stashes the `(target, event, capture,
// closure)` tuple in a thread-local table keyed by a numeric id
// stamped on the element via the existing `set_private` path.
// `release_subtree` walks the ids, calls
// `remove_event_listener_with_callback` for each, and drops the
// `Closure` — which drops the underlying `Box<dyn FnMut>`.

/// One installed listener. Kept alive by the side-table so the
/// closure's JS function pointer stays valid; torn down when the
/// owning element unmounts.
struct ListenerEntry {
    target: EventTarget,
    event: String,
    capture: bool,
    closure: Closure<dyn FnMut(Event)>,
}

thread_local! {
    /// Monotonically-increasing id stamped on each element that
    /// tracks listeners.
    static LISTENER_NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    static LISTENERS: RefCell<HashMap<u64, Vec<ListenerEntry>>> =
        RefCell::new(HashMap::new());
}

fn listener_slot_for(el: &Element) -> u64 {
    if let Some(v) = get_private(el, LISTENERS_KEY).and_then(|v| v.as_f64()) {
        return v as u64;
    }
    let id = LISTENER_NEXT_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    });
    set_private(el, LISTENERS_KEY, &JsValue::from_f64(id as f64));
    id
}

/// Install `closure` as an event listener for `event` on `target`,
/// and tie its lifetime to `el`. When `el`'s subtree is released,
/// `remove_event_listener_with_callback` runs and the closure's
/// `Box<dyn FnMut>` is dropped.
pub fn track_listener_on(
    el: &Element,
    target: EventTarget,
    event: &str,
    capture: bool,
    closure: Closure<dyn FnMut(Event)>,
) {
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_capture(capture);
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        event,
        closure.as_ref().unchecked_ref(),
        &opts,
    );
    let slot = listener_slot_for(el);
    LISTENERS.with(|m| {
        m.borrow_mut().entry(slot).or_default().push(ListenerEntry {
            target,
            event: event.to_string(),
            capture,
            closure,
        });
    });
}

/// Same as [`track_listener_on`] but passes through extra
/// `AddEventListenerOptions` (currently only `once`). A `once`
/// listener still needs cleanup in case the element unmounts
/// before the event fires.
pub fn track_listener_on_with_opts(
    el: &Element,
    target: EventTarget,
    event: &str,
    opts: &web_sys::AddEventListenerOptions,
    closure: Closure<dyn FnMut(Event)>,
) {
    let capture = opts.get_capture().unwrap_or(false);
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        event,
        closure.as_ref().unchecked_ref(),
        opts,
    );
    let slot = listener_slot_for(el);
    LISTENERS.with(|m| {
        m.borrow_mut().entry(slot).or_default().push(ListenerEntry {
            target,
            event: event.to_string(),
            capture,
            closure,
        });
    });
}

fn release_listeners(el: &Element) {
    let Some(slot) = get_private(el, LISTENERS_KEY).and_then(|v| v.as_f64()) else {
        return;
    };
    let entries = LISTENERS.with(|m| m.borrow_mut().remove(&(slot as u64)));
    if let Some(entries) = entries {
        for e in entries {
            let _ = e.target.remove_event_listener_with_callback_and_bool(
                &e.event,
                e.closure.as_ref().unchecked_ref(),
                e.capture,
            );
            drop(e);
        }
    }
}

/// Count of listener entries currently retained by the
/// element-scoped listener table. Used by tests (assert
/// `release_subtree` reclaims everything) and by the devtools
/// memory-health panel (leak-over-time sparkline).
#[cfg(any(debug_assertions, feature = "devtools"))]
pub fn listener_count() -> usize {
    LISTENERS.with(|m| m.borrow().values().map(|v| v.len()).sum())
}

pub(crate) fn release_subtree(node: &Node) {
    let unmount_start = crate::profiler::unmount::start();
    release_subtree_inner(node);
    crate::profiler::unmount::record_total(unmount_start);
}

/// Release every effect, listener, scope, and ref tied to the
/// elements rooted at `el`. Public entry point for generated
/// mount code (RFC-058 Phase 2+) that owns subtree teardown
/// directly — `pp-if`'s controller, `pp-for`'s row removal,
/// route-cluster swap, etc.
pub fn release_compiled_subtree(el: &Element) {
    release_subtree(el.as_ref());
}

fn release_subtree_inner(node: &Node) {
    if let Ok(el) = node.clone().dyn_into::<Element>() {
        run_before_subtree_release(&el);
        // RFC 054 bulk-clear short-circuit. When the row was torn
        // down synchronously by `for_::run_keyed`'s bulk path, the
        // row root carries `RELEASE_SKIP_KEY`. The entire subtree's
        // state has already been freed; the standard side-table
        // sweep below would pay 5+ `Reflect::get` calls per
        // descendant element for nothing.
        if get_private(&el, RELEASE_SKIP_KEY).is_some() {
            release_slot_scope_owner(node);
            return;
        }
        // Scoped-slot ownership can be stamped on a top-level text node
        // (for example a bare `{{ ctx.label }}` fragment). Visit all child
        // nodes, not only elements, so those ownership counts reach zero.
        let children = el.child_nodes();
        for i in 0..children.length() {
            if let Some(c) = children.item(i) {
                release_subtree_inner(&c);
            }
        }
        if let Some(v) = get_private(&el, EFFECTS_KEY)
            && let Ok(arr) = v.dyn_into::<Array>()
        {
            for i in 0..arr.length() {
                if let Some(n) = arr.get(i).as_f64() {
                    release(EffectId(n as u64));
                }
            }
        }
        if let Some(id) = get_private(&el, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
            let borrowed = get_private(&el, SCOPE_BORROWED_KEY)
                .map(|v| v.is_truthy())
                .unwrap_or(false);
            if !borrowed {
                let scope_id = ScopeId(id as u64);
                // Hold teardown inside the same callback frame as the user
                // unmount hook. Scope-owned work queued by that hook is then
                // skipped after `Scope::remove`; unscoped renderer work runs
                // only once the component borrow and teardown have finished.
                let _frame = crate::ComponentCallbackFrame::for_scope(scope_id);
                if let Some(scope) = Scope::find(scope_id) {
                    let unmount_ctx = crate::lifecycle::LifecycleContext::__new(
                        &el,
                        scope_id,
                        crate::lifecycle::LifecyclePhase::Unmount,
                    );
                    crate::scope::with_current_scope_id(scope_id, || {
                        scope.state.borrow_mut().unmount(unmount_ctx);
                    });
                }
                if crate::plugin::has_component_unmounted_hooks() {
                    crate::plugin::emit(crate::plugin::ComponentUnmounted {
                        component: component_name_for(scope_id),
                        scope_id,
                    });
                }
                COMPONENT_NAMES.with(|names| {
                    names.borrow_mut().remove(&scope_id);
                });
                // Drop the per-scope loader-data slot (RFC-078
                // §5.4 per-mount lifetime). No-op when the scope
                // didn't host a route loader.
                crate::router::release_loader_slot(scope_id);
                Scope::remove(scope_id);
            }
        }
        crate::directives::transition::release(&el);
        crate::directives::teleport::release(&el);
        crate::directives::resize::release(&el);
        crate::directives::intersect::release(&el);
        crate::directives::anchor::release(&el);
        crate::directives::flip::release(&el);
        crate::refs::unregister_element(&el);
        release_listeners(&el);
        match el.local_name().as_str() {
            "pp-component" => crate::dynamic_component::release_host(&el),
            "pp-outlet" => {
                crate::dynamic_component::release_host(&el);
                crate::router::release_outlet(&el);
            }
            _ => {}
        }
    }
    release_slot_scope_owner(node);
}

/// Clear the private sentinels that live on a component's custom-element
/// host. The rendered root owns the scope and is removed separately; these
/// host-only stamps must not block a later typed remount on the same element.
pub(crate) fn clear_component_host_stamps(host: &Element) {
    for key in [
        "__pp_mounted",
        HOST_CHILD_SCOPE_ID_KEY,
        WALKED_KEY,
        EFFECTS_KEY,
        LISTENERS_KEY,
        BEFORE_SUBTREE_RELEASE_KEY,
        MOUNT_HOOK_FIRED_KEY,
        COMPONENT_MOUNT_EVENT_FIRED_KEY,
        MOUNT_START_MS_KEY,
        PENDING_EFFECTS_KEY,
    ] {
        set_private(host, key, &JsValue::UNDEFINED);
    }
}

fn set_private(el: &Element, key: &str, value: &JsValue) {
    let _ = Reflect::set(el.as_ref(), &key.into(), value);
}

fn fire_component_mounted_plugin_hooks(el: &Element, scope_id: ScopeId) {
    if !crate::plugin::has_component_mounted_hooks() {
        return;
    }
    if get_private(el, COMPONENT_MOUNT_EVENT_FIRED_KEY)
        .map(|v| v.is_truthy())
        .unwrap_or(false)
    {
        return;
    }
    set_private(el, COMPONENT_MOUNT_EVENT_FIRED_KEY, &JsValue::TRUE);
    let start_ms = get_private(el, MOUNT_START_MS_KEY)
        .and_then(|value| value.as_f64())
        .unwrap_or_else(js_sys::Date::now);
    let elapsed = js_sys::Date::now() - start_ms;
    crate::plugin::emit(crate::plugin::ComponentMounted {
        component: component_name_for(scope_id),
        scope_id,
        duration_ms: if elapsed.is_finite() && elapsed >= 0.0 {
            elapsed
        } else {
            0.0
        },
    });
}

fn stamp_plugin_metadata(
    root: &Element,
    tag: &str,
    scope_id: ScopeId,
    plugin_hooks: crate::plugin::ComponentHookActivity,
    mount_start_ms: Option<f64>,
) {
    if plugin_hooks.needs_component_name {
        let canonical = canonical_component_name(tag);
        COMPONENT_NAMES.with(|names| {
            names.borrow_mut().insert(scope_id, canonical);
        });
    }
    if let Some(start_ms) = mount_start_ms {
        set_private(root, MOUNT_START_MS_KEY, &JsValue::from_f64(start_ms));
    }
}

fn fire_component_setup_plugin_hooks(tag: &str, scope_id: ScopeId) {
    if !crate::plugin::has_component_setup_hooks() {
        return;
    }
    crate::plugin::emit(crate::plugin::ComponentSetup {
        component: canonical_component_name(tag),
        scope_id,
    });
}

// Resolves the canonical component name for plugin events. The
// side-table is populated whenever any of `HOOK_COMPONENT_NAME_EVENTS`
// (mounted, ready, unmounted) is active, and the readers below are all
// gated on the same bitmask — so the `<unknown>` fallback is
// unreachable in practice and exists only as a release-mode guard
// against an invariant break.
fn component_name_for(scope_id: ScopeId) -> &'static str {
    COMPONENT_NAMES.with(|names| {
        if let Some(&name) = names.borrow().get(&scope_id) {
            return name;
        }
        debug_assert!(
            false,
            "component name side-table missing entry for scope {scope_id:?}"
        );
        "<unknown>"
    })
}

fn canonical_component_name(name: &str) -> &'static str {
    crate::registry::canonical_component_name(name).unwrap_or("<unknown>")
}

fn get_private(el: &Element, key: &str) -> Option<JsValue> {
    Reflect::get(el.as_ref(), &key.into())
        .ok()
        .filter(|v| !v.is_undefined())
}
