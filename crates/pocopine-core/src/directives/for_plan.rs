//! RFC 054 — Compiled `pp-for` row plans.
//!
//! Runtime-side data types and registry for pre-compiled keyed-list
//! row plans. The `#[component]` proc-macro walks each `.poco`
//! template at expansion time, identifies eligible `<template
//! pp-for>` rows (RFC 054 §5.2 envelope), and emits a `&'static
//! [StaticRowPlan]` constant alongside the template registration.
//! Each emitted plan stamps the source `<template>` opening tag
//! with `data-pp-row-plan="<id>"` so this module can match a
//! runtime `pp-for` directive call back to its compiled plan.
//!
//! At first use, each `expr_src` is parsed once via
//! [`crate::expr::parse_cached`] and the resulting AST is shared
//! across every row instance. Per-row mount then walks the
//! pre-recorded `node_path`s, evaluates ASTs, and patches DOM —
//! bypassing the generic walker's per-row attribute scan and
//! directive dispatch.
//!
//! v1 fast path covers the RFC §9 example shape: flat keyed table
//! rows with `pp-text`, `:class` / `pp-bind:class`, and bare `@<event>`
//! listeners. Anything else falls back to the generic walker
//! silently — `lookup_for_template` returns `None` when the
//! template isn't stamped, and `run_keyed` keeps its existing
//! body.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::{console, Element, Event, EventTarget};

use crate::expr::{self, Expr, Spanned};
use crate::loop_scope::LoopScope;
use crate::reactive::ScopeId;
use crate::scope::{with_current_el, Scope};

// ─── macro-emitted static shape ─────────────────────────────────

/// Kind of binding the macro recognised. Drives the patch
/// strategy at mount/reuse time.
///
/// `Text` and `Class` are the RFC-054 row-plan-only variants
/// emitted by the keyed-list compiler. The other variants are
/// RFC-058 Phase 2 template-plan additions; they're carried
/// in this same enum so both compilers share the runtime
/// hydration path even though they target different scopes
/// (per-row instance vs whole-template).
#[derive(Clone, Copy, Debug)]
pub enum BindingKind {
    /// `pp-text="<expr>"` — stringify the value, write
    /// `textContent`. Memoised against the last value.
    Text,
    /// `:class="<expr>"` / `pp-bind:class="<expr>"` —
    /// serialise via [`crate::directives::bind`]'s class
    /// shaping (string OR object form), write the result
    /// to `class`. Memoised against the serialised form.
    /// **RFC-054 row-plan only** — RFC-058 template plans
    /// emit `Bind { arg: "class" }` instead so a single grep
    /// shows which compiler emitted the entry.
    Class,
    /// `pp-html="<expr>"` — write the stringified value to
    /// `innerHTML`. RFC-058 Phase 2 template-plan addition.
    Html,
    /// `pp-bind:<arg>="<expr>"` / `:<arg>="<expr>"` — bind a
    /// native HTML attribute (data-/aria-/class/style/plain
    /// attr) to the expression, with the
    /// [`crate::directives::bind`] memoisation path.
    /// RFC-058 Phase 2 — child-component prop targets are
    /// **not** eligible (whole element falls back to walker).
    Bind { arg: &'static str },
    /// `pp-show="<expr>"` — toggle `display` style by
    /// truthiness; routes through transition presets when
    /// present. RFC-058 Phase 2 template-plan addition.
    Show,
}

/// Static-lifetime binding descriptor emitted by the macro. The
/// `expr_src` field is parsed once at registration into a
/// [`Spanned<Expr>`] cached in [`CompiledBinding`].
#[doc(hidden)]
pub struct StaticBinding {
    pub node_path: &'static [u16],
    pub kind: BindingKind,
    pub expr_src: &'static str,
}

/// Static-lifetime listener descriptor.
///
/// `modifiers` is the post-`.` token list — `prevent`, `stop`,
/// `self`, `once`, `window`, `document`, `outside`, `capture`,
/// key modifiers, and the `debounce` + numeric-ms pair (two
/// adjacent slots). The set must match what the runtime
/// [`crate::directives::on::install`] helper expects — the
/// macro-side classifier validates ahead of time, so reaching
/// the install with an unknown token is a framework bug.
///
/// RFC-054 row plans emit an empty `modifiers: &[]`; RFC-058
/// template plans populate it from the parsed `@event.<mod>`
/// chain.
#[doc(hidden)]
pub struct StaticListener {
    pub node_path: &'static [u16],
    pub event: &'static str,
    pub modifiers: &'static [&'static str],
    pub expr_src: &'static str,
}

/// Static-lifetime descriptor for a `pp-init="<expr>"` entry.
/// The expression is enqueued onto the walker's deferred-init
/// pending list ([`crate::walker::defer_init_on`]) so the
/// handler fires post-order after descendants are bound and
/// refs are registered. RFC-058 Phase 2 template-plan addition.
#[doc(hidden)]
pub struct StaticInit {
    pub node_path: &'static [u16],
    pub expr_src: &'static str,
}

/// Static-lifetime descriptor for a `pp-ref="<name>"` entry.
/// Resolves the named ref against the current scope at mount
/// time via [`crate::refs::register`]. RFC-058 Phase 2 template-
/// plan addition.
#[doc(hidden)]
pub struct StaticRef {
    pub node_path: &'static [u16],
    pub name: &'static str,
}

/// Static-lifetime descriptor for a child-component mount site.
///
/// RFC-058 Phase 3 — emitted by the macro for every non-HTML5
/// tag inside a plan-eligible template, so the runtime applier
/// can call [`crate::walker::mount_child_component`] explicitly
/// instead of leaving the tag for the walker's auto-discovery
/// pass to find. Today the walker still walks the subtree; its
/// `__pp_mounted` guard makes the discovery a no-op for any tag
/// the plan already mounted. Phase 6 (`legacy-dom` quarantine)
/// drops the walker discovery and the plan-driven path becomes
/// the sole entry.
///
/// `slots` carries the parent-authored slot fragments from
/// RFC-058 Phase 3.5b. Empty slice means the parent didn't
/// emit any fragments (either the slot content was dynamic and
/// the macro left it on the walker's capture path, or the
/// custom tag had no children). Non-empty slice flips the
/// applier onto [`crate::walker::mount_child_component_with_slots`]
/// so the walker's `materialize_slot` invokes the parent's
/// fragment instead of replaying captured DOM.
#[doc(hidden)]
pub struct StaticChildMount {
    pub node_path: &'static [u16],
    pub tag: &'static str,
    pub slots: &'static [StaticSlotFragment],
}

/// One macro-emitted slot fragment binding within a
/// [`StaticChildMount`]. RFC-058 Phase 3.5b.
///
/// `name` matches the wire-name the runtime walker uses to
/// look up slot content (`"default"` for the unnamed slot,
/// otherwise the value of `pp-slot="<name>"` on the parent's
/// `<template>` wrapper).
///
/// `fragment` is a stateless `fn` pointer the macro wired into
/// the parent's generated `register()` body — invoking it
/// stamps the parent-authored slot DOM into the
/// [`crate::slot_fragment::SlotMountCtx::host`] buffer.
#[doc(hidden)]
pub struct StaticSlotFragment {
    pub name: &'static str,
    pub fragment: crate::slot_fragment::SlotFragment,
}

/// Static-lifetime descriptor for a `<template pp-if="<expr>">`
/// site. RFC-058 Phase 4.1b.
///
/// `template_node_path` resolves the `<template>` element
/// inside the cleaned plan root — the macro keeps the element
/// in the rewritten HTML so `clone_template_body` still has
/// something to clone, but strips the `pp-if` attribute so the
/// runtime walker's directive-dispatch path doesn't double-
/// install the effect.
///
/// `expr_src` is the original truthy expression. The applier
/// parses it via `expr::parse_cached` and hands the AST to
/// [`crate::directives::if_::install`].
#[doc(hidden)]
pub struct StaticIfPlan {
    pub template_node_path: &'static [u16],
    pub expr_src: &'static str,
}

/// One macro-emitted row plan. The macro emits
/// `pub static __POC_ROW_PLANS_<COMPONENT>: &[StaticRowPlan] =
/// &[ … ];` and a registration call alongside
/// `register_template`.
#[doc(hidden)]
pub struct StaticRowPlan {
    /// Sequential `pp-for` index within the template, starting
    /// at 0. Matches the `data-pp-row-plan="<id>"` attribute the
    /// macro stamps on the source `<template>` tag.
    pub plan_id: u32,
    /// `pp-for="<v> in <items>"` — preserved so the runtime can
    /// publish it on the row scope without re-parsing the
    /// directive value.
    pub item_name: &'static str,
    pub bindings: &'static [StaticBinding],
    pub listeners: &'static [StaticListener],
}

// ─── runtime hydrated form ──────────────────────────────────────

pub(crate) struct CompiledBinding {
    pub node_path: &'static [u16],
    pub kind: BindingKind,
    pub ast: Spanned<Expr>,
    fast: Option<FastExpr>,
    /// Top-level parent-scope field names referenced anywhere in
    /// this binding's expression. Empty when the binding only
    /// reads loop-local data (`row.*`, `$index`, `$first`,
    /// `$last`).
    ///
    /// Non-empty entries make `mount_row_compiled` install one
    /// `effect()` per row for this binding — the effect reads
    /// each named field through `parent_proxy` to subscribe via
    /// the parent proxy's `get` trap, then evaluates the binding
    /// and patches DOM. Without this, RFC 054 M2's effect-less
    /// per-row eval path silently drops parent-state reactivity:
    /// changes to `selected_id` (e.g. selection styling on
    /// `:class="selected_id == row.id ? 'danger' : ''"`) wouldn't
    /// re-fire the row binding because the row's `reuse` only
    /// runs when the loop item itself changes.
    parent_field_paths: Box<[String]>,
}

pub(crate) struct CompiledListener {
    pub node_path: &'static [u16],
    pub event: &'static str,
    pub ast: Spanned<Expr>,
}

pub struct CompiledRowPlan {
    pub plan_id: u32,
    pub item_name: &'static str,
    pub(crate) bindings: Vec<CompiledBinding>,
    pub(crate) listeners: Vec<CompiledListener>,
}

impl CompiledRowPlan {
    /// True when every binding in this plan resolves through the
    /// typed `FastExpr` evaluator — meaning none of the per-row
    /// hot path needs the `js_sys::Proxy` for slow-path
    /// `expr::evaluate`. The runtime can skip `Scope::into_proxy`
    /// entirely at mount and lazy-mint only if a delegated
    /// listener actually fires for this row.
    pub(crate) fn is_proxy_elision_eligible(&self) -> bool {
        self.bindings.iter().all(|b| b.fast.is_some())
    }
}

// ─── registry ────────────────────────────────────────────────────

thread_local! {
    static ROW_PLANS: RefCell<HashMap<(String, u32), Rc<CompiledRowPlan>>> =
        RefCell::new(HashMap::new());
}

/// Hydrate every `StaticRowPlan` for a component into a
/// [`CompiledRowPlan`] (parsed AST, owned listener data) and
/// store it in the per-component registry. Called by macro-emitted
/// code immediately after `register_template`.
///
/// Idempotent — repeat calls overwrite the prior entry. Parse
/// failures here would indicate a macro bug; we log and skip the
/// plan so the runtime falls back to the generic walker.
pub fn register_row_plans(component_name: &str, plans: &'static [StaticRowPlan]) {
    if plans.is_empty() {
        return;
    }
    ROW_PLANS.with(|registry| {
        let mut r = registry.borrow_mut();
        for sp in plans {
            let mut bindings: Vec<CompiledBinding> = Vec::with_capacity(sp.bindings.len());
            let mut ok = true;
            for b in sp.bindings {
                match expr::parse_cached(b.expr_src) {
                    Ok(ast) => {
                        let fast = compile_fast_expr(sp.item_name, &ast);
                        let parent_field_paths =
                            collect_parent_field_paths(sp.item_name, &ast);
                        bindings.push(CompiledBinding {
                            node_path: b.node_path,
                            kind: b.kind,
                            ast,
                            fast,
                            parent_field_paths,
                        });
                    }
                    Err(e) => {
                        console::error_1(&JsValue::from_str(&format!(
                            "rfc-054: row plan {component_name}#{} binding parse failed (macro bug): {} at {}..{}",
                            sp.plan_id, e.message, e.span.start, e.span.end,
                        )));
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let mut listeners: Vec<CompiledListener> = Vec::with_capacity(sp.listeners.len());
            for l in sp.listeners {
                match expr::parse_cached(l.expr_src) {
                    Ok(ast) => listeners.push(CompiledListener {
                        node_path: l.node_path,
                        event: l.event,
                        ast,
                    }),
                    Err(e) => {
                        console::error_1(&JsValue::from_str(&format!(
                            "rfc-054: row plan {component_name}#{} listener parse failed (macro bug): {} at {}..{}",
                            sp.plan_id, e.message, e.span.start, e.span.end,
                        )));
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            r.insert(
                (component_name.to_string(), sp.plan_id),
                Rc::new(CompiledRowPlan {
                    plan_id: sp.plan_id,
                    item_name: sp.item_name,
                    bindings,
                    listeners,
                }),
            );
        }
    });
}

#[derive(Clone)]
enum FastExpr {
    Path(FastPath),
    TernaryEq {
        lhs: FastPath,
        rhs: FastPath,
        then_value: JsValue,
        else_value: JsValue,
        invert: bool,
    },
}

#[derive(Clone)]
struct FastPath {
    root: FastPathRoot,
    keys: Box<[JsValue]>,
}

#[derive(Clone, Copy)]
enum FastPathRoot {
    LoopItem,
    LoopIndex,
    LoopFirst,
    LoopLast,
    Parent,
}

/// Walk a parsed binding expression and collect every distinct
/// top-level field name that resolves against the **parent**
/// scope (i.e. anything other than the loop variable, `$index`,
/// `$first`, `$last`). The runtime uses this list to subscribe
/// each row's parent-touching binding to changes on those
/// fields — without it, RFC 054 M2's effect-less binding eval
/// path drops parent-state reactivity.
///
/// Returns `Box<[]>` for purely-loop-local expressions; those
/// bindings stay on the direct mount/reuse path with no
/// effect overhead.
fn collect_parent_field_paths(item_name: &str, expr: &Spanned<Expr>) -> Box<[String]> {
    let mut out: Vec<String> = Vec::new();
    walk_collect(item_name, expr, &mut out);
    out.into_boxed_slice()
}

fn walk_collect(item_name: &str, expr: &Spanned<Expr>, out: &mut Vec<String>) {
    match &expr.value {
        Expr::Path(segments) => {
            let Some(first) = segments.first() else {
                return;
            };
            if first == item_name || first == "$index" || first == "$first" || first == "$last" {
                return;
            }
            if !out.iter().any(|existing| existing == first) {
                out.push(first.clone());
            }
        }
        Expr::Not(inner) => walk_collect(item_name, inner, out),
        Expr::BinOp(_, lhs, rhs) => {
            walk_collect(item_name, lhs, out);
            walk_collect(item_name, rhs, out);
        }
        Expr::Ternary(c, t, e) => {
            walk_collect(item_name, c, out);
            walk_collect(item_name, t, out);
            walk_collect(item_name, e, out);
        }
        Expr::Call(_, args) => {
            // The call name itself is a handler, not a parent
            // field — handlers route through `LoopScope::invoke`
            // and don't need reactive subscriptions. Only the
            // args' field reads matter.
            for arg in args {
                walk_collect(item_name, arg, out);
            }
        }
        Expr::Assign(_, rhs) => {
            // Assignment target is a write, not a tracked read.
            walk_collect(item_name, rhs, out);
        }
        Expr::Seq(stmts) => {
            for s in stmts {
                walk_collect(item_name, s, out);
            }
        }
        Expr::Literal(_) => {}
    }
}

fn compile_fast_expr(item_name: &str, expr: &Spanned<Expr>) -> Option<FastExpr> {
    match &expr.value {
        Expr::Path(segments) => Some(FastExpr::Path(compile_fast_path(item_name, segments)?)),
        Expr::Ternary(cond, then_e, else_e) => {
            let Expr::BinOp(op, lhs, rhs) = &cond.value else {
                return None;
            };
            let invert = match op {
                crate::expr::BinOp::Eq => false,
                crate::expr::BinOp::Ne => true,
                _ => return None,
            };
            let lhs = match &lhs.value {
                Expr::Path(segments) => compile_fast_path(item_name, segments)?,
                _ => return None,
            };
            let rhs = match &rhs.value {
                Expr::Path(segments) => compile_fast_path(item_name, segments)?,
                _ => return None,
            };
            Some(FastExpr::TernaryEq {
                lhs,
                rhs,
                then_value: literal_fast_value(then_e)?,
                else_value: literal_fast_value(else_e)?,
                invert,
            })
        }
        _ => None,
    }
}

fn compile_fast_path(item_name: &str, segments: &[String]) -> Option<FastPath> {
    let (root, rest) = match segments.first().map(String::as_str)? {
        first if first == item_name => (FastPathRoot::LoopItem, &segments[1..]),
        "$index" => (FastPathRoot::LoopIndex, &segments[1..]),
        "$first" => (FastPathRoot::LoopFirst, &segments[1..]),
        "$last" => (FastPathRoot::LoopLast, &segments[1..]),
        _ => (FastPathRoot::Parent, segments),
    };
    Some(FastPath {
        root,
        keys: rest
            .iter()
            .map(|segment| JsValue::from_str(segment))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    })
}

fn literal_fast_value(expr: &Spanned<Expr>) -> Option<JsValue> {
    match &expr.value {
        Expr::Literal(crate::expr::Literal::Null) => Some(JsValue::NULL),
        Expr::Literal(crate::expr::Literal::Bool(v)) => Some(JsValue::from_bool(*v)),
        Expr::Literal(crate::expr::Literal::Number(v)) => Some(JsValue::from_f64(*v)),
        Expr::Literal(crate::expr::Literal::String(v)) => Some(JsValue::from_str(v)),
        _ => None,
    }
}

fn evaluate_binding(
    binding: &CompiledBinding,
    proxy: &JsValue,
    loop_state: Option<&LoopScope>,
) -> JsValue {
    if let (Some(fast), Some(loop_state)) = (&binding.fast, loop_state) {
        return evaluate_fast_expr(fast, loop_state);
    }
    expr::evaluate(&binding.ast, proxy)
}

fn evaluate_fast_expr(expr: &FastExpr, loop_state: &LoopScope) -> JsValue {
    match expr {
        FastExpr::Path(path) => evaluate_fast_path(path, loop_state),
        FastExpr::TernaryEq {
            lhs,
            rhs,
            then_value,
            else_value,
            invert,
        } => {
            let eq = Object::is(
                &evaluate_fast_path(lhs, loop_state),
                &evaluate_fast_path(rhs, loop_state),
            );
            if eq ^ *invert {
                then_value.clone()
            } else {
                else_value.clone()
            }
        }
    }
}

fn fast_expr_depends_on_loop_position(expr: &FastExpr) -> bool {
    match expr {
        FastExpr::Path(path) => fast_path_depends_on_loop_position(path),
        FastExpr::TernaryEq { lhs, rhs, .. } => {
            fast_path_depends_on_loop_position(lhs) || fast_path_depends_on_loop_position(rhs)
        }
    }
}

fn fast_path_depends_on_loop_position(path: &FastPath) -> bool {
    matches!(
        path.root,
        FastPathRoot::LoopIndex | FastPathRoot::LoopFirst | FastPathRoot::LoopLast
    )
}

impl CompiledRowPlan {
    /// True when any compiled binding reads `$index`, `$first`, or
    /// `$last`. Reconcile can still update the row's LoopScope for
    /// listener correctness, but when this is false an index/total-only
    /// change does not require reapplying DOM bindings.
    pub(crate) fn depends_on_loop_position(&self) -> bool {
        self.bindings.iter().any(|binding| {
            binding
                .fast
                .as_ref()
                .map(fast_expr_depends_on_loop_position)
                // Non-fast bindings may read positional fields through
                // the generic evaluator, so stay conservative.
                .unwrap_or(true)
        })
    }
}

fn evaluate_fast_path(path: &FastPath, loop_state: &LoopScope) -> JsValue {
    let mut cur = match path.root {
        FastPathRoot::LoopItem => loop_state.item.clone(),
        FastPathRoot::LoopIndex => JsValue::from_f64(loop_state.index as f64),
        FastPathRoot::LoopFirst => JsValue::from_bool(loop_state.index == 0),
        FastPathRoot::LoopLast => JsValue::from_bool(loop_state.index + 1 == loop_state.total),
        FastPathRoot::Parent => loop_state.parent.clone(),
    };
    for key in path.keys.iter() {
        cur = Reflect::get(&cur, key).unwrap_or(JsValue::UNDEFINED);
    }
    cur
}

pub(crate) fn ensure_delegated_listeners(
    plan: &Rc<CompiledRowPlan>,
    template_el: &Element,
    parent_node: &web_sys::Node,
) {
    let Some(parent_el) = parent_node.dyn_ref::<Element>() else {
        return;
    };
    for event in unique_listener_events(plan) {
        let marker = format!("data-pp-delegated-{event}");
        if template_el.has_attribute(&marker) {
            continue;
        }
        let _ = template_el.set_attribute(&marker, "");
        install_list_delegated_listener(parent_el.clone(), event);
    }
}

fn unique_listener_events(plan: &CompiledRowPlan) -> Vec<&'static str> {
    let mut events: Vec<&'static str> = Vec::new();
    for listener in &plan.listeners {
        if !events.contains(&listener.event) {
            events.push(listener.event);
        }
    }
    events
}

fn install_list_delegated_listener(parent_el: Element, event: &'static str) {
    let parent_for_track = parent_el.clone();
    let closure = Closure::wrap(Box::new(move |ev: Event| {
        let Some(target) = ev
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
        else {
            return;
        };
        let mut cursor = target.dyn_ref::<Element>().cloned();
        while let Some(el) = cursor {
            // Use the id-only variant so a click on a proxy-elided
            // row doesn't force a `Scope::into_proxy` mint just to
            // discard the proxy here. The handler dispatch path
            // (`dispatch_delegated_event` →`instance_proxy`)
            // lazy-mints only if the listener AST actually needs it.
            if let Some(scope_id) = crate::walker::scope_id_of_element(&el) {
                dispatch_delegated_event(scope_id, event, &target, &ev);
                return;
            }
            cursor = el.parent_element();
        }
    }) as Box<dyn FnMut(Event)>);

    let target: EventTarget = parent_el.into();
    crate::walker::track_listener_on(&parent_for_track, target, event, false, closure);
}

fn dispatch_delegated_event(
    scope_id: ScopeId,
    event: &'static str,
    target: &web_sys::Node,
    ev: &Event,
) {
    ROW_INSTANCES.with(|m| {
        let map = m.borrow();
        let Some(instance) = map.get(&scope_id) else {
            return;
        };
        let Some(proxy) = instance_proxy(instance, scope_id) else {
            return;
        };
        let ev_js: JsValue = {
            let r: &JsValue = ev.as_ref();
            r.clone()
        };
        for route in instance.listener_routes.iter().rev() {
            if route.event != event {
                continue;
            }
            let route_node: &web_sys::Node = route.node.as_ref();
            if !route_node.contains(Some(target)) {
                continue;
            }
            with_current_el(&route.node, || {
                crate::scope::with_current_scope_id(scope_id, || {
                    crate::magics::with_current_event(&ev_js, || {
                        expr::evaluate(&route.ast, &proxy);
                    });
                });
            });
        }
    });
}

/// Resolve the compiled plan for a `<template pp-for>` element.
/// Returns `None` when the template isn't stamped (eligibility
/// rejection at macro time, or the consumer crate's macro version
/// predates RFC 054) — the caller falls back to the generic walker.
pub fn lookup_for_template(template_el: &Element) -> Option<Rc<CompiledRowPlan>> {
    let plan_id = template_el
        .get_attribute("data-pp-row-plan")
        .and_then(|s| s.parse::<u32>().ok())?;
    let component_name = nearest_component_name(template_el)?;
    ROW_PLANS.with(|r| r.borrow().get(&(component_name, plan_id)).cloned())
}

/// Fetch (or lazy-mint) the row's `js_sys::Proxy`. Compiled-row
/// instances may have been mounted with `proxy = None` when the
/// plan is FastExpr-only; the first listener fire / slow-path
/// binding eval routes through here, mints the proxy on demand
/// from the live `Scope`, and caches it on the `RowInstance`.
fn instance_proxy(instance: &RowInstance, scope_id: ScopeId) -> Option<JsValue> {
    if let Some(p) = instance.proxy.borrow().as_ref() {
        return Some(p.clone());
    }
    let scope = Scope::find(scope_id)?;
    let proxy = scope.into_proxy();
    *instance.proxy.borrow_mut() = Some(proxy.clone());
    Some(proxy)
}

/// Walk up from `el` looking for the nearest ancestor (inclusive)
/// stamped with `pp-data="<name>"` — that's the host component.
/// Used at directive-bind time to find which component's plan
/// table to consult; runs once per `<template pp-for>`, not per
/// row.
fn nearest_component_name(el: &Element) -> Option<String> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(e) = cur {
        if let Some(name) = e.get_attribute("pp-data") {
            return Some(name);
        }
        let tag = e.tag_name().to_ascii_lowercase();
        if tag.contains('-') {
            return Some(tag);
        }
        cur = e.parent_element();
    }
    None
}

// ─── per-row instance state ─────────────────────────────────────

/// Per-row state owned by the compiled fast path. Replaces the
/// per-binding `effect()` closures the M1 path used: bindings
/// evaluate explicitly via `reuse_row_compiled` instead of via
/// `trigger_scope`, so update cycles skip the entire reactive
/// dispatch + Reflect::get tracking machinery and just walk a
/// flat array of cached values.
struct RowInstance {
    plan: Rc<CompiledRowPlan>,
    /// Element handles resolved from `plan.bindings[i].node_path`
    /// once at mount.
    binding_nodes: Box<[Element]>,
    /// Last serialised value written per binding. `None` means
    /// the slot is currently absent / not-yet-written. Compared
    /// against the new evaluation each tick to skip DOM writes
    /// when nothing changed.
    binding_cache: Vec<Option<Rc<str>>>,
    /// `js_sys::Proxy` for the row scope. Lazy-minted: when the
    /// plan is FastExpr-only (see [`is_proxy_elision_eligible`]),
    /// this stays `None` through mount and is built on first
    /// listener fire / slow-path binding eval. Mounting 10K rows
    /// without minting saves the per-row `Object::new ×2 + 2 trap
    /// closures + Proxy::new` (~24K wasm-js bridge ops).
    proxy: RefCell<Option<JsValue>>,
    /// Typed loop scope for compiled binding fast paths. Generic
    /// expression evaluation still uses `proxy`; fast paths read the
    /// loop item and parent proxy directly to avoid row-proxy traps.
    loop_state: Option<Rc<RefCell<LoopScope>>>,
    /// Per-row listener routes used by the list-level delegated
    /// listener installed for compiled row plans.
    listener_routes: Box<[RowListenerRoute]>,
    /// `(parent_scope_id, plan_ptr)` membership key for the
    /// list-level parent-state watcher (see [`ListWatcherKey`]).
    /// Lets the watcher iterate exactly its rows on re-fire and
    /// lets unmount drop the row from the per-list index without
    /// scanning the whole `ROW_INSTANCES` map.
    list_key: ListWatcherKey,
}

struct RowListenerRoute {
    event: &'static str,
    node: Element,
    ast: Spanned<Expr>,
}

/// Key for the per-(parent_scope, plan) list-level watcher
/// registry. The plan pointer disambiguates multiple `pp-for`s
/// in the same parent component; the parent scope id covers the
/// case of two different component instances each rendering
/// their own list.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
struct ListWatcherKey {
    parent_scope_id: ScopeId,
    plan_ptr: usize,
}

impl ListWatcherKey {
    fn new(parent_scope_id: ScopeId, plan: &Rc<CompiledRowPlan>) -> Self {
        Self {
            parent_scope_id,
            plan_ptr: Rc::as_ptr(plan) as usize,
        }
    }
}

/// One entry per `(parent_scope_id, plan)` pair. Holds the live
/// effect id (so unmount can release it once the list is
/// fully empty) and the rows currently belonging to this list
/// (so the effect's body iterates only its own rows on
/// re-fire instead of scanning the whole `ROW_INSTANCES` map).
struct ListWatcher {
    /// `effect()` id that subscribes to every parent-state
    /// field referenced by any binding in `plan`. Released via
    /// `crate::reactive::release` once the last row in the
    /// list unmounts.
    effect_id: crate::reactive::EffectId,
    /// Live row scope ids for this list. Mutated in lockstep
    /// with `ROW_INSTANCES`: mount adds, unmount removes.
    members: Vec<ScopeId>,
}

thread_local! {
    /// Per-row state, keyed by the row's loop scope id. Populated
    /// on `mount_row_compiled`, drained on `unmount_row_compiled`
    /// when the for_.rs reconcile loop drops a row.
    static ROW_INSTANCES: RefCell<HashMap<ScopeId, RowInstance>> =
        RefCell::new(HashMap::new());

    /// One watcher per (parent_scope_id, plan) pair. RFC 054
    /// patches parent-state reactivity at the list level rather
    /// than per row: a single effect subscribes to every
    /// distinct parent-field name any of the plan's bindings
    /// touch, then on fire iterates the list's `members` and
    /// re-evaluates only the parent-touching bindings on each.
    /// One closure allocation per list instead of one per row —
    /// crucial for `runLots(10000)` mount cost.
    static LIST_WATCHERS: RefCell<HashMap<ListWatcherKey, ListWatcher>> =
        RefCell::new(HashMap::new());
}

// ─── per-row mount ──────────────────────────────────────────────

/// Resolve a `node_path` against a freshly cloned row root.
/// Walks `Element::children().item(idx)` per hop. Returns
/// `None` if any hop falls off the end (defensive against macro
/// emission bugs).
fn resolve_node_path(root: &Element, path: &[u16]) -> Option<Element> {
    let mut cur: Element = root.clone();
    for &idx in path {
        let children = cur.children();
        let next = children.item(idx as u32)?;
        cur = next;
    }
    Some(cur)
}

/// Install the bindings + listeners for a freshly cloned row.
/// `proxy` is the row's loop scope proxy if the caller already
/// minted one (non-elision path); `None` means the caller skipped
/// `Scope::into_proxy` because the plan is FastExpr-only and the
/// per-row hot path doesn't need a proxy. `scope_id` is the row
/// scope's id, used to lazy-mint the proxy on demand.
///
/// RFC 054 §5.5 — bindings evaluate **synchronously** here with
/// no `effect()` wrapper. The plan + per-row [`RowInstance`]
/// cache replaces reactive dispatch on update; reuse goes
/// through [`reuse_row_compiled`] which walks the cache and
/// patches DOM only on real change. This skips the per-row
/// effect-machinery + Reflect::get tracking that dominates
/// `update every 10th` cost in the generic walker.
pub(crate) fn mount_row_compiled(
    plan: &Rc<CompiledRowPlan>,
    row_root: &Element,
    scope_id: ScopeId,
    proxy: Option<&JsValue>,
) {
    // Resolve every binding/listener node path up front. A miss
    // is a macro emission bug — log and bail; the row still has
    // its scope bound so the runtime won't double-bind on retry.
    let mut binding_nodes: Vec<Element> = Vec::with_capacity(plan.bindings.len());
    let node_path_start = crate::profiler::mount::start();
    for b in &plan.bindings {
        let Some(node) = resolve_node_path(row_root, b.node_path) else {
            console::warn_1(&JsValue::from_str(&format!(
                "rfc-054: row binding node_path {:?} did not resolve",
                b.node_path,
            )));
            return;
        };
        binding_nodes.push(node);
    }
    crate::profiler::mount::record_node_path_resolution(node_path_start);

    // Initial evaluate + DOM patch + prime cache. Purely loop-
    // local bindings get evaluated synchronously here; parent-
    // touching bindings are deferred to the per-binding effect
    // installed below so their first run subscribes via the
    // parent proxy's `get` trap. Slots for deferred bindings
    // start as `None` and the effect's first immediate run fills
    // them in.
    //
    // FastExpr-only plans take the `proxy.is_none()` path — none
    // of their bindings consult `proxy` (the slow-path
    // `expr::evaluate(&binding.ast, proxy)` branch is unreachable
    // when every `b.fast.is_some()`). For mixed plans, the caller
    // already minted a proxy and passed it in.
    let loop_state = Scope::find(scope_id).and_then(|scope| scope.typed::<LoopScope>());
    let binding_start = crate::profiler::mount::start();
    let binding_cache = {
        let mut binding_cache: Vec<Option<Rc<str>>> = vec![None; plan.bindings.len()];
        let loop_borrow = loop_state.as_ref().map(|state| state.borrow());
        let loop_ref = loop_borrow.as_deref();
        let placeholder_proxy = JsValue::UNDEFINED;
        let proxy_for_eval = proxy.unwrap_or(&placeholder_proxy);
        for (i, b) in plan.bindings.iter().enumerate() {
            if !b.parent_field_paths.is_empty() {
                continue;
            }
            let v = evaluate_binding(b, proxy_for_eval, loop_ref);
            binding_cache[i] = apply_binding(&binding_nodes[i], b.kind, &v, None);
        }
        binding_cache
    };
    crate::profiler::mount::record_initial_binding_apply(binding_start);

    // Listener nodes resolve once and route through one delegated
    // listener per event on the list container.
    let listener_start = crate::profiler::mount::start();
    let listener_routes = resolve_listener_routes(plan, row_root);
    crate::profiler::mount::record_listener_installation(listener_start);
    crate::profiler::mount::record_compiled_row_mounted();

    let initial_proxy: RefCell<Option<JsValue>> = RefCell::new(proxy.cloned());

    // Determine the row's parent scope id + proxy (needed for
    // both the list_key and the watcher install path).
    let (parent_scope_id, parent_proxy) = match loop_state_parent(scope_id) {
        Some(pair) => pair,
        None => {
            ROW_INSTANCES.with(|m| {
                m.borrow_mut().insert(
                    scope_id,
                    RowInstance {
                        plan: plan.clone(),
                        binding_nodes: binding_nodes.into_boxed_slice(),
                        binding_cache,
                        proxy: initial_proxy,
                        loop_state,
                        listener_routes,
                        list_key: ListWatcherKey::new(ScopeId(0), plan),
                    },
                );
            });
            return;
        }
    };
    let list_key = ListWatcherKey::new(parent_scope_id, plan);
    ROW_INSTANCES.with(|m| {
        m.borrow_mut().insert(
            scope_id,
            RowInstance {
                plan: plan.clone(),
                binding_nodes: binding_nodes.into_boxed_slice(),
                binding_cache,
                proxy: initial_proxy,
                loop_state,
                listener_routes,
                list_key,
            },
        );
    });

    // Hook up to the list-level parent-state watcher.
    // `ensure_list_watcher` installs one effect per
    // (parent_scope_id, plan) pair on first row mount; later
    // rows just append themselves to its `members` list. The
    // effect, when fired, iterates `members` and re-evaluates
    // every parent-touching binding on each row. One closure
    // allocation per list instead of per row.
    ensure_list_watcher(plan, list_key, parent_proxy);
    LIST_WATCHERS.with(|m| {
        if let Some(watcher) = m.borrow_mut().get_mut(&list_key) {
            watcher.members.push(scope_id);
        }
    });
    // The list watcher's first run (during install) iterated an
    // empty `members` list, so this row's parent-touching
    // bindings haven't been evaluated yet. Do it now —
    // synchronously, against the just-inserted instance — so
    // the row's first-mount DOM is correct without waiting
    // for a `Handle::update` to drive the watcher.
    refresh_parent_bindings(scope_id);
}

/// Recover `(parent_scope_id, parent_proxy)` for a row scope.
/// `LoopScope` carries both the parent proxy (for fall-through
/// reads via JS Reflect) and the parent scope id (so handler
/// invocations route correctly).
fn loop_state_parent(scope_id: ScopeId) -> Option<(ScopeId, JsValue)> {
    let scope = Scope::find(scope_id)?;
    let loop_state = scope.typed::<LoopScope>()?;
    let borrow = loop_state.borrow();
    Some((borrow.parent_scope_id, borrow.parent.clone()))
}

/// Install (idempotently) the list-level watcher for a
/// `(parent_scope_id, plan)` pair. The watcher subscribes to
/// every distinct top-level parent field name any of the
/// plan's bindings touch — fired exactly once per
/// `Handle::update` on the parent that touches one of those
/// keys.
///
/// The effect's body iterates the list's current `members`
/// (the row scope ids tracked alongside the watcher), looks up
/// each row's `RowInstance`, and re-evaluates only the
/// parent-touching bindings — the existing serialised-value
/// cache on `RowInstance.binding_cache` skips DOM writes when
/// nothing actually changed.
///
/// First-mount invariant: when the watcher is installed the
/// effect runs immediately, but `members` is still empty — the
/// row that triggered the install only appends itself *after*
/// `ensure_list_watcher` returns. So that first effect run
/// observes no rows and writes nothing.
///
/// Parent-touching bindings are skipped by the per-row initial
/// loop in `mount_row_compiled` (rows whose `parent_field_paths`
/// are non-empty defer to the watcher). To prevent the row's
/// first-mount DOM from being stale until the next parent
/// update, the caller invokes `refresh_parent_bindings` for
/// the just-mounted scope right before pushing it into
/// `members`.
///
/// From the second mount onward the watcher already exists,
/// no new effect is allocated, and the row simply appends to
/// `members` — subsequent parent updates re-fire the effect
/// against the full list.
fn ensure_list_watcher(
    plan: &Rc<CompiledRowPlan>,
    list_key: ListWatcherKey,
    parent_proxy: JsValue,
) {
    let already = LIST_WATCHERS.with(|m| m.borrow().contains_key(&list_key));
    if !already {
        // Distinct top-level parent fields across all bindings
        // in the plan. `&'static str`-style names live as
        // owned `String` on `CompiledBinding`; we re-key as
        // `JsValue` once at install (cheap; not in the hot
        // re-fire loop).
        let mut field_keys: Vec<JsValue> = Vec::new();
        for binding in &plan.bindings {
            for name in binding.parent_field_paths.iter() {
                let key_js = JsValue::from_str(name);
                let already_have = field_keys
                    .iter()
                    .any(|existing| existing.as_string().as_deref() == Some(name.as_str()));
                if !already_have {
                    field_keys.push(key_js);
                }
            }
        }
        if field_keys.is_empty() {
            return;
        }
        let parent_proxy_for_effect = parent_proxy.clone();
        let list_key_for_effect = list_key;
        let effect_id = crate::reactive::effect(move || {
            // Subscribe via the parent proxy's `get` trap.
            for k in &field_keys {
                let _ = Reflect::get(&parent_proxy_for_effect, k);
            }
            // Iterate this list's current rows and re-evaluate
            // their parent-touching bindings.
            let members: Vec<ScopeId> = LIST_WATCHERS.with(|m| {
                m.borrow()
                    .get(&list_key_for_effect)
                    .map(|w| w.members.clone())
                    .unwrap_or_default()
            });
            for row_scope in members {
                refresh_parent_bindings(row_scope);
            }
        });
        LIST_WATCHERS.with(|m| {
            m.borrow_mut().insert(
                list_key,
                ListWatcher {
                    effect_id,
                    members: Vec::new(),
                },
            );
        });
    }

    // First-mount eval for the row that triggered install
    // happens after the caller appends to `members`. See
    // `mount_row_compiled` for the explicit
    // `refresh_parent_bindings(scope_id)` call right after the
    // push.
}

/// Re-evaluate every parent-touching binding for the row in
/// `ROW_INSTANCES` keyed by `scope_id`. Used by the list
/// watcher's effect body and by `reuse_row_compiled`'s
/// fallback path. No-op if the row was already unmounted.
fn refresh_parent_bindings(scope_id: ScopeId) {
    ROW_INSTANCES.with(|m| {
        let mut map = m.borrow_mut();
        let Some(instance) = map.get_mut(&scope_id) else {
            return;
        };
        let plan = instance.plan.clone();
        // Slow-path eval (non-FastExpr) needs a real proxy. For
        // FastExpr-only plans the placeholder is never consulted —
        // every `evaluate_binding` short-circuits via `fast.is_some()`.
        let proxy_value = instance.proxy.borrow().clone();
        let placeholder = JsValue::UNDEFINED;
        let proxy_ref = proxy_value.as_ref().unwrap_or(&placeholder);
        let loop_borrow = instance.loop_state.as_ref().map(|state| state.borrow());
        let loop_ref = loop_borrow.as_deref();
        for (i, binding) in plan.bindings.iter().enumerate() {
            if binding.parent_field_paths.is_empty() {
                continue;
            }
            let v = evaluate_binding(binding, proxy_ref, loop_ref);
            let prev = instance.binding_cache[i].as_deref();
            instance.binding_cache[i] =
                apply_binding(&instance.binding_nodes[i], binding.kind, &v, prev);
        }
    });
}

/// Re-run the row plan's bindings against the (mutated) loop
/// scope. Evaluates each binding's AST, compares against the
/// cached serialisation, and patches DOM only on real change.
/// Replaces `trigger_scope(scope_id)` on the compiled path —
/// no effect dispatch, no Reflect::get dep tracking, just a
/// flat array walk over `plan.bindings`.
///
/// Returns `true` if the row was tracked (compiled-path row);
/// `false` lets the caller fall back to `trigger_scope` for any
/// row whose mount went through the generic walker.
pub(crate) fn reuse_row_compiled(scope_id: ScopeId) -> bool {
    ROW_INSTANCES.with(|m| {
        let mut map = m.borrow_mut();
        let Some(instance) = map.get_mut(&scope_id) else {
            return false;
        };
        let plan = instance.plan.clone();
        // Same proxy-or-placeholder dance as `refresh_parent_bindings`:
        // FastExpr-only plans never consult the proxy; mixed plans
        // mounted with a real proxy use it for slow-path evals.
        let proxy_value = instance.proxy.borrow().clone();
        let placeholder = JsValue::UNDEFINED;
        let proxy_ref = proxy_value.as_ref().unwrap_or(&placeholder);
        let loop_borrow = instance.loop_state.as_ref().map(|state| state.borrow());
        let loop_ref = loop_borrow.as_deref();
        for (i, b) in plan.bindings.iter().enumerate() {
            let v = evaluate_binding(b, proxy_ref, loop_ref);
            let prev = instance.binding_cache[i].as_deref();
            instance.binding_cache[i] = apply_binding(&instance.binding_nodes[i], b.kind, &v, prev);
        }
        true
    })
}

/// Drop a row's per-instance state. Called from `for_.rs` when
/// reconcile permanently retires a row. The walker's
/// `release_subtree` already tears down `addEventListener`
/// registrations via the element-scoped listener side-table; we
/// drop this row from its list watcher's `members` index here,
/// and when the last row in a list unmounts we release the
/// list-level effect too so its parent-state subscription
/// doesn't outlive the list.
pub fn unmount_row_compiled(scope_id: ScopeId) {
    let list_key = ROW_INSTANCES.with(|m| {
        m.borrow_mut()
            .remove(&scope_id)
            .map(|instance| instance.list_key)
    });
    let Some(list_key) = list_key else {
        return;
    };
    let dropped_effect = LIST_WATCHERS.with(|m| {
        let mut map = m.borrow_mut();
        let watcher = map.get_mut(&list_key)?;
        watcher.members.retain(|id| *id != scope_id);
        if watcher.members.is_empty() {
            // Last row in this list — tear the watcher down so
            // its parent-state subscription stops firing.
            map.remove(&list_key).map(|w| w.effect_id)
        } else {
            None
        }
    });
    if let Some(effect_id) = dropped_effect {
        crate::reactive::release(effect_id);
    }
}

/// Bulk teardown for the bulk-clear fast path in
/// `for_.rs::run_keyed`. Drops every `RowInstance` for the given
/// scope ids in a single `ROW_INSTANCES.with` borrow, then drops
/// the entire list-level watcher in a single `LIST_WATCHERS.with`
/// borrow.
///
/// Why this matters: per-row `unmount_row_compiled` calls
/// `watcher.members.retain(|id| *id != scope_id)` which is O(N)
/// over the list's members. Looping that N times is O(N²) — for a
/// 10K-row clear that's ~100M ops, and the slowest-run profile
/// pinned **358ms** of `clear`'s 401ms in this loop. Releasing
/// the watcher in one shot collapses it to O(N).
pub fn unmount_rows_bulk(scope_ids: &[ScopeId]) {
    if scope_ids.is_empty() {
        return;
    }
    // Pull `list_key` out of every removed instance. They should
    // all share the same key for the bulk-clear case (one
    // `pp-for` list going empty), but the API is correct even if
    // multiple lists are torn down together.
    let mut list_keys: Vec<ListWatcherKey> = ROW_INSTANCES.with(|m| {
        let mut map = m.borrow_mut();
        scope_ids
            .iter()
            .filter_map(|id| map.remove(id).map(|instance| instance.list_key))
            .collect()
    });
    if list_keys.is_empty() {
        return;
    }
    list_keys.sort_unstable_by_key(|k| (k.parent_scope_id.0, k.plan_ptr));
    list_keys.dedup();

    // Drop the list-level watcher(s) in one borrow. Capture the
    // effect ids to release outside the LIST_WATCHERS borrow
    // (release may indirectly read other thread_local state).
    let dropped_effects: Vec<crate::reactive::EffectId> = LIST_WATCHERS.with(|m| {
        let mut map = m.borrow_mut();
        list_keys
            .iter()
            .filter_map(|key| map.remove(key).map(|w| w.effect_id))
            .collect()
    });
    for effect_id in dropped_effects {
        crate::reactive::release(effect_id);
    }
}

/// Apply one binding's evaluated value to its DOM node. `prev`
/// is the cached serialisation from the last write; returns the
/// new cache slot to store. `None` return means "value shape
/// unrecognised — leave DOM alone, leave cache untouched".
fn apply_binding(
    el: &Element,
    kind: BindingKind,
    v: &JsValue,
    prev: Option<&str>,
) -> Option<Rc<str>> {
    match kind {
        BindingKind::Text => {
            let next = js_to_string(v);
            if prev == Some(next.as_str()) {
                return Some(Rc::from(next));
            }
            el.set_text_content(Some(&next));
            Some(Rc::from(next))
        }
        BindingKind::Class => {
            let serialised = serialise_class_value(v)?;
            if prev == Some(serialised.as_str()) {
                return Some(Rc::from(serialised));
            }
            if serialised.is_empty() {
                let _ = el.remove_attribute("class");
            } else {
                let _ = el.set_attribute("class", &serialised);
            }
            Some(Rc::from(serialised))
        }
        // RFC-058 Phase 2 template-plan variants. The keyed row
        // compiler never emits these (it only emits `Text` /
        // `Class`), so reaching this branch on the row-plan
        // path means a macro bug let a template-plan binding
        // ride a `StaticRowPlan`. Treat it as a framework bug
        // rather than silently dropping.
        BindingKind::Html | BindingKind::Bind { .. } | BindingKind::Show => {
            console::warn_1(&JsValue::from_str(
                "rfc-054: row plan received an RFC-058 template-plan-only \
                 BindingKind (Html / Bind / Show); skipping",
            ));
            None
        }
    }
}

fn resolve_listener_routes(plan: &CompiledRowPlan, row_root: &Element) -> Box<[RowListenerRoute]> {
    let mut routes: Vec<RowListenerRoute> = Vec::with_capacity(plan.listeners.len());
    for listener in &plan.listeners {
        let Some(node) = resolve_node_path(row_root, listener.node_path) else {
            console::warn_1(&JsValue::from_str(&format!(
                "rfc-054: row listener node_path {:?} did not resolve",
                listener.node_path,
            )));
            continue;
        };
        routes.push(RowListenerRoute {
            event: listener.event,
            node,
            ast: listener.ast.clone(),
        });
    }
    routes.into_boxed_slice()
}

// ─── helpers ────────────────────────────────────────────────────

/// Stringify a JsValue for `pp-text`. Mirrors text.rs.
fn js_to_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(|n| n.to_string()))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| {
            js_sys::JSON::stringify(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default()
        })
}

/// Class serialisation — mirrors `directives/bind.rs::serialise_class`
/// behaviour. String values pass through; object values join their
/// truthy keys.
///
/// Returns `None` for shapes the runtime explicitly leaves alone
/// (matching the existing `bind` behaviour — undefined / null /
/// false values get the attribute removed via the empty string;
/// non-string non-object values yield `None` so we don't touch DOM).
fn serialise_class_value(v: &JsValue) -> Option<String> {
    if v.is_undefined() || v.is_null() || v == &JsValue::FALSE {
        return Some(String::new());
    }
    if let Some(s) = v.as_string() {
        return Some(s);
    }
    if v.is_object() {
        use js_sys::{Object, Reflect};
        use wasm_bindgen::JsCast;
        let obj: Object = v.clone().unchecked_into();
        let keys = Object::keys(&obj);
        let mut out: Vec<String> = Vec::new();
        for i in 0..keys.length() {
            let k = keys.get(i);
            let truthy = Reflect::get(&obj, &k)
                .map(|val| val.as_bool().unwrap_or(!val.is_falsy()))
                .unwrap_or(false);
            if truthy {
                if let Some(s) = k.as_string() {
                    out.push(s);
                }
            }
        }
        return Some(out.join(" "));
    }
    None
}
