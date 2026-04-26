//! RFC-058 Phase 2 — compiled template plans.
//!
//! Macro-emitted static descriptor for an entire compiled
//! template's bindings, listeners, refs, and deferred-init
//! entries. The runtime fast-path in [`crate::walker::mount_component`]
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
//! * Not eligible (attribute-preserved, walker-owned): every
//!   directive on or under a non-HTML-native tag, every
//!   directive on `pp-for` / `pp-if` / `pp-teleport` / `<slot>`
//!   subtrees, `pp-model`, `pp-route`, listeners with
//!   unsupported modifiers.
//! * Stripped attributes are owned by the plan and **fail fast**
//!   on framework bugs; attribute-preserved fallbacks degrade
//!   silently to the runtime walker as today.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::Reflect;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{console, Element};

use crate::directives::for_plan::{
    BindingKind, StaticBinding, StaticChildMount, StaticForPlan, StaticIfPlan, StaticInit,
    StaticListener, StaticOpaqueDirective, StaticRef, StaticSlotOutlet, StaticTeleportPlan,
};
use crate::directives::{self, DirectiveCall};
use crate::expr;
use crate::reactive::ScopeId;
use crate::slot_fragment::SlotSet;

const COMPILED_FALLBACK_WALK_KEY: &str = "__pp_compiled_fallback_walk";

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
    /// [`crate::walker::mount_child_component`] before the walker
    /// recurses. Empty for templates that contain no child
    /// components — the prior Phase 2 envelope.
    pub child_mounts: &'static [StaticChildMount],
    /// `pp-if` controller sites the classifier lifted out of
    /// the runtime walker's directive-dispatch path (RFC-058
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
    /// have installed, so the recursive walker no longer has to
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
    /// True when the cleaned HTML still contains framework-owned
    /// attributes that this plan does not install. Compiled
    /// fragments can skip fallback only when their own plan and
    /// every planned child component are walker-complete.
    pub requires_walker: bool,
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
    TEMPLATE_PLANS.with(|registry| registry.borrow().get(tag).copied())
}

/// Snapshot every registered component tag. Order is
/// implementation-defined (HashMap iteration); callers that
/// need stable ordering should sort. Intended for audit /
/// survey tooling — RFC-058 Phase 3 hardening uses it to
/// enumerate which compiled plans still trip
/// [`StaticTemplatePlan::requires_walker`] so the migration to
/// a walker-free runtime can target the remaining offenders.
pub fn registered_template_tags() -> Vec<String> {
    TEMPLATE_PLANS.with(|registry| registry.borrow().keys().cloned().collect())
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
/// [`crate::walker`] / [`apply_static_plan`] when an install
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
/// [`crate::walker::mount_component`]'s fast-path right after
/// the template HTML is set + the scope is bound.
///
/// Behaviour:
///
/// * For each `StaticInit` — enqueue via
///   [`crate::walker::defer_init_on`] so the handler fires
///   post-order alongside any walker-discovered `pp-init`.
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

    // Order matches the walker's pre-/post-order intuition: refs
    // first (so a planned `pp-ref` is visible to any planned
    // `pp-init` further down), then bindings (effects subscribe
    // before any synchronous trigger), then listeners
    // (delegation surface ready before user interaction), then
    // child mounts (RFC-058 Phase 3 — explicit
    // `mount_child_component` calls before the walker's
    // recursive descent reaches each `<custom-tag>`; the
    // walker's `__pp_mounted` guard makes the discovery a
    // no-op for tags this loop already mounted), then inits
    // (enqueued for the walker's post-order drain so the
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
            crate::walker::mount_child_component(&el, c.tag);
        } else {
            let mut set = SlotSet::new();
            for s in c.slots {
                set = match s.scoped_let {
                    Some(let_ident) => set.scoped(s.name, s.fragment, let_ident),
                    None => set.named(s.name, s.fragment),
                };
            }
            crate::walker::mount_child_component_with_slots(&el, c.tag, set, scope_id, proxy);
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
        crate::walker::defer_init_on(&el, scope_id, i.expr_src);
    }
    for slot in slot_outlets {
        crate::walker::materialize_compiled_slot_outlet(&slot);
    }
    // Install opaque runtime directives last so container
    // behaviours like `pp-roving` see the slot-materialised item
    // DOM in place (the legacy walker fired them after attribute
    // dispatch on each item, which happened post-slot-clone).
    install_opaque_directives(root, scope_id, proxy, plan, template_name);
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
        let Some(handler) = directives::lookup(d.name) else {
            // The macro emitted an entry for a directive the
            // runtime registry doesn't know about — either a
            // typo'd allowlist entry or a directive that was
            // removed from the registry. Surface it via the
            // fail-fast counter so tests catch the drift.
            fail(
                "opaque-directive-lookup",
                template_name,
                d.node_path,
                Some(d.name),
            );
            continue;
        };
        let modifiers: Vec<String> = d.modifiers.iter().map(|s| (*s).to_string()).collect();
        let call = DirectiveCall {
            el: &el,
            proxy,
            scope_id,
            arg: d.arg.map(|s| s.to_string()),
            modifiers,
            value: d.value.to_string(),
        };
        handler(&call);
    }
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
        crate::walker::defer_init_on(root, scope_id, i.expr_src);
    }
    for d in plan
        .opaque_directives
        .iter()
        .filter(|d| d.node_path.is_empty())
    {
        let Some(handler) = directives::lookup(d.name) else {
            fail(
                "opaque-directive-lookup",
                template_name,
                d.node_path,
                Some(d.name),
            );
            continue;
        };
        let modifiers: Vec<String> = d.modifiers.iter().map(|s| (*s).to_string()).collect();
        let call = DirectiveCall {
            el: root,
            proxy,
            scope_id,
            arg: d.arg.map(|s| s.to_string()),
            modifiers,
            value: d.value.to_string(),
        };
        handler(&call);
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
        let call = DirectiveCall {
            el,
            proxy,
            scope_id,
            arg: m.arg.map(str::to_string),
            modifiers,
            value: m.expr_src.to_string(),
        };
        directives::model::run(&call);
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
/// the Phase 1 helpers (no walker `walk` / `bind` /
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
    crate::walker::bind_borrowed_scope_to(&root, scope_id, proxy);
    let ctx_key = JsValue::from_str(crate::walker::CTX_PARENT_KEY);
    let ctx_val = JsValue::from_f64(scope_id.0 as f64);
    let _ = js_sys::Reflect::set(root.as_ref(), &ctx_key, &ctx_val);
    apply_static_plan(&root, scope_id, proxy, plan, "<pp-if body>");
    if plan_requires_compiled_fallback(plan) {
        let _ = Reflect::set(
            root.as_ref(),
            &JsValue::from_str(COMPILED_FALLBACK_WALK_KEY),
            &JsValue::TRUE,
        );
    }
    Some(root)
}

/// True when a fragment plan still needs the temporary runtime
/// discovery bridge after `apply_static_plan` runs. Native-only
/// bindings/listeners/refs are complete after the plan install;
/// nested components/controllers still need walker-backed
/// lifecycle, slot outlet, and preserved-directive handling until
/// those are compiled explicitly.
pub(crate) fn plan_requires_compiled_fallback(plan: &StaticTemplatePlan) -> bool {
    if plan.requires_walker {
        return true;
    }
    plan.child_mounts.iter().any(|c| {
        template_plan_for(c.tag)
            .map(plan_requires_compiled_fallback)
            .unwrap_or(true)
    })
}

/// Read the marker set by [`stamp_if_body`] for lifted controller
/// body fragments.
pub(crate) fn fragment_requires_compiled_fallback(root: &Element) -> bool {
    Reflect::get(
        root.as_ref(),
        &JsValue::from_str(COMPILED_FALLBACK_WALK_KEY),
    )
    .ok()
    .map(|v| v.is_truthy())
    .unwrap_or(false)
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
