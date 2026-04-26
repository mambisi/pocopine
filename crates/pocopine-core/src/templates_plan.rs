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

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{console, Element};

use crate::directives;
use crate::directives::for_plan::{
    BindingKind, StaticBinding, StaticChildMount, StaticForPlan, StaticIfPlan, StaticInit,
    StaticListener, StaticRef,
};
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
                set = set.named(s.name, s.fragment);
            }
            crate::walker::mount_child_component_with_slots(&el, c.tag, set);
        }
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
        );
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
        directives::if_::install(template, proxy.clone(), ast);
    }
    for i in plan.inits {
        let Some(el) = resolve(root, i.node_path) else {
            fail("init", template_name, i.node_path, Some(i.expr_src));
            continue;
        };
        crate::walker::defer_init_on(&el, scope_id, i.expr_src);
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
