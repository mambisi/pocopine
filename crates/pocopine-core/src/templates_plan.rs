//! RFC-058 Phase 2 — compiled template plans.
//!
//! Macro-emitted static descriptor for an entire compiled
//! template's bindings, listeners, refs, and deferred-init
//! entries. The runtime fast-path in `mount::mount_component`
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
//! * Eligible: native HTML/SVG elements; `pp-text`, `pp-html`,
//!   `pp-show`, `pp-bind:<attr>` (DOM attrs, not child-component
//!   props), `pp-on:<event>` with the §6.1 supported modifier
//!   set, `pp-ref`.
//! * Not eligible (attribute-preserved, mount-owned): every
//!   directive on or under a non-HTML-native tag, every
//!   directive on `pp-for` / `pp-if` / `pp-teleport` / `<slot>`
//!   subtrees, `pp-model`, `pp-route`, listeners with
//!   unsupported modifiers.
//! * Stripped attributes are owned by the plan and **fail fast**
//!   on framework bugs; attribute-preserved fallbacks degrade
//!   silently to the runtime mount as today.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{console, Element, Node};

use crate::directives::for_plan::{
    BindingKind, MatchCase, StaticBinding, StaticChildMount, StaticCondPlan, StaticForPlan,
    StaticInterp, StaticListener, StaticMatchPlan, StaticNativeModel, StaticOpaqueDirective,
    StaticRef, StaticSlotOutlet, StaticTeleportPlan,
};
use crate::directives::interp::PlannedSegment;
use crate::directives::{self};
use crate::expr;
use crate::expr::StaticExpr;
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
///         refs: &[ /* … */ ],
///     };
/// ```
#[doc(hidden)]
pub struct StaticTemplatePlan {
    pub bindings: &'static [StaticBinding],
    pub listeners: &'static [StaticListener],
    pub refs: &'static [StaticRef],
    /// Child-component mount sites the macro discovered in this
    /// template (RFC-058 Phase 3). Each entry names a non-HTML5
    /// tag the runtime applier mounts via
    /// [`crate::mount::mount_child_component`] before the mount
    /// recurses. Empty for templates that contain no child
    /// components — the prior Phase 2 envelope.
    pub child_mounts: &'static [StaticChildMount],
    /// Conditional-chain controller sites (RFC-058 Phase 4.1b,
    /// RFC-094 chains). One entry per `pp-if [pp-else-if…]
    /// [pp-else]` chain; the applier installs the access-based
    /// controller via [`crate::directives::if_::install_cond`]
    /// against the chain-head `<template>`, which the controller
    /// swaps for a `<!--pp:cond-->` comment anchor. Empty for
    /// templates with no chain.
    pub if_plans: &'static [StaticCondPlan],
    /// RFC-094 Phase 3 — `pp-match` dispatch sites; same anchor
    /// and exactly-one-clone contract as `if_plans`, selected by
    /// serde-tag extraction.
    pub match_plans: &'static [StaticMatchPlan],
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
    /// RFC-095 W3b — `false` when the macro proved no install in
    /// this plan ever consults the scope proxy: only bindings /
    /// interps / refs, every expression `$`-free (so the W1
    /// scoped root reader resolves every root and the proxy
    /// fallback is unreachable). `mount_component` then skips
    /// `into_proxy()` entirely — no trap closures, no `Proxy` —
    /// and stamps only `SCOPE_ID_KEY`; `scope_of_element` /
    /// `enclosing_scope` lazy-mint on first dynamic need
    /// (devtools, a parent's prop write, …), the same contract
    /// RFC-054 proxy-elided rows already live under.
    pub needs_proxy: bool,
}

/// Captured interpolation target for RFC 062 specialized mount
/// bodies. The macro resolves every target first, then installs
/// them as a second pass to preserve the generic applier's text
/// index stability guarantee.
#[doc(hidden)]
pub struct StaticInterpTarget {
    pub parent: Element,
    pub text: web_sys::Text,
    pub segments: &'static [PlannedSegment],
}

// ─── registry ────────────────────────────────────────────────────

thread_local! {
    static TEMPLATE_PLANS: RefCell<HashMap<&'static str, &'static StaticTemplatePlan>> =
        RefCell::new(HashMap::new());

    /// Counter for fail-fast-but-recover events in release builds
    /// — incremented when a generated install helper fails
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
pub fn register_template_plan(tag: &'static str, plan: &'static StaticTemplatePlan) {
    TEMPLATE_PLANS.with(|registry| {
        registry.borrow_mut().insert(tag, plan);
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
    let mut seen: HashSet<String> = tags.iter().cloned().collect();
    TEMPLATE_PLANS.with(|registry| {
        for tag in registry.borrow().keys() {
            if seen.insert((*tag).to_string()) {
                tags.push((*tag).to_string());
            }
        }
    });
    tags
}

/// Cumulative count of plan-install failures observed since
/// process start. Increments via [`record_plan_failure`] when a
/// generated install entry's `node_path` doesn't resolve to a
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

/// Record one plan-install failure. Called from generated mount
/// helpers when an install entry can't deliver. In debug builds
/// this also panics with a message naming the template + entry;
/// release builds log to `console.error` and continue (the
/// surrounding mount keeps going so a single misclassification
/// doesn't take down the page).
#[doc(hidden)]
pub fn record_plan_failure() {
    PLAN_FAILURES.with(|c| c.set(c.get().saturating_add(1)));
}

/// RFC 062 Phase 2 helper used by macro-specialized mount
/// bodies. It preserves the generic applier's ref semantics
/// after the macro has resolved the target element directly.
#[doc(hidden)]
pub fn install_static_ref(
    el: &Element,
    scope_id: ScopeId,
    entry: &'static StaticRef,
    _template_name: &str,
) {
    crate::refs::register(scope_id, entry.name, el);
}

/// RFC 062 Phase 2 helper used by macro-specialized mount
/// bodies. Parsing and fail-fast behaviour intentionally match
/// the generic applier.
#[doc(hidden)]
/// RFC-099 Phase 2c — resolve a `node_path` against an **existing** DOM
/// root, walking **element children only** (text/comment nodes don't
/// shift the index) — the same convention the macro's
/// `emit_specialized_resolve` and the row-plan resolver use, and which
/// matches the element-only `node_path` the plan records. An empty path
/// is the root itself. Used by the hydration claim walk to attach
/// bindings to server-rendered nodes without re-creating them.
///
/// `Element::children()` is the live HTMLCollection of element children
/// (the browser excludes text/comment nodes), so `.item(idx)` indexes
/// exactly as the client's `first_element_child`/`next_element_sibling`
/// walk does.
pub fn resolve_node_path(root: &Element, path: &[u16]) -> Option<Element> {
    let mut cur = root.clone();
    for &idx in path {
        cur = cur.children().item(idx as u32)?;
    }
    Some(cur)
}

/// RFC-099 Phase 2c — attach reactivity to a **server-rendered** subtree
/// under `root` without creating any DOM (the "claim" walk). For each
/// binding / interpolation / listener / ref the plan records, it
/// resolves the existing node by `node_path` and installs the directive
/// on it. Structural controllers (`pp-if` / `pp-for` / `pp-match`),
/// child-component mounts, and slots resolve **client-side** in Phase 2
/// and are skipped here.
///
/// Correctness: the installed binding effects re-evaluate the same state
/// the server rendered from, so their first run writes identical values
/// — the DOM stays byte-equal to the server output (verified by the
/// differential harness). The zero-initial-DOM-write refinement (via
/// [`crate::reactive::effect_hydrating`]) layers on top later.
pub fn hydrate_plan(
    root: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    plan: &'static StaticTemplatePlan,
    template_name: &str,
) {
    for r in plan.refs {
        if let Some(el) = resolve_node_path(root, r.node_path) {
            crate::refs::register(scope_id, r.name, &el);
        }
    }
    for b in plan.bindings {
        if let Some(el) = resolve_node_path(root, b.node_path) {
            install_static_binding(&el, scope_id, proxy, b, template_name);
        }
    }
    for it in plan.interps {
        if let Some(parent) = resolve_node_path(root, it.node_path) {
            if let Some(target) =
                directives::interp::resolve_text_target(&parent, it.text_index as usize)
            {
                // Resolve through the scope's proxy-free reader — same as
                // the binding evaluators and the client mount path. (A
                // `None` root + an elided `UNDEFINED` proxy would leave
                // dynamic segments unresolvable.)
                directives::interp::install_planned_target(
                    &parent,
                    proxy,
                    crate::scope::scoped_root_reader(scope_id),
                    &target,
                    it.segments,
                );
            }
        }
    }
    for l in plan.listeners {
        if let Some(el) = resolve_node_path(root, l.node_path) {
            install_static_listener(&el, scope_id, proxy, l, template_name);
        }
    }
    // RFC-099 Phase 3 — claim the structural controllers the server
    // stamped (pp-if chains so far). Each finds its decision anchor by
    // label, adopts the server-rendered clone, installs its body
    // effects, and seeds the controller so its first run is a no-op.
    for (idx, cp) in plan.if_plans.iter().enumerate() {
        hydrate_static_if_plan(root, scope_id, proxy, idx, cp, template_name);
    }
}

/// Find the `<!--label-->` comment among `parent`'s child nodes (the
/// decision anchor the SSR stamper emitted, e.g. `pp:cond:0`).
fn find_comment_anchor(parent: &Element, label: &str) -> Option<web_sys::Node> {
    let kids = parent.child_nodes();
    for i in 0..kids.length() {
        let node = kids.item(i)?;
        if node.node_type() == web_sys::Node::COMMENT_NODE
            && node.node_value().as_deref() == Some(label)
        {
            return Some(node);
        }
    }
    None
}

/// RFC-099 Phase 3 — claim a server-stamped `pp-if` chain. The server
/// rendered `[active-branch-clone]<!--pp:cond:idx-->` (member
/// `<template>`s dropped). Find the anchor, adopt the clone, install
/// the active branch body's effects on it, and hand the seeded chain
/// to [`directives::if_::hydrate_cond`]. If the anchor is absent the
/// server left the chain unexpanded (an unliftable branch) — fall back
/// to a normal client mount on the surviving `<template>`.
fn hydrate_static_if_plan(
    root: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    idx: usize,
    entry: &'static StaticCondPlan,
    template_name: &str,
) {
    let path = entry.template_node_path;
    if path.is_empty() {
        return; // scope-root template: SSR left it bare for client mount
    }
    let Some(parent) = resolve_node_path(root, &path[..path.len() - 1]) else {
        return;
    };
    let label = format!("pp:cond:{idx}");
    let Some(anchor) = find_comment_anchor(&parent, &label) else {
        // Unexpanded chain — the surviving <template> is at `path`.
        if let Some(tpl_el) = resolve_node_path(root, path) {
            install_static_if_plan(&tpl_el, scope_id, proxy, entry, template_name);
        }
        return;
    };

    // Branch evaluators (mirrors install_static_if_plan).
    let Some(head) = scoped_static_evaluator(scope_id, entry.compiled, entry.expr_src) else {
        fail(
            "hydrate-if-parse",
            template_name,
            path,
            Some(entry.expr_src),
        );
        return;
    };
    let mut branches: Vec<(
        directives::if_::BranchEval,
        Option<crate::directives::for_plan::IfBodyFn>,
    )> = Vec::with_capacity(1 + entry.else_if.len());
    branches.push((head, entry.body));
    for b in entry.else_if {
        let Some(eval) = scoped_static_evaluator(scope_id, b.compiled, b.expr_src) else {
            fail(
                "hydrate-else-if-parse",
                template_name,
                path,
                Some(b.expr_src),
            );
            return;
        };
        branches.push((eval, b.body));
    }

    // Active branch — client eval against the same state the server
    // rendered from, so it matches the adopted clone.
    let active = branches
        .iter()
        .position(|(e, _)| !e(proxy).is_falsy())
        .or_else(|| entry.has_else.then_some(branches.len()));
    let adopted = anchor
        .previous_sibling()
        .and_then(|n| n.dyn_into::<Element>().ok());
    let active_body_plan = match active {
        Some(0) => entry.body_plan,
        Some(i) if i <= entry.else_if.len() => entry.else_if[i - 1].body_plan,
        Some(_) => entry.else_body_plan,
        None => None,
    };
    if let (Some(el), Some(bp)) = (adopted.as_ref(), active_body_plan) {
        // The adopted clone IS the body root; install its bindings /
        // interps / nested controllers (recursive claim).
        hydrate_plan(el, scope_id, proxy, bp, template_name);
    }

    directives::if_::hydrate_cond(
        anchor,
        active,
        adopted,
        scope_id,
        proxy.clone(),
        branches,
        entry.has_else,
        entry.else_body,
        entry.teleport_selector,
    );
}

pub fn install_static_binding(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticBinding,
    template_name: &str,
) {
    let Some(evaluator) = scoped_static_evaluator(scope_id, entry.compiled, entry.expr_src) else {
        fail(
            "binding-parse",
            template_name,
            entry.node_path,
            Some(entry.expr_src),
        );
        return;
    };
    match entry.kind {
        BindingKind::Text => {
            // RFC-096 S3 — single-field pp-text takes the typed
            // lane (track + scalar extract, no serde-to-JS);
            // anything else keeps the evaluator path.
            if let Some(StaticExpr::Path([key])) = entry.compiled {
                directives::text::install_fast(el, scope_id, key, proxy, evaluator);
            } else {
                directives::text::install_eval(el, proxy, evaluator);
            }
        }
        BindingKind::Html => directives::html::install_eval(el, proxy, evaluator),
        BindingKind::Show => directives::show::install_eval(el, proxy, evaluator),
        BindingKind::Bind { arg } => directives::bind::install_eval(el, proxy, arg, evaluator),
        BindingKind::Class => fail(
            "binding-kind",
            template_name,
            entry.node_path,
            Some(entry.expr_src),
        ),
    }
}

/// RFC 062 Phase 2 helper used by macro-specialized mount
/// bodies. Parsing and legacy-call backfill stay shared with
/// the generic applier.
#[doc(hidden)]
pub fn install_static_listener(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticListener,
    template_name: &str,
) {
    let ast = match expr::parse_cached(entry.expr_src) {
        Ok(a) => a,
        Err(_) => {
            fail(
                "listener-parse",
                template_name,
                entry.node_path,
                Some(entry.expr_src),
            );
            return;
        }
    };
    directives::on::install(
        el,
        scope_id,
        proxy,
        entry.event,
        entry.modifiers,
        Rc::new(directives::on::backfill_legacy_call(ast)),
    );
}

/// RFC 062 Phase 2 helper used by macro-specialized mount
/// bodies for child-component hosts.
#[doc(hidden)]
pub fn install_static_child_mount(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticChildMount,
    template_name: &str,
) {
    if entry.slots.is_empty() {
        crate::mount::mount_child_component(el, entry.tag);
    } else {
        let mut set = SlotSet::new();
        for s in entry.slots {
            set = match s.scoped_let {
                Some(let_ident) => set.scoped(s.name, s.fragment, let_ident),
                None => set.named(s.name, s.fragment),
            };
        }
        crate::mount::mount_child_component_with_slots(el, entry.tag, set, scope_id, proxy);
    }
    install_child_host_directives(el, scope_id, proxy, entry, template_name);
}

/// RFC 062 Phase 2 helper used by macro-specialized mount
/// bodies for `pp-for` controller templates.
#[doc(hidden)]
pub fn install_static_for_plan(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticForPlan,
    template_name: &str,
) {
    let template = match directives::for_::ForTemplate::from_element(el.clone()) {
        Some(t) => t,
        None => {
            fail(
                "for-plan-template",
                template_name,
                entry.template_node_path,
                Some(entry.items_expr),
            );
            return;
        }
    };
    directives::for_::install(
        template,
        proxy.clone(),
        scope_id,
        entry.item_name,
        entry.items_expr,
        entry.key_expr,
        entry.stagger_ms,
        entry.body,
    );
}

/// RFC 062 Phase 2 helper used by macro-specialized mount
/// bodies for standalone `pp-teleport` controller templates.
#[doc(hidden)]
pub fn install_static_teleport_plan(
    el: &Element,
    entry: &'static StaticTeleportPlan,
    template_name: &str,
) {
    let template = match el.clone().dyn_into::<web_sys::HtmlTemplateElement>() {
        Ok(t) => t,
        Err(_) => {
            fail(
                "teleport-plan-template",
                template_name,
                entry.template_node_path,
                Some(entry.selector),
            );
            return;
        }
    };
    directives::teleport::install(template, entry.selector, entry.body);
}

/// RFC 062 / RFC-094 helper used by macro-specialized mount
/// bodies for conditional-chain controller templates. Builds one
/// access-based evaluator per branch and hands the chain to the
/// comment-anchored controller.
#[doc(hidden)]
pub fn install_static_if_plan(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticCondPlan,
    template_name: &str,
) {
    let template = match el.clone().dyn_into::<web_sys::HtmlTemplateElement>() {
        Ok(t) => t,
        Err(_) => {
            fail(
                "if-plan-template",
                template_name,
                entry.template_node_path,
                Some(entry.expr_src),
            );
            return;
        }
    };
    let mut branches: Vec<(
        directives::if_::BranchEval,
        Option<crate::directives::for_plan::IfBodyFn>,
    )> = Vec::with_capacity(1 + entry.else_if.len());
    let Some(head) = scoped_static_evaluator(scope_id, entry.compiled, entry.expr_src) else {
        fail(
            "if-plan-parse",
            template_name,
            entry.template_node_path,
            Some(entry.expr_src),
        );
        return;
    };
    branches.push((head, entry.body));
    for b in entry.else_if {
        let Some(eval) = scoped_static_evaluator(scope_id, b.compiled, b.expr_src) else {
            fail(
                "else-if-parse",
                template_name,
                entry.template_node_path,
                Some(b.expr_src),
            );
            return;
        };
        branches.push((eval, b.body));
    }
    directives::if_::install_cond(
        template,
        scope_id,
        proxy.clone(),
        branches,
        entry.has_else,
        entry.else_body,
        entry.consumed_count,
        entry.teleport_selector,
    );
}

/// RFC-094 Phase 3 helper for `pp-match` controller templates.
#[doc(hidden)]
pub fn install_static_match_plan(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticMatchPlan,
    template_name: &str,
) {
    let template = match el.clone().dyn_into::<web_sys::HtmlTemplateElement>() {
        Ok(t) => t,
        Err(_) => {
            fail(
                "match-plan-template",
                template_name,
                entry.template_node_path,
                Some(entry.expr_src),
            );
            return;
        }
    };
    let Some(evaluator) = scoped_static_evaluator(scope_id, entry.compiled, entry.expr_src) else {
        fail(
            "match-plan-parse",
            template_name,
            entry.template_node_path,
            Some(entry.expr_src),
        );
        return;
    };
    let arms: Vec<directives::if_::MatchArm> = entry
        .cases
        .iter()
        .map(|c: &MatchCase| directives::if_::MatchArm {
            tags: c.tags,
            bind_name: c.bind_name,
            body: c.body,
        })
        .collect();
    directives::if_::install_match(
        template,
        scope_id,
        proxy.clone(),
        evaluator,
        arms,
        entry.teleport_selector,
    );
}

/// RFC 062 Phase 3 helper used by macro-specialized mount
/// bodies to snapshot `<slot>` outlets before later
/// materialisation mutates element-child positions.
#[doc(hidden)]
pub fn capture_static_slot_outlet(
    el: &Element,
    entry: &'static StaticSlotOutlet,
    template_name: &str,
) -> Option<Element> {
    if el.local_name() != "slot" {
        fail(
            "slot-outlet-tag",
            template_name,
            entry.node_path,
            Some(entry.name),
        );
        return None;
    }
    Some(el.clone())
}

/// RFC 062 Phase 3 helper used by macro-specialized mount
/// bodies after every path-based entry has resolved.
#[doc(hidden)]
pub fn materialize_static_slot_outlet(el: &Element) {
    crate::mount::materialize_compiled_slot_outlet(el);
}

/// RFC 062 Phase 3 helper used by macro-specialized mount
/// bodies for allowlisted runtime-only DOM directives.
#[doc(hidden)]
pub fn install_static_opaque_directive(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticOpaqueDirective,
    template_name: &str,
) {
    if !dispatch_opaque(
        entry.name,
        el,
        entry.arg,
        entry.modifiers,
        entry.value,
        scope_id,
        proxy,
    ) {
        fail(
            "opaque-directive-lookup",
            template_name,
            entry.node_path,
            Some(entry.name),
        );
    }
}

/// RFC 062 Phase 3 helper used by macro-specialized mount
/// bodies to snapshot interpolation text nodes before any
/// planned target mutates sibling text-node indexes.
#[doc(hidden)]
pub fn capture_static_interp_target(
    el: &Element,
    entry: &'static StaticInterp,
    _template_name: &str,
) -> Option<StaticInterpTarget> {
    let target = directives::interp::resolve_text_target(el, entry.text_index as usize)?;
    Some(StaticInterpTarget {
        parent: el.clone(),
        text: target,
        segments: entry.segments,
    })
}

/// RFC 062 Phase 3 helper used by macro-specialized mount
/// bodies after every interpolation target has been captured.
#[doc(hidden)]
pub fn install_static_interp_target(
    target: &StaticInterpTarget,
    scope_id: ScopeId,
    proxy: &JsValue,
) {
    directives::interp::install_planned_target(
        &target.parent,
        proxy,
        crate::scope::scoped_root_reader(scope_id),
        &target.text,
        target.segments,
    );
}

/// RFC 062 Phase 3 helper used by macro-specialized mount
/// bodies for native `pp-model`.
#[doc(hidden)]
pub fn install_static_native_model(
    el: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    entry: &'static StaticNativeModel,
) {
    directives::model::install_native(
        el,
        scope_id,
        proxy,
        entry.expr_src.to_string(),
        entry.number,
        entry.lazy,
    );
}

/// Dispatch an opaque directive lift to its typed install entry.
/// Returns `false` when `name` doesn't match a known opaque
/// directive — caller treats that as a fail-fast event.
fn dispatch_opaque(
    name: &str,
    el: &Element,
    arg: Option<&str>,
    modifiers: &'static [&'static str],
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
        let Some(evaluator) = scoped_static_evaluator(scope_id, b.compiled, b.expr_src) else {
            fail(
                "pp-as-binding-parse",
                template_name,
                b.node_path,
                Some(b.expr_src),
            );
            continue;
        };
        match b.kind {
            BindingKind::Text => directives::text::install_eval(root, proxy, evaluator),
            BindingKind::Html => directives::html::install_eval(root, proxy, evaluator),
            BindingKind::Show => directives::show::install_eval(root, proxy, evaluator),
            BindingKind::Bind { arg } => {
                directives::bind::install_eval(root, proxy, arg, evaluator)
            }
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
        directives::on::install(
            root,
            scope_id,
            proxy,
            l.event,
            l.modifiers,
            Rc::new(directives::on::backfill_legacy_call(ast)),
        );
    }
    for d in plan
        .opaque_directives
        .iter()
        .filter(|d| d.node_path.is_empty())
    {
        if !dispatch_opaque(d.name, root, d.arg, d.modifiers, d.value, scope_id, proxy) {
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
        let Some(evaluator) = scoped_static_evaluator(scope_id, b.compiled, b.expr_src) else {
            fail(
                "child-host-binding-parse",
                template_name,
                child.node_path,
                Some(b.expr_src),
            );
            continue;
        };
        directives::bind::install_eval(el, proxy, b.arg, evaluator);
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
        directives::on::install(
            el,
            scope_id,
            proxy,
            l.event,
            l.modifiers,
            Rc::new(directives::on::backfill_legacy_call(ast)),
        );
    }
    for m in child.models {
        directives::model::install_compiled(el, scope_id, proxy, m.arg, m.modifiers, m.expr_src);
    }
}

/// RFC-058 Phase 4.1d / RFC 064 §5.1 — `pp-if` body fragment
/// runtime helper.
///
/// Parses `html` (the macro-cleaned body markup) into a fresh
/// namespace-aware fragment, extracts the single root element,
/// scope-binds it, stamps `CTX_PARENT_KEY`, then hands the
/// install pass off to `install_plan` — the macro-emitted
/// per-fragment closure that has the plan body unrolled inline.
/// Returns the root element ready for the `if_::install` caller
/// to pin its borrowed scope on + insert into the live DOM.
///
/// `None` return signals the body HTML didn't parse to a
/// single root element — same shape as the legacy
/// `clone_template_body` miss; the caller surfaces a console
/// error and bails the mount.
///
/// The closure boundary eliminates generic runtime plan
/// iteration for lifted body fragments (RFC 064 §5.1 Phase 1).
/// The closure receives the prepared body root with
/// `bind_borrowed_scope_to` + `CTX_PARENT_KEY` already stamped.
pub fn stamp_if_body_with(
    html: &str,
    scope_id: ScopeId,
    proxy: &JsValue,
    ctx_parent_id: ScopeId,
    install_plan: impl FnOnce(&Element, ScopeId, &JsValue),
) -> Option<Element> {
    let doc = web_sys::window().and_then(|w| w.document())?;
    let (root, content_parent) = parse_body_fragment_root(&doc, html)?;
    crate::mount::bind_borrowed_scope_to(&root, scope_id, proxy);
    let ctx_key = JsValue::from_str(crate::mount::CTX_PARENT_KEY);
    let ctx_val = JsValue::from_f64(ctx_parent_id.0 as f64);
    let _ = js_sys::Reflect::set(root.as_ref(), &ctx_key, &ctx_val);
    install_plan(&root, scope_id, proxy);
    if root.parent_node().is_some() {
        return Some(root);
    }
    // Slot-transparent recovery (see `stamp_if_body` doc).
    let kids = content_parent.child_nodes();
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i) {
            if let Ok(el) = n.dyn_into::<Element>() {
                return Some(el);
            }
        }
    }
    None
}

const SVG_NS: &str = "http://www.w3.org/2000/svg";

fn parse_body_fragment_root(doc: &web_sys::Document, html: &str) -> Option<(Element, Node)> {
    if first_fragment_tag(html).is_some_and(is_svg_fragment_root_tag) {
        let wrapper = doc.create_element_ns(Some(SVG_NS), "svg").ok()?;
        wrapper.set_inner_html(html);
        let root = first_element_child(wrapper.as_ref())?;
        return Some((root, wrapper.into()));
    }

    let template_el = doc.create_element("template").ok()?;
    template_el.set_inner_html(html);
    let template_el = template_el
        .dyn_into::<web_sys::HtmlTemplateElement>()
        .ok()?;
    let content = template_el.content();
    let root = first_element_child(content.as_ref())?;
    Some((root, content.into()))
}

fn first_element_child(parent: &Node) -> Option<Element> {
    let kids = parent.child_nodes();
    for i in 0..kids.length() {
        if let Some(n) = kids.item(i) {
            if let Ok(el) = n.dyn_into::<Element>() {
                return Some(el);
            }
        }
    }
    None
}

fn first_fragment_tag(html: &str) -> Option<&str> {
    let rest = html.trim_start().strip_prefix('<')?;
    if rest.starts_with('!') || rest.starts_with('?') || rest.starts_with('/') {
        return None;
    }
    let end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    let tag = &rest[..end];
    (!tag.is_empty()).then_some(tag)
}

fn is_svg_fragment_root_tag(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "animate"
            | "animatemotion"
            | "animatetransform"
            | "circle"
            | "clippath"
            | "defs"
            | "desc"
            | "ellipse"
            | "feblend"
            | "fecolormatrix"
            | "fecomponenttransfer"
            | "fecomposite"
            | "feconvolvematrix"
            | "fediffuselighting"
            | "fedisplacementmap"
            | "fedistantlight"
            | "fedropshadow"
            | "feflood"
            | "fefunca"
            | "fefuncb"
            | "fefuncg"
            | "fefuncr"
            | "fegaussianblur"
            | "feimage"
            | "femerge"
            | "femergenode"
            | "femorphology"
            | "feoffset"
            | "fepointlight"
            | "fespecularlighting"
            | "fespotlight"
            | "fetile"
            | "feturbulence"
            | "filter"
            | "foreignobject"
            | "g"
            | "image"
            | "line"
            | "lineargradient"
            | "marker"
            | "mask"
            | "metadata"
            | "mpath"
            | "path"
            | "pattern"
            | "polygon"
            | "polyline"
            | "radialgradient"
            | "rect"
            | "script"
            | "set"
            | "stop"
            | "style"
            | "svg"
            | "switch"
            | "symbol"
            | "text"
            | "textpath"
            | "title"
            | "tspan"
            | "use"
            | "view"
    )
}

type StaticEvaluator = Rc<dyn Fn(&JsValue) -> JsValue>;

/// RFC-095 W1 — the evaluator constructor: closures carry
/// a [`crate::expr::RootAccess`] for `scope_id`, so every effect
/// re-run resolves root field reads Rust-side (track + field
/// cache + `ComponentState::get`) instead of bouncing through the
/// proxy's `get` trap. The proxy is still consulted at call time
/// for `$`-roots, magics, and nested-object walks. A dead scope
/// (or a `$`-root) degrades to exactly the proxy-only path.
fn scoped_static_evaluator(
    scope_id: ScopeId,
    compiled: Option<&'static StaticExpr>,
    expr_src: &'static str,
) -> Option<StaticEvaluator> {
    let reader = crate::scope::scoped_root_reader(scope_id);
    if let Some(compiled) = compiled {
        return Some(Rc::new(move |scope| {
            compiled.evaluate_with(scope, reader.as_ref())
        }));
    }
    scoped_runtime_evaluator(reader, expr_src)
}

#[cfg(feature = "runtime-expr-fallback")]
fn scoped_runtime_evaluator(
    reader: Option<crate::expr::RootAccess>,
    expr_src: &'static str,
) -> Option<StaticEvaluator> {
    let ast = expr::parse_cached(expr_src).ok()?;
    Some(Rc::new(move |scope| {
        expr::evaluate_with(&ast, scope, reader.as_ref())
    }))
}

#[cfg(not(feature = "runtime-expr-fallback"))]
fn scoped_runtime_evaluator(
    _reader: Option<crate::expr::RootAccess>,
    _expr_src: &'static str,
) -> Option<StaticEvaluator> {
    None
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
