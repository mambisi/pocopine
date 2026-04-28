//! Compiled-view runtime hooks + adopted-DOM compatibility bridge.
//!
//! RFC-058 Phase 6.5 retired the runtime walker. The
//! `MutationObserver`, the recursive `pp-*` attribute scan, and
//! the `start` / `start_on_body` entry points are gone. The
//! directive registry has shrunk to the five typed-install
//! opaque directives the compiled plan applier uses
//! (`anchor` / `roving` / `intersect` / `resize` / `flip`).
//! There is no longer any runtime `pp-*` parsing or dispatch
//! loop. **Authoring `pp-*` / `:prop` / `@event` / `pp-text` /
//! `pp-bind` / `pp-show` / `pp-init` / `pp-model` directly on
//! arbitrary runtime HTML is no longer a framework feature** —
//! such directives only bind when the macro processes them at
//! compile time inside a `#[component]` template.
//!
//! ## What this module ships now
//!
//! ### 1. Compiled-view mount runtime
//!
//! The surface every macro-emitted template plan calls into:
//!
//! - [`start_compiled`] — discover registered component tags
//!   under a root and mount each via its template plan.
//! - [`mount_component`], [`mount_child_component`],
//!   [`mount_child_component_with_slots`] — the per-component
//!   mount entry called by `apply_static_plan`.
//! - Scope / proxy stamping helpers used by `pp-for` rows,
//!   `pp-if` bodies, `pp-teleport` portals, and slot fragments.
//! - Lifecycle dispatch helpers shared between `mount_component`
//!   and the plan applier (`fire_deferred_init`,
//!   `fire_mount_post_order`, `fire_ready_next_tick`,
//!   `finalize_compiled_subtree`).
//! - Element-scoped listener and effect side tables so a
//!   subtree teardown can release everything tied to it
//!   (`track_listener_on`, `track_effect_on`, `release_subtree`).
//!
//! ### 2. Adopted-DOM compatibility bridge — explicit, narrow
//!
//! A small set of helpers handle the cases where the macro
//! never saw the DOM tree (user-authored slot content, runtime-
//! injected partials, app-root mounts whose host is a literal
//! HTML string). The bridge's contract:
//!
//! | Allowed | Disallowed |
//! |---|---|
//! | Discover registered custom-component tags + mount them | Bind `pp-*` / `:prop` / `@event` on plain or custom-tag hosts |
//! | Install `<template pp-for>` / `<template pp-if>` / `<template pp-teleport>` controllers (structural only) | Per-element `pp-text` / `pp-bind` / `pp-show` / `pp-init` / `pp-model` / `pp-html` on adopted DOM |
//! | Materialise runtime-captured slot content (`slots::lookup` consume path) | Anything that requires the deleted directive registry / `dispatch` / `parse_attr` |
//!
//! Bridge entry points (each named with a documented narrow
//! contract): [`mount_adopted_components`],
//! [`install_adopted_controllers`], [`materialize_adopted_slot`].
//! The bridge is the *only* code in this module that walks DOM
//! it didn't compile; it walks for tag-name discovery and
//! `<template pp-*>` discovery only — never for per-element
//! attribute scanning.
//!
//! Authors who need per-element directive binding on dynamic
//! content wrap that content in a `#[component]` template (or
//! the test-only `template_inline` shorthand) — the macro
//! compiles the directives at expansion time and the bridge is
//! never reached.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{DocumentFragment, Element, Event, EventTarget, HtmlTemplateElement, Node};

use crate::reactive::{release, EffectId, ScopeId};
use crate::registry::instantiate;
use crate::scope::Scope;
use crate::slot_scope::SlotScope;
use crate::slots::{self, SlotStore, UserSlot};
use crate::templates::{is_registered, template_for};

const SCOPE_ID_KEY: &str = "__pp_scope_id";
const SCOPE_PROXY_KEY: &str = "__pp_scope_proxy";
const SCOPE_BORROWED_KEY: &str = "__pp_scope_borrowed";
const EFFECTS_KEY: &str = "__pp_effects";
const LISTENERS_KEY: &str = "__pp_listeners";
const INIT_PENDING_KEY: &str = "__pp_init_pending";
const WALKED_KEY: &str = "__pp_walked";
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

/// Compiled-only mount entry. Discovers registered component tags
/// in `root`'s subtree via a single
/// [`Element::query_selector_all`] against the union of every
/// registered plan tag, then mounts each in document order.
///
/// Inner component tags inside a parent's compiled subtree will
/// already be mounted by the parent's `apply_static_plan`
/// child_mounts pass before the iteration reaches them; the
/// `__pp_mounted` guard short-circuits the duplicate mount.
///
/// After mounting components, every `<pp-outlet>` element under
/// `root` is registered with the router so route navigations can
/// paint into it.
pub fn start_compiled(root: &Element) {
    crate::styles::inject_style("__pp_cloak", "[pp-cloak] { display: none !important; }");
    let tags = crate::templates::registered_template_names();
    if !tags.is_empty() {
        let selector = tags.join(",");
        if let Ok(matches) = root.query_selector_all(&selector) {
            for i in 0..matches.length() {
                let Some(node) = matches.item(i) else {
                    continue;
                };
                let Ok(el) = node.dyn_into::<Element>() else {
                    continue;
                };
                if get_private(&el, "__pp_mounted").is_some() {
                    continue;
                }
                let tag = el.local_name();
                mount_component(&el, &tag, None);
                finalize_compiled_subtree(&el);
            }
        }
    }
    // RFC-058 Phase 6.5 — register every `<pp-outlet>` under `root`
    // with the router. Previously the legacy `bind` step matched the
    // tag and called `set_outlet`; with the walker gone, the
    // compiled mount entry takes over.
    if let Ok(outlets) = root.query_selector_all("pp-outlet") {
        for i in 0..outlets.length() {
            let Some(node) = outlets.item(i) else {
                continue;
            };
            if let Ok(el) = node.dyn_into::<Element>() {
                crate::router::set_outlet(el);
            }
        }
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

/// Read just the scope id without forcing proxy lazy-mint. Used by
/// the compiled-row mount loop in `for_.rs::run_keyed`, which has
/// the proxy-or-None decision already made and shouldn't pay for a
/// proxy fetch it'll throw away.
pub fn scope_id_of_element(el: &Element) -> Option<ScopeId> {
    let id_num = get_private(el, SCOPE_ID_KEY).and_then(|v| v.as_f64())?;
    Some(ScopeId(id_num as u64))
}

/// Pin a **borrowed** scope. Same lookup semantics as `bind_scope_to`,
/// but `release_subtree` will leave the scope alone when this element
/// unmounts — the real owner is elsewhere. Used by `pp-teleport` and
/// the teleport path of `pp-if` to keep the enclosing component's
/// scope reachable from a clone that lives outside the component's
/// subtree.
pub fn bind_borrowed_scope_to(el: &Element, scope_id: ScopeId, proxy: &JsValue) {
    set_private(el, SCOPE_ID_KEY, &JsValue::from_f64(scope_id.0 as f64));
    set_private(el, SCOPE_PROXY_KEY, proxy);
    set_private(el, SCOPE_BORROWED_KEY, &JsValue::TRUE);
}

fn fire_deferred_init(el: &Element) {
    let Some(value) = get_private(el, INIT_PENDING_KEY).and_then(|v| v.as_string()) else {
        return;
    };
    let _ = Reflect::delete_property(el.as_ref(), &INIT_PENDING_KEY.into());
    let Some((scope_id, _proxy)) = enclosing_scope(el) else {
        return;
    };
    crate::directives::init::install(el, scope_id, &value);
}

/// Defer a `pp-init` handler invocation on `el` until the
/// surrounding subtree's plan install completes (post-order
/// drain in `finalize_compiled_subtree` — see `fire_deferred_init`).
/// Public entry point for the macro-emitted plan applier; the
/// deferred-init pending state stays owned by a single
/// implementation.
///
/// `scope_id` is accepted for API symmetry with the other
/// lifecycle helpers — the actual fire-time dispatch
/// rediscovers the scope through `enclosing_scope(el)` because
/// the same element may be re-queued under a fresh scope by
/// `pp-for` row reuse.
pub fn defer_init_on(el: &Element, scope_id: ScopeId, expr_src: &str) {
    let _ = scope_id; // see doc-comment
    set_private(el, INIT_PENDING_KEY, &JsValue::from_str(expr_src));
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
    if !has_mount {
        return;
    }
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
    if !has_ready {
        return;
    }
    let el_owned = el.clone();
    crate::tick::next(move || {
        let Some(scope) = Scope::find(scope_id) else {
            return;
        };
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
///  * bind the scope to the template's root and strip its `pp-data`,
///  * forward fallthrough attrs onto the template root (RFC-010),
///  * apply the registered template plan against the freshly
///    stamped subtree.
fn mount_component(
    el: &Element,
    tag: &str,
    supplied_slots: Option<(crate::slot_fragment::SlotSet, ScopeId, JsValue)>,
) {
    // RFC-019 — `pp-as` hoists the user's single child element as
    // the rendered root, discarding the template's wrapper. Only
    // engages when all the structural constraints hold; otherwise
    // falls through to the normal mount path.
    if el.has_attribute("pp-as") && try_mount_component_as(el, tag) {
        return;
    }

    let Some(scope) = instantiate(tag) else {
        return;
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
    // RFC-030: fire `on_setup` — the component's pre-children-walk
    // hook where fields can be initialised from injected context.
    // Runs with CURRENT_SCOPE_ID bound so `inject` / `this` resolve.
    if scope.state.borrow().has_setup() {
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
    let proxy = scope.into_proxy();
    crate::model_runtime::capture_emit_el(scope.id, el);

    // Capture slot content. Named slot templates go into the slot
    // store keyed by the component's scope id; everything else lands
    // in the default slot. Pass the just-created scope as the
    // owner-fallback so user-authored slot content at the
    // app-root mount (no enclosing component scope) still
    // resolves directives — `<template pp-for="…">` inside a
    // `<pine-tags-input-root>` mounted at the app root needs
    // *some* scope to bind against, and the only sensible one
    // is the owner.
    let slot_store = capture_slots(el, scope.id, &proxy);
    slots::put(scope.id, slot_store);
    if let Some((slots, parent_scope_id, parent_proxy)) = supplied_slots {
        crate::slot_fragment::install(scope.id, slots, parent_scope_id, parent_proxy);
    }

    // Clone the registered template in. `set_inner_html` drops the
    // tag's former children, which is the "capture" side of the old
    // flow. Prefer `template_clone_for` (parses the HTML once into a
    // cached `<template>` element, every mount clones the `.content`
    // `DocumentFragment`) over re-parsing the HTML string per mount.
    if let Some(fragment) = crate::templates::template_clone_for(tag) {
        el.set_inner_html("");
        let _ = el.append_child(fragment.as_ref());
    } else {
        let Some(html) = template_for(tag) else {
            return;
        };
        el.set_inner_html(&html);
    }

    // Bind scope to the template's root element and strip pp-data so
    // nothing later tries to re-instantiate it.
    if let Some(root) = first_element_child(el) {
        set_private(&root, SCOPE_ID_KEY, &JsValue::from_f64(scope.id.0 as f64));
        set_private(&root, SCOPE_PROXY_KEY, &proxy);
        let _ = root.remove_attribute("pp-data");

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
            let already_set = root.has_attribute("pp-transition:enter")
                || root.has_attribute("pp-transition:enter-start")
                || root.has_attribute("pp-transition:enter-end")
                || root.has_attribute("pp-transition:leave")
                || root.has_attribute("pp-transition:leave-start")
                || root.has_attribute("pp-transition:leave-end")
                || root.has_attribute("pp-transition")
                || root.has_attribute("pp-transition:in")
                || root.has_attribute("pp-transition:out")
                || el.has_attribute("pp-transition:enter")
                || el.has_attribute("pp-transition:enter-start")
                || el.has_attribute("pp-transition:enter-end")
                || el.has_attribute("pp-transition:leave")
                || el.has_attribute("pp-transition:leave-start")
                || el.has_attribute("pp-transition:leave-end")
                || el.has_attribute("pp-transition")
                || el.has_attribute("pp-transition:in")
                || el.has_attribute("pp-transition:out");
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

        // Apply the macro-emitted template plan against the freshly
        // stamped subtree. Every directive in the cleaned HTML is
        // installed via the typed plan helpers; nothing else scans
        // for `pp-*` attributes.
        if let Some(plan) = crate::templates_plan::template_plan_for(tag) {
            crate::templates_plan::apply_static_plan(&root, scope.id, &proxy, plan, tag);
        }
    }

    // Mark the tag as mounted so duplicate discovery (e.g. an outer
    // `start_compiled` query_selector hit after a parent already
    // mounted it via `child_mounts`) short-circuits.
    set_private(el, "__pp_mounted", &JsValue::TRUE);
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
    if scope.state.borrow().has_setup() {
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
    let proxy = scope.into_proxy();
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

    merge_template_attrs_as(&tpl_root, &user_root);

    set_private(
        &user_root,
        SCOPE_ID_KEY,
        &JsValue::from_f64(scope.id.0 as f64),
    );
    set_private(&user_root, SCOPE_PROXY_KEY, &proxy);
    let _ = user_root.remove_attribute("pp-data");

    apply_fallthrough_attrs(el, &user_root, &scope);

    if let Some(plan) = crate::templates_plan::template_plan_for(tag) {
        crate::templates_plan::apply_static_pp_as_plan(&user_root, scope.id, &proxy, plan, tag);
    }

    slots::put(
        scope.id,
        SlotStore {
            by_name: Default::default(),
        },
    );

    let _ = el.remove_attribute("pp-as");
    set_private(el, "__pp_mounted", &JsValue::TRUE);

    true
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
        if let Some(tpl) = el.dyn_ref::<HtmlTemplateElement>() {
            if tpl.has_attribute("pp-slot") {
                continue;
            }
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
/// (`pp-data`, `pp-as`) are dropped.
fn merge_template_attrs_as(tpl_root: &Element, user_root: &Element) {
    let attrs = tpl_root.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if name == "pp-data" || name == "pp-as" {
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

/// `setAttribute` rejects names whose first character isn't a
/// Name-start (per the XML Name production the DOM standard cites).
/// `:foo` is allowed but `@foo` isn't. Convert RFC-020 `@event`
/// shorthand to `pp-on:event` long form so the call goes through
/// cleanly. Other names pass through unchanged.
fn setattr_safe_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix('@') {
        if !rest.is_empty() {
            return format!("pp-on:{rest}");
        }
    }
    name.to_string()
}

/// Collect the component tag's direct children into named slots.
/// A child `<template pp-slot="name" pp-let="ident">` contributes
/// its `.content` fragment to the named slot; every other child
/// (text, elements, nested templates without `pp-slot`) goes into
/// the default slot.
fn capture_slots(el: &Element, fallback_scope_id: ScopeId, fallback_proxy: &JsValue) -> SlotStore {
    let doc = match web_sys::window().and_then(|w| w.document()) {
        Some(d) => d,
        None => {
            return SlotStore {
                by_name: Default::default(),
            }
        }
    };

    // Prefer the enclosing-scope (true author) when one exists;
    // fall back to the host's just-created scope so app-root
    // mounts (no parent component) still resolve directives in
    // user-authored slot content.
    let (owner_scope_id, owner_proxy) = match enclosing_scope(el) {
        Some(s) => s,
        None => (fallback_scope_id, fallback_proxy.clone()),
    };

    let mut by_name: std::collections::HashMap<String, UserSlot> = std::collections::HashMap::new();
    let default_fragment = doc.create_document_fragment();

    let children = el.child_nodes();
    let mut to_consume: Vec<Node> = Vec::with_capacity(children.length() as usize);
    for i in 0..children.length() {
        if let Some(n) = children.item(i) {
            to_consume.push(n);
        }
    }
    for n in to_consume {
        if let Some(tpl) = n.dyn_ref::<HtmlTemplateElement>() {
            if let Some(name) = tpl.get_attribute("pp-slot") {
                let ident = tpl.get_attribute("pp-let").unwrap_or_default();
                let source = tpl.content();
                if by_name.contains_key(&name) {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "pocopine: duplicate pp-slot=\"{name}\"; later wins"
                    )));
                }
                by_name.insert(
                    name,
                    UserSlot {
                        source,
                        ident,
                        owner_scope_id,
                        owner_proxy: owner_proxy.clone(),
                    },
                );
                continue;
            }
        }
        let _ = default_fragment.append_child(&n);
    }

    if default_fragment.child_nodes().length() > 0 {
        by_name.entry("default".to_string()).or_insert(UserSlot {
            source: default_fragment,
            ident: String::new(),
            owner_scope_id,
            owner_proxy,
        });
    }

    SlotStore { by_name }
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
        if !scope.state.borrow().is_prop(&field) {
            continue;
        }
        let raw = a.value();
        let js = coerce_attr_value(&raw);
        crate::model_runtime::with_scope_write(
            scope.id,
            crate::model_runtime::WriteOrigin::SetupSeed,
            || scope.state.borrow_mut().set(&field, js),
        );
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
    if matches!(first, Some(b'{') | Some(b'[') | Some(b'"')) {
        if let Ok(v) = js_sys::JSON::parse(raw) {
            return v;
        }
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
        // No compile-time parent-emitted fragment registered.
        // Try the runtime-captured slot store next (the
        // `mount_component` path's `capture_slots` → `slots::put`
        // bridge — used when a user writes
        // `<pine-tooltip-root><button>...</button></pine-tooltip-root>`
        // in a test or non-compiled host where no parent
        // template exists to emit a Phase-3.5b fragment).
        if let Some((fragment, ident, author_scope_id, author_proxy)) =
            crate::slots::lookup(owner_scope_id, &slot_name)
        {
            materialize_adopted_slot(
                slot_el,
                &parent,
                fragment,
                ident,
                author_scope_id,
                author_proxy,
                &bindings,
                owner_scope_id,
                &owner_proxy,
            );
            return;
        }
        // Fall back to the slot's default children — `<slot>`
        // content authored inline in the component template.
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
                    bind_source: owner_proxy.clone(),
                    caller: parent_proxy.clone(),
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
    let kids = buffer.child_nodes();
    let mut snapshot: Vec<Node> = Vec::with_capacity(kids.length() as usize);
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i) {
            snapshot.push(n);
        }
    }
    for n in snapshot {
        let _ = parent.insert_before(&n, Some(slot_el));
        if let Ok(e) = n.dyn_into::<Element>() {
            if let Some(slot_scope_id) = slot_scope_for_pin {
                bind_borrowed_scope_to(&e, slot_scope_id, &fragment_parent_proxy);
            }
            finalize_compiled_subtree(&e);
        }
    }
    let _ = parent.remove_child(slot_el);
}

/// **Adopted-DOM bridge — captured slot replay.** Splice
/// runtime-captured slot content (from `mount_component`'s
/// `capture_slots` → `slots::put` bridge) in place of the
/// `<slot>` tag. Pins inserted elements' borrowed scope to the
/// *author's* scope (not the slot owner's) so directives
/// inside the slot resolve against the caller per
/// RFC-011 / Vue convention. When the slot declares `:prop`
/// bindings AND the user wrote `pp-let`, builds a `SlotScope`
/// so `ident.field` routes to the slot's bound source while
/// fall-through reads still hit the author.
///
/// After splicing, runs [`mount_adopted_components`] over each
/// inserted element so custom-component tags inside slot
/// content (e.g. `<pine-icon-bell />` inside
/// `<pine-tooltip-trigger>`) get mounted via the compiled
/// path AND `<template pp-*>` controllers (e.g. a `pp-for`
/// over chips inside `<pine-tags-input-root>`) get installed.
///
/// **Bridge contract**: native HTML elements with `pp-*` /
/// `:prop` / `@event` etc. attributes inside the captured
/// fragment are left unbound — those directives only bind
/// when the macro processes them at compile time inside a
/// `#[component]` template. See module doc-comment.
#[allow(clippy::too_many_arguments)]
fn materialize_adopted_slot(
    slot_el: &Element,
    parent: &Node,
    fragment: web_sys::DocumentFragment,
    user_ident: String,
    author_scope_id: ScopeId,
    author_proxy: JsValue,
    bindings: &[(String, String)],
    owner_scope_id: ScopeId,
    owner_proxy: &JsValue,
) {
    // Snapshot fragment children before insertion mutates the
    // fragment's child list.
    let mut frag_snapshot: Vec<Node> = Vec::with_capacity(fragment.child_nodes().length() as usize);
    let kids = fragment.child_nodes();
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i) {
            frag_snapshot.push(n);
        }
    }
    let mut inserted: Vec<Element> = Vec::new();
    for n in frag_snapshot {
        let _ = parent.insert_before(&n, Some(slot_el));
        if let Ok(e) = n.dyn_into::<Element>() {
            inserted.push(e);
        }
    }
    let _ = parent.remove_child(slot_el);

    // Pin scope: SlotScope when the slot has :prop bindings AND
    // the user wrote pp-let; otherwise pin the author's scope
    // directly so directives inside resolve against the caller.
    if !bindings.is_empty() && !user_ident.is_empty() {
        let slot_state = SlotScope {
            ident: user_ident,
            bindings: bindings.to_vec(),
            bind_source: owner_proxy.clone(),
            caller: author_proxy.clone(),
            caller_scope_id: author_scope_id,
        };
        let slot_scope = Scope::new(Rc::new(RefCell::new(slot_state)));
        crate::context::set_parent(slot_scope.id, owner_scope_id);
        let proxy = slot_scope.into_proxy();
        for el in &inserted {
            bind_borrowed_scope_to(el, slot_scope.id, &proxy);
        }
    } else {
        for el in &inserted {
            bind_borrowed_scope_to(el, author_scope_id, &author_proxy);
            // Stamp explicit inject-chain parent so any
            // `mount_component` on a custom tag inside slot
            // content chains to the slot OWNER for RFC-027
            // inject (matching Pine's compound-context pattern),
            // not to whatever DOM-ancestor scope happens to be
            // sitting around.
            set_private(
                el,
                CTX_PARENT_KEY,
                &JsValue::from_f64(owner_scope_id.0 as f64),
            );
        }
    }

    // Mount any custom-component tags inside the inserted
    // subtree + fire lifecycle on every element. The discovery
    // pass mirrors `start_compiled` — querySelectorAll over the
    // registered tag set.
    for el in &inserted {
        mount_adopted_components(el);
    }
    for el in inserted {
        finalize_compiled_subtree(&el);
    }
}

/// **Adopted-DOM bridge entry.** Walk a subtree the macro
/// never compiled and reconcile its structure against the
/// compiled-mount runtime. Two passes:
///
///   1. Install structural controllers
///      ([`install_adopted_controllers`]) — finds every
///      `<template pp-for>` / `<template pp-if>` /
///      `<template pp-teleport>` in `root` and installs the
///      matching controller at runtime. The controllers run
///      with `body_fn = None`, so each row/branch goes through
///      `clone_template_body` + a recursive call back into
///      this function — enough to wake up custom-tag bodies.
///   2. Mount registered custom-component tags
///      ([`templates_plan::registered_template_tags`]) under
///      the root via [`mount_component`].
///
/// **Bridge contract — narrow on purpose**: this function
/// discovers structure (tag names, structural controller
/// templates) and nothing else. It does **not** parse or
/// install per-element `pp-*` / `:prop` / `@event` /
/// `pp-text` / `pp-bind` / `pp-show` / `pp-init` / `pp-model`
/// directives — those only bind when the macro processes them
/// at compile time inside a `#[component]` template. Authors
/// who need per-element directives on dynamic content wrap
/// that content in a `#[component]` (or the `template_inline`
/// test shorthand). See module doc-comment for the full
/// allowed / disallowed table.
///
/// Public so directive runtime helpers (notably `pp-for`'s
/// row install when `body_fn = None`) and the slot
/// materialiser ([`materialize_adopted_slot`]) can drive
/// component / controller discovery over freshly-cloned
/// template bodies.
pub fn mount_adopted_components(root: &Element) {
    // Step 1: install runtime controllers on user-authored
    // `<template pp-*>` elements. Done first because pp-for /
    // pp-if can produce custom tags that need mounting in step 2.
    install_adopted_controllers(root);

    // Step 2: mount any registered custom tags discovered in the
    // subtree (including the root itself).
    let tags = crate::templates::registered_template_names();
    if tags.is_empty() {
        return;
    }
    let selector = tags.join(",");
    let Ok(matches) = root.query_selector_all(&selector) else {
        return;
    };
    let mut roots: Vec<Element> = Vec::with_capacity(matches.length() as usize + 1);
    if tags.iter().any(|t| t == &root.local_name()) && get_private(root, "__pp_mounted").is_none() {
        roots.push(root.clone());
    }
    for i in 0..matches.length() {
        if let Some(node) = matches.item(i) {
            if let Ok(el) = node.dyn_into::<Element>() {
                if get_private(&el, "__pp_mounted").is_none() {
                    roots.push(el);
                }
            }
        }
    }
    for el in roots {
        let tag = el.local_name();
        mount_component(&el, &tag, None);
    }
}

/// **Adopted-DOM bridge — structural-controller discovery.**
/// Find every `<template pp-for>` / `<template pp-if>` /
/// `<template pp-teleport>` in `root`'s subtree (and `root`
/// itself) and install the matching controller. Used for
/// adopted-DOM containers the macro never saw: runtime-
/// captured slot content, `pp-for` row bodies cloned via
/// `clone_template_body` when `body_fn` was None, and any
/// other path that injects `<template pp-*>` markup at
/// runtime.
///
/// **Strict subset of walker behaviour**: this only handles
/// the three structural controllers. Per-element directive
/// binding (`pp-text`, `pp-bind`, `:prop`, `@event`,
/// `pp-show`, `pp-init`, `pp-model`, `pp-html`) is **not** in
/// the bridge contract — those need to be authored inside a
/// `#[component]` template so the macro processes them at
/// compile time. See module doc-comment.
fn install_adopted_controllers(root: &Element) {
    let Ok(templates) = root.query_selector_all("template") else {
        return;
    };
    let mut candidates: Vec<Element> = Vec::with_capacity(templates.length() as usize + 1);
    if root.local_name() == "template" {
        candidates.push(root.clone());
    }
    for i in 0..templates.length() {
        if let Some(node) = templates.item(i) {
            if let Ok(el) = node.dyn_into::<Element>() {
                candidates.push(el);
            }
        }
    }
    for tpl in candidates {
        let Ok(template) = tpl.clone().dyn_into::<web_sys::HtmlTemplateElement>() else {
            continue;
        };
        // Skip controllers we've already installed (re-discovery
        // passes hit the same template multiple times).
        if get_private(&tpl, "__pp_runtime_controller_installed").is_some() {
            continue;
        }
        let Some((scope_id, proxy)) = enclosing_scope(&tpl) else {
            continue;
        };
        if let Some(for_value) = tpl.get_attribute("pp-for") {
            // pp-for="<item> in <items>"
            let parts: Vec<&str> = for_value.splitn(2, " in ").collect();
            if parts.len() != 2 {
                continue;
            }
            let item_name = parts[0].trim().to_string();
            let items_expr = parts[1].trim().to_string();
            let key_expr = tpl.get_attribute("pp-key").map(|s| s.trim().to_string());
            let stagger_ms = tpl
                .get_attribute("pp-stagger")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0);
            set_private(&tpl, "__pp_runtime_controller_installed", &JsValue::TRUE);
            crate::directives::for_::install(
                template, proxy, scope_id, item_name, items_expr, key_expr, stagger_ms, None,
            );
            continue;
        }
        if let Some(if_value) = tpl.get_attribute("pp-if") {
            let Ok(ast) = crate::expr::parse_cached(&if_value) else {
                continue;
            };
            let teleport_selector = tpl.get_attribute("pp-teleport");
            set_private(&tpl, "__pp_runtime_controller_installed", &JsValue::TRUE);
            crate::directives::if_::install(
                template,
                proxy,
                ast,
                None,
                teleport_selector.as_deref(),
            );
            continue;
        }
        if let Some(teleport_selector) = tpl.get_attribute("pp-teleport") {
            set_private(&tpl, "__pp_runtime_controller_installed", &JsValue::TRUE);
            crate::directives::teleport::install(template, &teleport_selector, None);
            continue;
        }
    }
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
        if let Some(n) = kids.item(i) {
            if let Ok(clone) = n.clone_node_with_deep(true) {
                let _ = frag.append_child(&clone);
            }
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

/// If `el` is a registered-component tag with its scope mounted on the
/// template root, return the child component's proxy so directives like
/// `pp-bind:` can write props directly.
pub fn child_component_proxy(el: &Element) -> Option<JsValue> {
    child_component_scope(el).map(|(_, p)| p)
}

/// RFC-031 — like [`child_component_proxy`] but also returns the
/// child's scope id, so callers can consult `is_prop` on the
/// child's `ComponentState` before writing through the proxy.
pub fn child_component_scope(el: &Element) -> Option<(ScopeId, JsValue)> {
    if !is_registered(&el.local_name()) {
        return None;
    }
    let root = first_element_child(el)?;
    scope_of_element(&root)
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
    if let Some(id_num) = get_private(root, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
        if id_num as u64 == scope_id.0 {
            return Some(root.clone());
        }
    }
    let children = root.children();
    for i in 0..children.length() {
        if let Some(child) = children.item(i) {
            if let Some(found) = find_in_subtree(&child, scope_id) {
                return Some(found);
            }
        }
    }
    None
}

/// Finish a compiled subtree without running directive discovery.
///
/// Generated fragment paths call this after `apply_static_plan`
/// has installed every known binding/listener/controller. It
/// preserves the post-order observable work — deferred `pp-init`,
/// `on_mount`, `on_ready`, and the re-walk guard — but
/// intentionally does not scan attributes or mount custom tags.
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
    fire_deferred_init(el);
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
        // RFC 054 bulk-clear short-circuit. When the row was torn
        // down synchronously by `for_::run_keyed`'s bulk path, the
        // row root carries `RELEASE_SKIP_KEY`. The entire subtree's
        // state has already been freed; the standard side-table
        // sweep below would pay 5+ `Reflect::get` calls per
        // descendant element for nothing.
        if get_private(&el, RELEASE_SKIP_KEY).is_some() {
            return;
        }
        let children = el.children();
        for i in 0..children.length() {
            if let Some(c) = children.item(i) {
                release_subtree_inner(&c);
            }
        }
        if let Some(v) = get_private(&el, EFFECTS_KEY) {
            if let Ok(arr) = v.dyn_into::<Array>() {
                for i in 0..arr.length() {
                    if let Some(n) = arr.get(i).as_f64() {
                        release(EffectId(n as u64));
                    }
                }
            }
        }
        if let Some(id) = get_private(&el, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
            let borrowed = get_private(&el, SCOPE_BORROWED_KEY)
                .map(|v| v.is_truthy())
                .unwrap_or(false);
            if !borrowed {
                let scope_id = ScopeId(id as u64);
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
                Scope::remove(scope_id);
                crate::lifecycle::__clear_mount_epoch(scope_id);
            }
        }
        crate::directives::transition::release(&el);
        crate::directives::teleport::release(&el);
        crate::directives::resize::release(&el);
        crate::directives::intersect::release(&el);
        crate::directives::anchor::release(&el);
        crate::directives::roving::release(&el);
        crate::directives::flip::release(&el);
        release_listeners(&el);
    }
}

fn set_private(el: &Element, key: &str, value: &JsValue) {
    let _ = Reflect::set(el.as_ref(), &key.into(), value);
}

fn get_private(el: &Element, key: &str) -> Option<JsValue> {
    Reflect::get(el.as_ref(), &key.into())
        .ok()
        .filter(|v| !v.is_undefined())
}
