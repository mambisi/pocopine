//! RFC-058 Phase 2 — compiled template plans.
//!
//! Macro-emitted static descriptor for an entire compiled
//! template's bindings, listeners, refs, and deferred-init
//! entries. The runtime fast-path in [`crate::mount::mount_component`]
//! consumes the plan when one is registered for a component
//! tag, calling the cleanup-safe install helpers from RFC-058
//! Phase 1 directly instead of running the per-attribute
//! directive scan.
//!
//! Companion to [`crate::directives::for_plan`]:
//!
//! * `for_plan` — RFC-054 — keyed `pp-for` row bodies; per-row
//!   instance plan; reused across every row mount.
//! * `templates_plan` (this) — RFC-058 — whole-template plan;
//!   one entry per `#[component]`; runs once per mount.
//!
//! The two share `BindingKind` / `StaticBinding` / `StaticListener`
//! types from [`crate::directives::for_plan`] so the install
//! helpers don't need to switch on which compiler emitted the
//! entry.
//!
//! v1 envelope per RFC-057 §6 (deferred to RFC-058 Phase 2):
//!
//! * Eligible: native HTML elements only; `pp-text`, `pp-html`,
//!   `pp-show`, `pp-bind:<attr>` (HTML attrs, not child-component
//!   props), `pp-on:<event>` with the §6.1 supported modifier
//!   set, `pp-ref`, `pp-init` (deferred).
//! * Not eligible (attribute-preserved, mount-owned): every
//!   directive on or under a non-HTML-native tag, every
//!   directive on `pp-for` / `pp-if` / `pp-teleport` / `<slot>`
//!   subtrees, `pp-model`, `pp-route`, listeners with
//!   unsupported modifiers.
//! * Stripped attributes are owned by the plan and **fail fast**
//!   on framework bugs; attribute-preserved fallbacks degrade
//!   silently to the runtime mount as today.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{console, Element};

use crate::directives::for_plan::{
    BindingKind, StaticBinding, StaticChildMount, StaticForPlan, StaticIfPlan, StaticInit,
    StaticInterp, StaticListener, StaticNativeModel, StaticOpaqueDirective, StaticRef,
    StaticSlotOutlet, StaticTeleportPlan,
};
use crate::directives::{self};
use crate::expr;
use crate::reactive::ScopeId;
use crate::slot_fragment::SlotSet;

// ─── macro-emitted static shape ─────────────────────────────────

/// Static-lifetime template plan emitted by `#[component]`. The
/// macro emits one `&'static StaticTemplatePlan` per component
/// alongside the existing `register_template` call:
///
/// ```ignore
/// pub static __POC_TEMPLATE_PLAN_<COMPONENT>: StaticTemplatePlan =
///     StaticTemplatePlan {
///         bindings: &[ /* … */ ],
///         listeners: &[ /* … */ ],
///         inits: &[ /* … */ ],
///         refs: &[ /* … */ ],
///     };
/// ```
#[doc(hidden)]
pub struct StaticTemplatePlan {
    pub bindings: &'static [StaticBinding],
    pub listeners: &'static [StaticListener],
    pub inits: &'static [StaticInit],
    pub refs: &'static [StaticRef],
    /// Child-component mount sites the macro discovered in this
    /// template (RFC-058 Phase 3). Each entry names a non-HTML5
    /// tag the runtime applier mounts via
    /// [`crate::mount::mount_child_component`] before the mount
    /// recurses. Empty for templates that contain no child
    /// components — the prior Phase 2 envelope.
    pub child_mounts: &'static [StaticChildMount],
    /// `pp-if` controller sites the classifier lifted out of
    /// the runtime mount's directive-dispatch path (RFC-058
    /// Phase 4.1b). The macro strips the `pp-if` attribute from
    /// the cleaned HTML — the applier installs the effect via
    /// [`crate::directives::if_::install`] against the
    /// `<template>` element resolved through `template_node_path`.
    /// Empty for templates with no `pp-if` site.
    pub if_plans: &'static [StaticIfPlan],
    /// `pp-for` controller sites (RFC-058 Phase 4.2). The macro
    /// strips `pp-for` / `pp-key` / `pp-stagger` from the
    /// cleaned HTML; the applier installs the effect via
    /// [`crate::directives::for_::install`] with the parsed
    /// item / items / key / stagger pre-resolved. The
    /// `data-pp-row-plan` attribute the §6.2 layering bakes
    /// in stays alongside, so the RFC-054 row-plan registry
    /// still resolves keyed lists. Empty for templates with no
    /// `pp-for` site.
    pub for_plans: &'static [StaticForPlan],
    /// `pp-teleport` controller sites without a co-occurring
    /// `pp-if` (RFC-058 Phase 4.3). The macro strips
    /// `pp-teleport` from the cleaned HTML; the applier resolves
    /// the target selector via
    /// [`crate::directives::teleport::install`]. Empty for
    /// templates with no plan-eligible `pp-teleport` site.
    pub teleport_plans: &'static [StaticTeleportPlan],
    /// `<slot>` outlet sites in a compiled component template
    /// (RFC-058 Phase 3.5e). These are materialised explicitly
    /// by the plan applier after all other path-resolved entries
    /// have installed, so the recursive mount no longer has to
    /// discover `<slot>` elements for planned templates.
    pub slot_outlets: &'static [StaticSlotOutlet],
    /// Runtime-only directives the macro lifts via the directive
    /// registry (RFC-058 Phase 3 hardening). One entry per
    /// allowlisted `pp-X` attribute (currently `pp-roving`,
    /// `pp-resize`, `pp-intersect`, `pp-anchor`, `pp-flip`); the
    /// applier dispatches them through
    /// [`crate::directives::lookup`] after slot materialisation
    /// so container directives that walk descendants find a fully
    /// settled DOM. Empty for plans whose elements use only the
    /// macro's structured directives.
    pub opaque_directives: &'static [StaticOpaqueDirective],
    /// `{{expr}}` text interpolation sites lifted out of the
    /// runtime mount (RFC-058 Phase 6.2). The macro pre-parses
    /// the segment list at compile time; the applier hands it
    /// to [`crate::directives::interp::install_planned`] to
    /// install effects per dynamic segment. Empty for templates
    /// with no interpolation.
    pub interps: &'static [StaticInterp],
    /// `pp-model[.modifier]="field"` sites on native input /
    /// textarea / select elements. Lifted out of the runtime
    /// mount so compiled-only apps wire two-way input bindings
    /// without the runtime mount. Component-target `pp-model`
    /// is on `StaticChildMount` instead.
    pub native_models: &'static [StaticNativeModel],
}

// ─── registry ────────────────────────────────────────────────────

thread_local! {
    static TEMPLATE_PLANS: RefCell<HashMap<String, &'static StaticTemplatePlan>> =
        RefCell::new(HashMap::new());

    /// Counter for fail-fast-but-recover events in release builds
    /// — incremented when an `apply_static_plan` install fails
    /// (`node_path` doesn't resolve, `expr_src` doesn't parse).
    /// Tests assert this stays zero across mount/unmount cycles
    /// for templates the macro stripped attributes from. Public
    /// reader: [`plan_failure_count`].
    static PLAN_FAILURES: Cell<u32> = const { Cell::new(0) };
}

/// Register a template plan against a component tag. Called by
/// macro-emitted code immediately after `register_template` for
/// every `#[component]` whose template has at least one
/// plan-eligible directive (RFC-058 Phase 2 §6 envelope).
///
/// Idempotent — repeat calls overwrite the prior entry.
pub fn register_template_plan(tag: &str, plan: &'static StaticTemplatePlan) {
    TEMPLATE_PLANS.with(|registry| {
        registry.borrow_mut().insert(tag.to_string(), plan);
    });
}

/// Look up a registered template plan by component tag. Returns
/// `None` for components the macro chose not to plan (e.g.
/// templates entirely inside non-HTML-native subtrees, or
/// pre-RFC-058 components compiled before the plan emitter
/// existed).
pub fn template_plan_for(tag: &str) -> Option<&'static StaticTemplatePlan> {
    if let Some(plan) = crate::registry::active_component_vtable(tag).and_then(|v| v.plan) {
        return Some(plan);
    }
    TEMPLATE_PLANS.with(|registry| registry.borrow().get(tag).copied())
}

/// Snapshot every registered component tag. Order is
/// implementation-defined (HashMap iteration); callers that
/// need stable ordering should sort. Intended for audit /
/// survey tooling and compiled root discovery.
pub fn registered_template_tags() -> Vec<String> {
    let mut tags: Vec<String> = crate::registry::active_component_names()
        .into_iter()
        .filter(|name| {
            crate::registry::active_component_vtable(name)
                .and_then(|v| v.plan)
                .is_some()
        })
        .map(str::to_string)
        .collect();
    TEMPLATE_PLANS.with(|registry| {
        for tag in registry.borrow().keys() {
            if !tags.iter().any(|existing| existing == tag) {
                tags.push(tag.clone());
            }
        }
    });
    tags
}

/// Cumulative count of plan-install failures observed since
/// process start. Increments via [`record_plan_failure`] when an
/// `apply_static_plan` entry's `node_path` doesn't resolve to a
/// live DOM node, or its `expr_src` doesn't parse — both are
/// framework bugs (the macro stripped a directive whose plan
/// entry can't deliver). Tests can assert this stays zero
/// across mount/unmount cycles for compiled components.
pub fn plan_failure_count() -> u32 {
    PLAN_FAILURES.with(|c| c.get())
}

/// Reset the plan-failure counter. Tests call this between
/// cases so a prior test's failure doesn't leak into the next
/// case's assertion. Production code should never call this.
#[doc(hidden)]
pub fn reset_plan_failure_count() {
    PLAN_FAILURES.with(|c| c.set(0));
}

/// Record one plan-install failure. Called from
/// [`crate::mount`] / [`apply_static_plan`] when an install
/// entry can't deliver. In debug builds this also panics with
/// a message naming the template + entry; release builds log
/// to `console.error` and continue (the surrounding mount
/// keeps going so a single misclassification doesn't take
/// down the page).
#[doc(hidden)]
pub fn record_plan_failure() {
    PLAN_FAILURES.with(|c| c.set(c.get().saturating_add(1)));
}

// ─── runtime apply ───────────────────────────────────────────────

/// Apply a registered template plan against the freshly-stamped
/// subtree rooted at `root`. Called from
/// [`crate::mount::mount_component`]'s fast-path right after
/// the template HTML is set + the scope is bound.
///
/// Behaviour:
///
/// * For each `StaticInit` — enqueue via
///   [`crate::mount::defer_init_on`] so the handler fires
///   post-order alongside any mount-discovered `pp-init`.
/// * For each `StaticRef` — register against the scope's ref
///   table via [`crate::refs::register`].
/// * For each `StaticBinding` — install the matching directive
///   helper (`text` / `html` / `bind` / `show`) using the
///   parsed expression AST.
/// * For each `StaticListener` — install via
///   [`crate::directives::on::install`] with the parsed AST
///   wrapped in `Rc`.
///
/// Fail-fast policy (RFC-057 §5.6 council pass 3):
/// stripped attributes are owned by the plan. If a `node_path`
/// doesn't resolve to a live DOM node or an `expr_src` doesn't
/// parse — both are framework bugs (the macro stripped a
/// directive whose plan entry can't deliver). Debug builds
/// panic with a message naming the template + entry; release
/// builds log to `console.error`, increment the
/// [`plan_failure_count`] counter, and abandon the install for
/// that single entry. The surrounding mount continues so a
/// single misclassification doesn't take the whole app down.
pub fn apply_static_plan(
    root: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    plan: &'static StaticTemplatePlan,
    template_name: &str,
) {
    // Slot materialisation mutates the element-child list by
    // replacing `<slot>` with author/default content. Snapshot
    // the outlet elements up front, then materialise them only
    // after every other plan entry has resolved its path.
    let mut slot_outlets: Vec<Element> = Vec::with_capacity(plan.slot_outlets.len());
    for s in plan.slot_outlets {
        let Some(el) = resolve(root, s.node_path) else {
            fail("slot-outlet", template_name, s.node_path, Some(s.name));
            continue;
        };
        if el.local_name() != "slot" {
            fail("slot-outlet-tag", template_name, s.node_path, Some(s.name));
            continue;
        }
        slot_outlets.push(el);
    }

    // Order matches the mount's pre-/post-order intuition: refs
    // first (so a planned `pp-ref` is visible to any planned
    // `pp-init` further down), then bindings (effects subscribe
    // before any synchronous trigger), then listeners
    // (delegation surface ready before user interaction), then
    // child mounts (RFC-058 Phase 3 — explicit
    // `mount_child_component` calls before the mount's
    // recursive descent reaches each `<custom-tag>`; the
    // mount's `__pp_mounted` guard makes the discovery a
    // no-op for tags this loop already mounted), then inits
    // (enqueued for the mount's post-order drain so the
    // handler observes child mounts as well as planned refs).
    for r in plan.refs {
        let Some(el) = resolve(root, r.node_path) else {
            fail("ref", template_name, r.node_path, None);
            continue;
        };
        crate::refs::register(scope_id, r.name, &el);
    }
    for b in plan.bindings {
        let Some(el) = resolve(root, b.node_path) else {
            fail("binding", template_name, b.node_path, Some(b.expr_src));
            continue;
        };
        let ast = match expr::parse_cached(b.expr_src) {
            Ok(a) => a,
            Err(_) => {
                fail(
                    "binding-parse",
                    template_name,
                    b.node_path,
                    Some(b.expr_src),
                );
                continue;
            }
        };
        match b.kind {
            BindingKind::Text => directives::text::install(&el, proxy, ast),
            BindingKind::Html => directives::html::install(&el, proxy, ast),
            BindingKind::Show => directives::show::install(&el, proxy, ast),
            BindingKind::Bind { arg } => directives::bind::install(&el, proxy, arg, ast),
            BindingKind::Class => {
                // RFC-054 row plans use `Class`; template plans
                // emit `Bind { arg: "class" }`. Reaching this
                // branch via an apply_static_plan call means a
                // macro bug let a row-only kind into the
                // template plan. Treat as fail-fast.
                fail("binding-kind", template_name, b.node_path, Some(b.expr_src));
            }
        }
    }
    for l in plan.listeners {
        let Some(el) = resolve(root, l.node_path) else {
            fail("listener", template_name, l.node_path, Some(l.expr_src));
            continue;
        };
        let ast = match expr::parse_cached(l.expr_src) {
            Ok(a) => a,
            Err(_) => {
                fail(
                    "listener-parse",
                    template_name,
                    l.node_path,
                    Some(l.expr_src),
                );
                continue;
            }
        };
        let modifiers: Vec<String> = l.modifiers.iter().map(|s| (*s).to_string()).collect();
        directives::on::install(
            &el,
            scope_id,
            proxy,
            l.event,
            &modifiers,
            Rc::new(directives::on::backfill_legacy_call(ast)),
        );
    }
    for c in plan.child_mounts {
        let Some(el) = resolve(root, c.node_path) else {
            fail("child-mount", template_name, c.node_path, Some(c.tag));
            continue;
        };
        if c.slots.is_empty() {
            crate::mount::mount_child_component(&el, c.tag);
        } else {
            let mut set = SlotSet::new();
            for s in c.slots {
                set = match s.scoped_let {
                    Some(let_ident) => set.scoped(s.name, s.fragment, let_ident),
                    None => set.named(s.name, s.fragment),
                };
            }
            crate::mount::mount_child_component_with_slots(&el, c.tag, set, scope_id, proxy);
        }
        install_child_host_directives(&el, scope_id, proxy, c, template_name);
    }
    for fp in plan.for_plans {
        let Some(el) = resolve(root, fp.template_node_path) else {
            fail(
                "for-plan",
                template_name,
                fp.template_node_path,
                Some(fp.items_expr),
            );
            continue;
        };
        let template = match el.dyn_into::<web_sys::HtmlTemplateElement>() {
            Ok(t) => t,
            Err(_) => {
                fail(
                    "for-plan-template",
                    template_name,
                    fp.template_node_path,
                    Some(fp.items_expr),
                );
                continue;
            }
        };
        directives::for_::install(
            template,
            proxy.clone(),
            scope_id,
            fp.item_name.to_string(),
            fp.items_expr.to_string(),
            fp.key_expr.map(|s| s.to_string()),
            fp.stagger_ms,
            fp.body,
        );
    }
    for tp in plan.teleport_plans {
        let Some(el) = resolve(root, tp.template_node_path) else {
            fail(
                "teleport-plan",
                template_name,
                tp.template_node_path,
                Some(tp.selector),
            );
            continue;
        };
        let template = match el.dyn_into::<web_sys::HtmlTemplateElement>() {
            Ok(t) => t,
            Err(_) => {
                fail(
                    "teleport-plan-template",
                    template_name,
                    tp.template_node_path,
                    Some(tp.selector),
                );
                continue;
            }
        };
        directives::teleport::install(template, tp.selector, tp.body);
    }
    for ip in plan.if_plans {
        let Some(el) = resolve(root, ip.template_node_path) else {
            fail(
                "if-plan",
                template_name,
                ip.template_node_path,
                Some(ip.expr_src),
            );
            continue;
        };
        let template = match el.dyn_into::<web_sys::HtmlTemplateElement>() {
            Ok(t) => t,
            Err(_) => {
                fail(
                    "if-plan-template",
                    template_name,
                    ip.template_node_path,
                    Some(ip.expr_src),
                );
                continue;
            }
        };
        let ast = match expr::parse_cached(ip.expr_src) {
            Ok(a) => a,
            Err(_) => {
                fail(
                    "if-plan-parse",
                    template_name,
                    ip.template_node_path,
                    Some(ip.expr_src),
                );
                continue;
            }
        };
        directives::if_::install(template, proxy.clone(), ast, ip.body, ip.teleport_selector);
    }
    for i in plan.inits {
        let Some(el) = resolve(root, i.node_path) else {
            fail("init", template_name, i.node_path, Some(i.expr_src));
            continue;
        };
        crate::mount::defer_init_on(&el, scope_id, i.expr_src);
    }
    for slot in slot_outlets {
        crate::mount::materialize_compiled_slot_outlet(&slot);
    }
    // Install opaque runtime directives last so container
    // behaviours like `pp-roving` see the slot-materialised item
    // DOM in place (the legacy mount fired them after attribute
    // dispatch on each item, which happened post-slot-clone).
    install_opaque_directives(root, scope_id, proxy, plan, template_name);
    // RFC-058 Phase 6.2 — `{{expr}}` text interpolation. The
    // macro pre-parses segments and stamps `data-pp-interp-managed`
    // on the carrier element so the runtime mount's
    // `interp::scan_children` skips the duplicate scan.
    //
    // Resolve every target text node BEFORE installing any
    // segment list. Each `install_planned_target` inserts new
    // text-node siblings before the placeholder and then removes
    // the placeholder — reading `text_index` against the live
    // list after a sibling install lands on the wrong node when
    // multiple entries share a parent (e.g. a single carrier
    // with `a {{x}}<em></em>b {{y}}` emits two entries against
    // the same parent with text_index 0 and 1, but after the
    // first install the dynamic node injected for `x` occupies
    // text_index 1 in the live list).
    let mut interp_targets: Vec<(Element, web_sys::Text, &'static [_])> =
        Vec::with_capacity(plan.interps.len());
    for ip in plan.interps {
        let Some(el) = resolve(root, ip.node_path) else {
            fail("interp", template_name, ip.node_path, None);
            continue;
        };
        let Some(target) = directives::interp::resolve_text_target(&el, ip.text_index as usize)
        else {
            continue;
        };
        interp_targets.push((el, target, ip.segments));
    }
    for (el, target, segments) in &interp_targets {
        directives::interp::install_planned_target(el, proxy, target, segments);
    }
    // RFC-058 Phase 6.5 — `pp-model` on native inputs lifted out
    // of `mount::dispatch`. The macro already parsed the
    // modifier list (`.number`, `.lazy`); the applier installs
    // the read-side effect + write-side listener directly.
    for nm in plan.native_models {
        let Some(el) = resolve(root, nm.node_path) else {
            fail(
                "native pp-model",
                template_name,
                nm.node_path,
                Some(nm.expr_src),
            );
            continue;
        };
        directives::model::install_native(&el, proxy, nm.expr_src.to_string(), nm.number, nm.lazy);
    }
}

fn install_opaque_directives(
    root: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    plan: &'static StaticTemplatePlan,
    template_name: &str,
) {
    for d in plan.opaque_directives {
        let Some(el) = resolve(root, d.node_path) else {
            fail("opaque-directive", template_name, d.node_path, Some(d.name));
            continue;
        };
        let modifiers: Vec<String> = d.modifiers.iter().map(|s| (*s).to_string()).collect();
        if !dispatch_opaque(d.name, &el, d.arg, &modifiers, d.value, scope_id, proxy) {
            // The macro emitted an entry for a directive the
            // dispatch table doesn't know about — either a
            // typo'd allowlist entry or a directive that was
            // removed. Surface it via the fail-fast counter so
            // tests catch the drift.
            fail(
                "opaque-directive-lookup",
                template_name,
                d.node_path,
                Some(d.name),
            );
        }
    }
}

/// Dispatch an opaque directive lift to its typed install entry.
/// Returns `false` when `name` doesn't match a known opaque
/// directive — caller treats that as a fail-fast event.
fn dispatch_opaque(
    name: &str,
    el: &Element,
    arg: Option<&str>,
    modifiers: &[String],
    value: &str,
    scope_id: ScopeId,
    proxy: &JsValue,
) -> bool {
    match name {
        "resize" => directives::resize::install_opaque(el, arg, modifiers, value, scope_id, proxy),
        "intersect" => {
            directives::intersect::install_opaque(el, arg, modifiers, value, scope_id, proxy)
        }
        "anchor" => directives::anchor::install_opaque(el, arg, modifiers, value, scope_id, proxy),
        "roving" => directives::roving::install_opaque(el, arg, modifiers, value, scope_id, proxy),
        "flip" => directives::flip::install_opaque(el, arg, modifiers, value, scope_id, proxy),
        _ => return false,
    }
    true
}

/// Apply the root-owned part of a template plan to the user
/// element hoisted by `pp-as`.
///
/// In `pp-as` mode the user element is already the slot content,
/// so the normal plan applier must not materialise `<slot>` outlets
/// or descend into template children. Root-level refs, bindings,
/// listeners, and deferred inits still belong to the component
/// template and should bind against the hoisted element in the
/// component scope.
#[doc(hidden)]
pub fn apply_static_pp_as_plan(
    root: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    plan: &'static StaticTemplatePlan,
    template_name: &str,
) {
    for r in plan.refs.iter().filter(|r| r.node_path.is_empty()) {
        crate::refs::register(scope_id, r.name, root);
    }
    for b in plan.bindings.iter().filter(|b| b.node_path.is_empty()) {
        let ast = match expr::parse_cached(b.expr_src) {
            Ok(a) => a,
            Err(_) => {
                fail(
                    "pp-as-binding-parse",
                    template_name,
                    b.node_path,
                    Some(b.expr_src),
                );
                continue;
            }
        };
        match b.kind {
            BindingKind::Text => directives::text::install(root, proxy, ast),
            BindingKind::Html => directives::html::install(root, proxy, ast),
            BindingKind::Show => directives::show::install(root, proxy, ast),
            BindingKind::Bind { arg } => directives::bind::install(root, proxy, arg, ast),
            BindingKind::Class => {
                fail(
                    "pp-as-binding-kind",
                    template_name,
                    b.node_path,
                    Some(b.expr_src),
                );
            }
        }
    }
    for l in plan.listeners.iter().filter(|l| l.node_path.is_empty()) {
        let ast = match expr::parse_cached(l.expr_src) {
            Ok(a) => a,
            Err(_) => {
                fail(
                    "pp-as-listener-parse",
                    template_name,
                    l.node_path,
                    Some(l.expr_src),
                );
                continue;
            }
        };
        let modifiers: Vec<String> = l.modifiers.iter().map(|s| (*s).to_string()).collect();
        directives::on::install(
            root,
            scope_id,
            proxy,
            l.event,
            &modifiers,
            Rc::new(directives::on::backfill_legacy_call(ast)),
        );
    }
    for i in plan.inits.iter().filter(|i| i.node_path.is_empty()) {
        crate::mount::defer_init_on(root, scope_id, i.expr_src);
    }
    for d in plan
        .opaque_directives
        .iter()
        .filter(|d| d.node_path.is_empty())
    {
        let modifiers: Vec<String> = d.modifiers.iter().map(|s| (*s).to_string()).collect();
        if !dispatch_opaque(d.name, root, d.arg, &modifiers, d.value, scope_id, proxy) {
            fail(
                "opaque-directive-lookup",
                template_name,
                d.node_path,
                Some(d.name),
            );
        }
    }
}

fn install_child_host_directives(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    child: &StaticChildMount,
    template_name: &str,
) {
    for b in child.bindings {
        let ast = match expr::parse_cached(b.expr_src) {
            Ok(a) => a,
            Err(_) => {
                fail(
                    "child-host-binding-parse",
                    template_name,
                    child.node_path,
                    Some(b.expr_src),
                );
                continue;
            }
        };
        directives::bind::install(el, proxy, b.arg, ast);
    }
    for l in child.listeners {
        let ast = match expr::parse_cached(l.expr_src) {
            Ok(a) => a,
            Err(_) => {
                fail(
                    "child-host-listener-parse",
                    template_name,
                    child.node_path,
                    Some(l.expr_src),
                );
                continue;
            }
        };
        let modifiers: Vec<String> = l.modifiers.iter().map(|s| (*s).to_string()).collect();
        directives::on::install(
            el,
            scope_id,
            proxy,
            l.event,
            &modifiers,
            Rc::new(directives::on::backfill_legacy_call(ast)),
        );
    }
    for m in child.models {
        let modifiers: Vec<String> = m.modifiers.iter().map(|s| (*s).to_string()).collect();
        let _ = scope_id;
        directives::model::install_compiled(el, proxy, m.arg, &modifiers, m.expr_src);
    }
}

/// Walk `node_path` from `root` by element-child indices to
/// reach the live DOM node the plan entry targets. Returns
/// `None` when any index is out-of-bounds — caller treats that
/// as a fail-fast event.
fn resolve(root: &Element, node_path: &[u16]) -> Option<Element> {
    let mut current: Element = root.clone();
    for &idx in node_path {
        current = current.children().item(idx as u32)?;
    }
    Some(current)
}

/// RFC-058 Phase 4.1d — `pp-if` body fragment runtime helper.
///
/// Parses `html` (the macro-cleaned body markup) into a fresh
/// `<template>`'s content, extracts the single root element,
/// then applies the per-body `StaticTemplatePlan` against the
/// parent scope so every directive in the body installs via
/// the Phase 1 helpers (no mount `walk` / `bind` /
/// `dispatch` involvement). Returns the root element ready
/// for the `if_::install` caller to pin its borrowed scope on
/// + insert into the live DOM.
///
/// `None` return signals the body HTML didn't parse to a
/// single root element — same shape as the legacy
/// `clone_template_body` miss; the caller surfaces a console
/// error and bails the mount.
pub fn stamp_if_body(
    html: &str,
    plan: &'static StaticTemplatePlan,
    scope_id: ScopeId,
    proxy: &JsValue,
    ctx_parent_id: ScopeId,
) -> Option<Element> {
    let doc = web_sys::window().and_then(|w| w.document())?;
    let template_el = doc.create_element("template").ok()?;
    template_el.set_inner_html(html);
    let template_el = template_el
        .dyn_into::<web_sys::HtmlTemplateElement>()
        .ok()?;
    let content = template_el.content();
    let kids = content.child_nodes();
    let mut root: Option<Element> = None;
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i) {
            if let Ok(el) = n.dyn_into::<Element>() {
                root = Some(el);
                break;
            }
        }
    }
    let root = root?;
    crate::mount::bind_borrowed_scope_to(&root, scope_id, proxy);
    // CTX_PARENT_KEY drives RFC-027 inject chain resolution for
    // nested custom tags inside this body fragment. Callers pass
    // `ctx_parent_id` distinct from `scope_id` when the controller
    // template was authored in slot content — `scope_id` is the
    // slot AUTHOR's scope (so directives bind against the right
    // proxy) but the inject parent must chain through the slot
    // OWNER. For pp-for body fragments `ctx_parent_id` equals the
    // LoopScope id; the loop scope's `parent` is already set to
    // the resolved inject parent at install time, so the chain
    // walks correctly from there.
    let ctx_key = JsValue::from_str(crate::mount::CTX_PARENT_KEY);
    let ctx_val = JsValue::from_f64(ctx_parent_id.0 as f64);
    let _ = js_sys::Reflect::set(root.as_ref(), &ctx_key, &ctx_val);
    apply_static_plan(&root, scope_id, proxy, plan, "<pp-if body>");
    Some(root)
}

fn fail(kind: &str, template_name: &str, node_path: &[u16], expr_src: Option<&str>) {
    record_plan_failure();
    let msg = match expr_src {
        Some(src) => format!(
            "pocopine: template plan {kind} install failed for `{template_name}` at \
             node_path {node_path:?} (expr: {src:?}). This is a framework bug — the \
             macro stripped a directive whose plan entry cannot deliver."
        ),
        None => format!(
            "pocopine: template plan {kind} install failed for `{template_name}` at \
             node_path {node_path:?}. This is a framework bug — the macro stripped a \
             directive whose plan entry cannot deliver."
        ),
    };
    if cfg!(debug_assertions) {
        panic!("{msg}");
    }
    console::error_1(&JsValue::from_str(&msg));
}
