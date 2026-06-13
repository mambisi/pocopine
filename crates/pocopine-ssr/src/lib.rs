//! RFC-099 Phase 2 — server-side template stamper.
//!
//! Walks a compiled component's bindings + interpolations over its
//! **cleaned template HTML**, fills them from the component's state via
//! the host expression evaluator ([`pocopine_core::host_eval`]), and
//! serializes the result back to an HTML string — entirely host-side,
//! no wasm / no live DOM. The browser then *claims* this HTML during
//! hydration (Phase 2c).
//!
//! It mirrors the **string semantics** of the client's directive apply
//! functions (it does not call them — they mutate `web_sys` nodes):
//!
//! * `pp-text` → element text content (escaped on serialize),
//! * `pp-html` → element inner HTML (raw; only a string value),
//! * `pp-bind:<attr>` → set/remove an attribute (null/false remove;
//!   `class`/`style` object serialization; `data-*`/`aria-*` bool
//!   rendering),
//! * `pp-show` → merge `display:none` into the inline style when falsy,
//! * `{{interp}}` → replace the target text node with the rendered
//!   segments.
//!
//! Number coercion matches the client *per path*: text/interp use the
//! JS display formatter ([`pocopine_core::js_number`]); a plain `pp-bind`
//! attribute uses Rust `f64::to_string` (exactly what `bind.rs` does).
//!
//! **Structural directives (RFC-099 Phase 3).** `pp-if` / `pp-match` /
//! `pp-for` are expanded here too, recursively, using the body data the
//! macro now exposes (`StaticCondPlan::body_plan` / `body_html` & co):
//!
//! * `pp-if` chain → evaluate the head + `pp-else-if` exprs, render the
//!   first truthy branch's body (or the `pp-else`), drop the member
//!   `<template>`s, and emit a `<!--pp:cond-->` anchor after the clone,
//! * `pp-match` → tag-extract the matched value (serde external tagging),
//!   render the first matching case's body (augmenting state with the
//!   `pp-let` payload), emit `<!--pp:match-->`,
//! * `pp-for` → evaluate the items, render one body clone per item
//!   against state augmented with `{ item, $index, $first, $last }`,
//!   emit `<!--pp:for-->` after the rows.
//!
//! Sites expand in **reverse document order** so an earlier expansion
//! never shifts the element-child indices a later site resolves against
//! (mirrors the client's specialized install pass). A body that fell
//! outside the macro's lift envelope (`body_html`/`body_plan` = `None`)
//! is left as the authored `<template>` for the client to mount — never
//! mis-rendered. Client-side claiming of these anchors is Phase 3 Unit D.

use std::cell::RefCell;
use std::rc::Rc;

use html5ever::serialize::{SerializeOpts, TraversalScope};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::TreeBuilderOpts;
use html5ever::{
    local_name, namespace_url, ns, parse_fragment, serialize, Attribute, LocalName, ParseOpts,
    QualName,
};
use markup5ever_rcdom::{Handle, Node, NodeData, RcDom, SerializableHandle};
use pocopine_core::directives::for_plan::{
    BindingKind, StaticBinding, StaticChildMount, StaticCondPlan, StaticForPlan, StaticInterp,
    StaticMatchPlan,
};
use pocopine_core::directives::interp::PlannedSegment;
use pocopine_core::router;
use pocopine_core::templates::template_for;
use pocopine_core::templates_plan::{template_plan_for, StaticTemplatePlan};
use pocopine_core::{expr, host_eval, js_number, Component};
use serde_json::Value;

/// Stamp one component's `bindings` + `interps` into its `cleaned_html`,
/// reading values from `state`. Returns the rendered HTML. If the
/// cleaned HTML has no element root, it is returned unchanged.
pub fn stamp(
    cleaned_html: &str,
    bindings: &[StaticBinding],
    interps: &[StaticInterp],
    state: &Value,
) -> String {
    // `markup5ever_rcdom::RcDom` clears its whole tree on `Drop` (it
    // detaches children iteratively to avoid deep-recursion stack
    // overflow), so a parsed node is only valid while its `RcDom` is
    // alive. Keep every dom we parse (the template + any `pp-html`
    // re-parses, body fragments) alive until after serialization.
    let mut keep: Vec<RcDom> = Vec::new();
    let Some(root) = parse_root(cleaned_html, &mut keep) else {
        return cleaned_html.to_string();
    };
    apply_flat(&root, bindings, interps, state, &mut keep);
    let out = serialize_node(&root);
    drop(keep); // tree must outlive the serialize above
    out
}

/// Parse `html` and return its first element root (unwrapping the
/// html5ever fragment wrapper). `None` when the fragment has no element.
fn parse_root(html: &str, keep: &mut Vec<RcDom>) -> Option<Handle> {
    parse_fragment_into(html, local_name!("body"), keep)
        .into_iter()
        .find(|n| matches!(n.data, NodeData::Element { .. }))
}

/// Apply the flat (path-resolved) bindings + interps of a plan to an
/// already-parsed `root`. Shared by [`stamp`] and the recursive
/// fragment stamper.
fn apply_flat(
    root: &Handle,
    bindings: &[StaticBinding],
    interps: &[StaticInterp],
    state: &Value,
    keep: &mut Vec<RcDom>,
) {
    for b in bindings {
        if let Some(node) = walk_element_path(root, b.node_path) {
            apply_binding(&node, b, state, keep);
        }
    }
    for it in interps {
        if let Some(parent) = walk_element_path(root, it.node_path) {
            apply_interp(&parent, it, state);
        }
    }
}

/// Stamp a body fragment's cleaned HTML through its sub-plan (flat
/// entries + nested structural), returning the rendered root element.
/// `None` when `html` has no element root. Recursion point for nested
/// controllers inside a `pp-if` / `pp-match` / `pp-for` body.
fn stamp_fragment(
    html: &str,
    plan: &StaticTemplatePlan,
    state: &Value,
    keep: &mut Vec<RcDom>,
) -> Option<Handle> {
    let root = parse_root(html, keep)?;
    apply_flat(&root, plan.bindings, plan.interps, state, keep);
    expand_child_mounts(&root, plan, state, keep);
    expand_structural(&root, plan, state, keep);
    Some(root)
}

/// Whole-template stamp: flat entries + structural expansion.
fn stamp_with_plan(cleaned_html: &str, plan: &StaticTemplatePlan, state: &Value) -> String {
    let mut keep: Vec<RcDom> = Vec::new();
    let out = match stamp_fragment(cleaned_html, plan, state, &mut keep) {
        Some(root) => serialize_node(&root),
        None => cleaned_html.to_string(),
    };
    drop(keep);
    out
}

/// A server-rendered component: the stamped HTML plus the serialized
/// state the client deserializes during hydration (Phase 2c).
pub struct RenderedPage {
    /// Rendered HTML for the component subtree.
    pub body: String,
    /// Component state as JSON, already **script-safe**: every `<` is
    /// escaped to the JSON unicode escape `<` so the payload
    /// cannot break out of the `<script>` tag or open an HTML comment
    /// (still valid JSON — `<` parses back to `<`).
    pub state: String,
}

impl RenderedPage {
    /// The `<script type="application/json" data-pp-state>` island the
    /// client reads to rehydrate this component's state.
    pub fn state_island(&self) -> String {
        format!(
            "<script type=\"application/json\" data-pp-state>{}</script>",
            self.state
        )
    }

    /// `body` followed by the state island — the unit the client
    /// hydrates (mounts the body, reads the adjacent state).
    pub fn into_fragment(self) -> String {
        let island = self.state_island();
        let mut out = self.body;
        out.push_str(&island);
        out
    }
}

/// Render bindings + interps into `cleaned_html` AND capture `state` as
/// a script-safe JSON island — the decoupled core (no registry). See
/// [`render_to_string`] for the component-level entry.
pub fn render(
    cleaned_html: &str,
    bindings: &[StaticBinding],
    interps: &[StaticInterp],
    state: &Value,
) -> RenderedPage {
    RenderedPage {
        body: stamp(cleaned_html, bindings, interps, state),
        state: script_safe_json(state),
    }
}

/// Render a **registered** component instance to a [`RenderedPage`].
/// Looks its template + plan up by [`Component::NAME`]; returns `None`
/// if the component isn't registered (the caller registers components
/// on the server, the same way the client app does). State is the
/// component serialized via serde — so loader/`on_setup` results that
/// are already in the instance flow into the island.
pub fn render_to_string<C>(component: &C) -> Option<RenderedPage>
where
    C: Component + serde::Serialize,
{
    let html = template_for(C::NAME)?;
    let plan = template_plan_for(C::NAME)?;
    let state = serde_json::to_value(component).ok()?;
    Some(RenderedPage {
        body: stamp_with_plan(&html, plan, &state),
        state: script_safe_json(&state),
    })
}

/// RFC-099 Phase 4 — render a **routed app document**: stamp the root
/// component, resolve `url` to a route via [`router::resolve_route`], and
/// render that route component into the root's `<pp-outlet>` (with the
/// captured params as both attributes and prop state). `App::hydrate`
/// then claims the whole tree. Routes must be registered host-side (the
/// same `App::route::<C>(…)` builder the client uses). The returned
/// `state` is the ROOT's island; child + route islands are embedded
/// inline by the recursive render.
pub fn render_app_to_string<C>(root: &C, url: &str) -> Option<RenderedPage>
where
    C: Component + serde::Serialize,
{
    let html = template_for(C::NAME)?;
    let plan = template_plan_for(C::NAME)?;
    let state = serde_json::to_value(root).ok()?;
    let mut keep: Vec<RcDom> = Vec::new();
    let body = match stamp_fragment(&html, plan, &state, &mut keep) {
        Some(root_node) => {
            if let (Some(outlet), Some((route_tag, params))) =
                (find_outlet(&root_node), router::resolve_route(url))
            {
                render_route_into_outlet(&outlet, route_tag, &params, &mut keep);
            }
            serialize_node(&root_node)
        }
        None => html.into_owned(),
    };
    drop(keep);
    Some(RenderedPage {
        body,
        state: script_safe_json(&state),
    })
}

/// Find the first `<pp-outlet>` element anywhere under `node`.
fn find_outlet(node: &Handle) -> Option<Handle> {
    if let NodeData::Element { name, .. } = &node.data {
        if name.local.as_ref() == "pp-outlet" {
            return Some(node.clone());
        }
    }
    for child in node.children.borrow().iter() {
        if let Some(found) = find_outlet(child) {
            return Some(found);
        }
    }
    None
}

/// Render the matched route component into `<pp-outlet>` as a
/// `<route-tag param="value">` host (mirroring the router's
/// `finish_route_mount`), with the captured params as both host
/// attributes (for the client re-mount path) and prop state (for the
/// SSR render of the route's template).
fn render_route_into_outlet(
    outlet: &Handle,
    route_tag: &str,
    params: &std::collections::HashMap<String, String>,
    keep: &mut Vec<RcDom>,
) {
    let attrs: Vec<(&str, &str)> = params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let route_el = element_node(route_tag, &attrs);
    let mut props = serde_json::Map::new();
    for (k, v) in params {
        props.insert(prop_field(k), coerce_attr(v));
    }
    let route_state = Value::Object(props);
    if render_component_into_host(&route_el, route_tag, &route_state, keep) {
        route_el.parent.set(Some(Rc::downgrade(outlet)));
        *outlet.children.borrow_mut() = vec![route_el];
    }
}

/// Serialize `state` to JSON and neutralise `<` (→ `<`) so it is
/// safe to embed verbatim inside a `<script>` element.
fn script_safe_json(state: &Value) -> String {
    serde_json::to_string(state)
        .unwrap_or_else(|_| "null".to_string())
        .replace('<', "\\u003c")
}

// ─── HTML parse / walk / serialize (html5ever RcDom) ───────────────

/// Parse an HTML fragment in the given context element, returning the
/// real top-level nodes (unwrapping html5ever's synthetic `<html>`
/// fragment wrapper — same recipe as `pocopine-template-parser`). The
/// owning `RcDom` is pushed into `keep` so the returned nodes stay
/// valid (see [`stamp`] for why).
fn parse_fragment_into(html: &str, ctx: LocalName, keep: &mut Vec<RcDom>) -> Vec<Handle> {
    let opts = ParseOpts {
        tree_builder: TreeBuilderOpts {
            drop_doctype: true,
            ..TreeBuilderOpts::default()
        },
        ..ParseOpts::default()
    };
    let context = QualName::new(None, ns!(html), ctx);
    let dom = parse_fragment(RcDom::default(), opts, context, Vec::new()).one(html.to_string());
    let roots = {
        let doc = dom.document.children.borrow();
        doc.iter()
            .find_map(|child| match &child.data {
                NodeData::Element { name, .. } if name.local == local_name!("html") => {
                    Some(child.children.borrow().clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| doc.clone())
    };
    keep.push(dom);
    roots
}

/// Walk a node path of **element-child** indices (text/comment nodes
/// don't shift the index — matching the client's
/// `first_element_child`/`next_element_sibling` walk and the macro's
/// element-only `node_path`). An empty path is the root itself.
fn walk_element_path(root: &Handle, path: &[u16]) -> Option<Handle> {
    let mut cur = root.clone();
    for &idx in path {
        let next = cur
            .children
            .borrow()
            .iter()
            .filter(|c| matches!(c.data, NodeData::Element { .. }))
            .nth(idx as usize)
            .cloned();
        cur = next?;
    }
    Some(cur)
}

fn serialize_node(node: &Handle) -> String {
    let mut buf = Vec::new();
    let opts = SerializeOpts {
        traversal_scope: TraversalScope::IncludeNode,
        ..SerializeOpts::default()
    };
    serialize(&mut buf, &SerializableHandle::from(node.clone()), opts).expect("serialize RcDom");
    String::from_utf8(buf).expect("serialized HTML is UTF-8")
}

fn text_node(text: &str) -> Handle {
    Node::new(NodeData::Text {
        contents: RefCell::new(StrTendril::from(text)),
    })
}

// ─── binding application (mirrors directives::{text,html,bind,show}) ─

fn apply_binding(node: &Handle, b: &StaticBinding, state: &Value, keep: &mut Vec<RcDom>) {
    let v = eval_src(b.expr_src, state);
    match &b.kind {
        BindingKind::Text => set_text_content(node, &display_string(&v)),
        BindingKind::Html => set_inner_html(node, v.as_str(), keep),
        BindingKind::Show => {
            if is_falsy(&v) {
                merge_display_none(node);
            }
        }
        BindingKind::Bind { arg } => apply_bind(node, arg, &v),
        // `Class` is emitted only by RFC-054 row plans, never whole-
        // template plans (mirrors `install_static_binding`).
        BindingKind::Class => {}
    }
}

/// `textContent` — replace all children with a single text node.
fn set_text_content(node: &Handle, text: &str) {
    let t = text_node(text);
    t.parent.set(Some(Rc::downgrade(node)));
    *node.children.borrow_mut() = vec![t];
}

/// `innerHTML` — re-parse the (string) value in this element's context
/// and replace its children. A non-string value clears the children.
fn set_inner_html(node: &Handle, html: Option<&str>, keep: &mut Vec<RcDom>) {
    let ctx = match &node.data {
        NodeData::Element { name, .. } => name.local.clone(),
        _ => local_name!("div"),
    };
    let kids = match html {
        Some(h) => parse_fragment_into(h, ctx, keep),
        None => Vec::new(),
    };
    for k in &kids {
        k.parent.set(Some(Rc::downgrade(node)));
    }
    *node.children.borrow_mut() = kids;
}

fn apply_bind(node: &Handle, arg: &str, v: &Value) {
    // Falsy removal: undefined/null/false → drop the attribute (the
    // client's `apply_memoised` does this before serialization).
    if matches!(v, Value::Null | Value::Bool(false)) {
        remove_attribute(node, arg);
        return;
    }
    let serialised = match arg {
        "class" => serialise_class(v),
        "style" => serialise_style(v),
        _ => Some(serialise_plain(arg, v)),
    };
    if let Some(s) = serialised {
        set_attribute(node, arg, &s);
    }
}

fn serialise_class(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        // Object: space-join truthy keys (key order follows the map).
        Value::Object(map) => Some(
            map.iter()
                .filter(|(_, val)| !is_falsy(val))
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

fn serialise_style(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => Some(
            map.iter()
                .map(|(k, val)| format!("{k}:{};", val.as_str().unwrap_or_default()))
                .collect::<String>(),
        ),
        _ => None,
    }
}

fn serialise_plain(attr: &str, v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        // Plain attrs use Rust `f64::to_string` (exactly `bind.rs`),
        // NOT the JS display formatter used for text/interp.
        Value::Number(n) => n.as_f64().map(|f| f.to_string()).unwrap_or_default(),
        // `false` is removed upstream; only `true` reaches here.
        Value::Bool(_) => {
            if attr.starts_with("data-") || attr.starts_with("aria-") {
                "true".to_string()
            } else {
                String::new() // present-with-empty (classic attr `true`)
            }
        }
        // Arrays/objects → JSON (mirrors `JSON.stringify`).
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// `pp-show` falsy → merge `display:none` into the inline `style` attr.
fn merge_display_none(node: &Handle) {
    let mut style = get_attribute(node, "style").unwrap_or_default();
    let trimmed = style.trim_end();
    if !trimmed.is_empty() && !trimmed.ends_with(';') {
        style.push(';');
    }
    style.push_str("display:none;");
    set_attribute(node, "style", &style);
}

// ─── interpolation ─────────────────────────────────────────────────

fn apply_interp(parent: &Handle, it: &StaticInterp, state: &Value) {
    let rendered = render_segments(it.segments, state);
    // Replace the `text_index`-th TEXT-node child (text nodes only
    // count, matching `interp::resolve_text_target`).
    {
        let children = parent.children.borrow();
        let mut seen = 0u16;
        for c in children.iter() {
            if let NodeData::Text { ref contents } = c.data {
                if seen == it.text_index {
                    *contents.borrow_mut() = StrTendril::from(rendered.as_str());
                    return;
                }
                seen += 1;
            }
        }
    }
    // Target text node absent (cleaning may have collapsed an empty
    // one). Best-effort append — real-component placement is gated by
    // the Phase-2c differential harness.
    let t = text_node(&rendered);
    t.parent.set(Some(Rc::downgrade(parent)));
    parent.children.borrow_mut().push(t);
}

fn render_segments(segments: &[PlannedSegment], state: &Value) -> String {
    let mut out = String::new();
    for seg in segments {
        match seg {
            PlannedSegment::Static(t) => out.push_str(t),
            PlannedSegment::Dynamic(src) => out.push_str(&display_string(&eval_src(src, state))),
        }
    }
    out
}

// ─── structural directives (pp-if / pp-match / pp-for) ─────────────

/// Expand every `pp-if` / `pp-match` / `pp-for` site in `plan` against
/// `state`, in **reverse document order** (by template node path) so an
/// earlier expansion never invalidates a later site's path resolution.
fn expand_structural(
    root: &Handle,
    plan: &StaticTemplatePlan,
    state: &Value,
    keep: &mut Vec<RcDom>,
) {
    // The kind-local index travels with each site so the anchor can be
    // labelled `pp:<kind>:<index>` — the client claimer (Unit D) finds
    // its anchor by that label (scanning the parent's child comments),
    // which is robust even when no clone precedes the anchor.
    enum Site<'a> {
        Cond(usize, &'a StaticCondPlan),
        Match(usize, &'a StaticMatchPlan),
        For(usize, &'a StaticForPlan),
    }
    let mut sites: Vec<(&[u16], Site)> = Vec::new();
    for (i, cp) in plan.if_plans.iter().enumerate() {
        sites.push((cp.template_node_path, Site::Cond(i, cp)));
    }
    for (i, mp) in plan.match_plans.iter().enumerate() {
        sites.push((mp.template_node_path, Site::Match(i, mp)));
    }
    for (i, fp) in plan.for_plans.iter().enumerate() {
        sites.push((fp.template_node_path, Site::For(i, fp)));
    }
    sites.sort_by(|a, b| b.0.cmp(a.0));
    for (path, site) in sites {
        // Scope-root templates (empty path) stay put — the client's
        // scope-root exception keeps the `<template>`; SSR leaves it.
        if path.is_empty() {
            continue;
        }
        let Some(tpl) = walk_element_path(root, path) else {
            continue;
        };
        if !is_template(&tpl) {
            continue;
        }
        match site {
            Site::Cond(i, cp) => expand_cond(&tpl, i, cp, state, keep),
            Site::Match(i, mp) => expand_match(&tpl, i, mp, state, keep),
            Site::For(i, fp) => expand_for(&tpl, i, fp, state, keep),
        }
    }
}

/// `pp-if` chain: render the first truthy branch (or `pp-else`), drop
/// the member `<template>`s, and place a `<!--pp:cond:idx-->` anchor
/// after the clone (matching the client's clone-before-anchor layout).
fn expand_cond(
    tpl: &Handle,
    idx: usize,
    cp: &StaticCondPlan,
    state: &Value,
    keep: &mut Vec<RcDom>,
) {
    // Expand only when EVERY branch is liftable as data: post-hydration
    // the client controller may flip to any branch, and the member
    // `<template>`s the legacy clone-fallback needs are gone from the
    // SSR output. If any branch fell outside the lift envelope, leave
    // the authored chain for the client to mount (never mis-render).
    let head_liftable = cp.body_html.is_some() && cp.body_plan.is_some();
    let elifs_liftable = cp
        .else_if
        .iter()
        .all(|br| br.body_html.is_some() && br.body_plan.is_some());
    let else_liftable =
        !cp.has_else || (cp.else_body_html.is_some() && cp.else_body_plan.is_some());
    if !(head_liftable && elifs_liftable && else_liftable) {
        return;
    }

    // First truthy branch among [head, else-if…], else the `pp-else`.
    let active: Option<(&str, &StaticTemplatePlan)> = if !is_falsy(&eval_src(cp.expr_src, state)) {
        Some((cp.body_html.unwrap(), cp.body_plan.unwrap()))
    } else {
        cp.else_if
            .iter()
            .find(|br| !is_falsy(&eval_src(br.expr_src, state)))
            .map(|br| (br.body_html.unwrap(), br.body_plan.unwrap()))
            .or(if cp.has_else {
                Some((cp.else_body_html.unwrap(), cp.else_body_plan.unwrap()))
            } else {
                None
            })
    };

    let body_node = match active {
        Some((html, plan)) => match stamp_fragment(html, plan, state, keep) {
            Some(n) => Some(n),
            None => return,
        },
        // No branch is active: just the anchor, no clone.
        None => None,
    };

    let mut new_nodes: Vec<Handle> = Vec::new();
    new_nodes.extend(body_node);
    new_nodes.push(comment_node(&format!("pp:cond:{idx}")));
    replace_template_chain(tpl, cp.consumed_count, new_nodes);
}

/// `pp-match`: tag-extract the value, render the first matching case
/// (augmenting state with the `pp-let` payload), `<!--pp:match-->`.
fn expand_match(
    tpl: &Handle,
    idx: usize,
    mp: &StaticMatchPlan,
    state: &Value,
    keep: &mut Vec<RcDom>,
) {
    let value = eval_src(mp.expr_src, state);
    let (tag, payload) = extract_tag(&value);
    let active = mp.cases.iter().find(|c| {
        c.tags.is_empty() || tag.as_deref().map(|t| c.tags.contains(&t)).unwrap_or(false)
    });

    let body_node = match active {
        Some(case) => match (case.body_html, case.body_plan) {
            (Some(html), Some(plan)) => {
                // `pp-let`: bind the payload (whole value for the `_`
                // wildcard arm, the inner payload otherwise — mirrors
                // `install_match`'s same-tag update).
                let scoped;
                let st = match case.bind_name {
                    Some(name) => {
                        let bound = if case.tags.is_empty() {
                            value.clone()
                        } else {
                            payload.clone()
                        };
                        scoped = augment(state, vec![(name, bound)]);
                        &scoped
                    }
                    None => state,
                };
                match stamp_fragment(html, plan, st, keep) {
                    Some(n) => Some(n),
                    None => return,
                }
            }
            // Matched case unliftable — leave for client mount.
            _ => return,
        },
        // No case matched — nothing renders, just the anchor.
        None => None,
    };

    let mut new_nodes: Vec<Handle> = Vec::new();
    new_nodes.extend(body_node);
    new_nodes.push(comment_node(&format!("pp:match:{idx}")));
    replace_template_chain(tpl, 0, new_nodes);
}

/// `pp-for`: render one body clone per item against state augmented
/// with `{ item_name, $index, $first, $last }`, then `<!--pp:for-->`.
fn expand_for(tpl: &Handle, idx: usize, fp: &StaticForPlan, state: &Value, keep: &mut Vec<RcDom>) {
    let (html, plan) = match (fp.body_html, fp.body_plan) {
        (Some(h), Some(p)) => (h, p),
        // Row body outside the lift envelope — leave for client mount.
        _ => return,
    };
    let items = match eval_src(fp.items_expr, state) {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    let len = items.len();
    let mut new_nodes: Vec<Handle> = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        let row_state = augment(
            state,
            vec![
                (fp.item_name, item),
                ("$index", Value::from(i)),
                ("$first", Value::Bool(i == 0)),
                ("$last", Value::Bool(i + 1 == len)),
            ],
        );
        if let Some(n) = stamp_fragment(html, plan, &row_state, keep) {
            new_nodes.push(n);
        }
    }
    new_nodes.push(comment_node(&format!("pp:for:{idx}")));
    replace_template_chain(tpl, 0, new_nodes);
}

/// Extract `(tag, payload)` from a matched value, mirroring the
/// client's `extract_tag` over serde external tagging: a string IS its
/// tag (payload absent ⇒ `Null`); a one-key object is `(key, value)`;
/// anything else has no tag (only the `_` wildcard arm matches).
fn extract_tag(v: &Value) -> (Option<String>, Value) {
    match v {
        Value::String(s) => (Some(s.clone()), Value::Null),
        Value::Object(map) if map.len() == 1 => {
            let (k, val) = map.iter().next().expect("len == 1");
            (Some(k.clone()), val.clone())
        }
        _ => (None, Value::Null),
    }
}

/// `base` (a JSON object) with `extra` keys inserted/overwritten — used
/// to thread `pp-for` loop-locals and the `pp-match` `pp-let` payload
/// into a body fragment's render state.
fn augment(base: &Value, extra: Vec<(&str, Value)>) -> Value {
    let mut map = base.as_object().cloned().unwrap_or_default();
    for (k, v) in extra {
        map.insert(k.to_string(), v);
    }
    Value::Object(map)
}

fn is_template(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Element { name, .. } if name.local == local_name!("template"))
}

fn comment_node(text: &str) -> Handle {
    Node::new(NodeData::Comment {
        contents: StrTendril::from(text),
    })
}

/// The (live) parent of `node`, via its weak back-pointer.
fn parent_of(node: &Handle) -> Option<Handle> {
    let weak = node.parent.take();
    let parent = weak.as_ref().and_then(|w| w.upgrade());
    node.parent.set(weak);
    parent
}

/// Replace `tpl` (a structural `<template>`) plus the next
/// `consumed_count` element siblings (the `pp-else-if` / `pp-else`
/// chain members) with `new_nodes`, in place. Nodes interleaved within
/// the consumed range (whitespace text) are removed with them.
fn replace_template_chain(tpl: &Handle, consumed_count: u16, new_nodes: Vec<Handle>) {
    let Some(parent) = parent_of(tpl) else {
        return;
    };
    let mut children = parent.children.borrow_mut();
    let Some(start) = children.iter().position(|c| Rc::ptr_eq(c, tpl)) else {
        return;
    };
    // Extend the removed range over `consumed_count` following element
    // siblings (and anything between them).
    let mut end = start;
    let mut consumed = 0u16;
    let mut i = start + 1;
    while consumed < consumed_count && i < children.len() {
        if matches!(children[i].data, NodeData::Element { .. }) {
            consumed += 1;
            end = i;
        }
        i += 1;
    }
    for n in &new_nodes {
        n.parent.set(Some(Rc::downgrade(&parent)));
    }
    children.splice(start..=end, new_nodes);
}

// ─── child components (recursive SSR) ──────────────────────────────

/// Recursively render each child-component mount site in `plan` whose
/// content is server-renderable (no parent-authored slots — slots stay
/// client-materialized for now: their fragments are wasm-only `fn`s).
/// For each: build the child's state from its host attributes (static
/// props) + the parent-scope `:prop` binds, look up the child's cleaned
/// HTML + plan, stamp it recursively INTO the host tag (mirroring
/// mount's `host.innerHTML = template`), and append the child's own
/// `data-pp-state` island so the client can claim it.
fn expand_child_mounts(
    root: &Handle,
    plan: &StaticTemplatePlan,
    parent_state: &Value,
    keep: &mut Vec<RcDom>,
) {
    for cm in plan.child_mounts {
        if !cm.slots.is_empty() {
            continue;
        }
        let Some(host) = walk_element_path(root, cm.node_path) else {
            continue;
        };
        let child_state = child_state_value(&host, cm, parent_state);
        render_component_into_host(&host, cm.tag, &child_state, keep);
    }
}

/// Render component `tag` with `state` recursively, then place its root
/// element and its `data-pp-state` island as the children of `host`
/// (mirroring mount's `host.innerHTML = template`). Shared by child-mount
/// expansion and route SSR. Returns `false` if `tag` isn't a
/// registered/plannable component (host left untouched → client mounts).
fn render_component_into_host(
    host: &Handle,
    tag: &str,
    state: &Value,
    keep: &mut Vec<RcDom>,
) -> bool {
    let (Some(html), Some(plan)) = (template_for(tag), template_plan_for(tag)) else {
        return false;
    };
    let Some(root) = stamp_fragment(&html, plan, state, keep) else {
        return false;
    };
    let island = state_island_node(state);
    root.parent.set(Some(Rc::downgrade(host)));
    island.parent.set(Some(Rc::downgrade(host)));
    *host.children.borrow_mut() = vec![root, island];
    true
}

/// Build a child component's render state: static props from the host
/// element's plain attributes (kebab→snake, coerced) overlaid with the
/// parent-scope `:prop` / `pp-bind:prop` binds (evaluated against the
/// parent state). Own `#[state]` fields default to absent (the binding
/// evaluators read them as null) — host-side default-state execution is
/// a later layer.
fn child_state_value(host: &Handle, cm: &StaticChildMount, parent_state: &Value) -> Value {
    let mut props = serde_json::Map::new();
    if let NodeData::Element { attrs, .. } = &host.data {
        for a in attrs.borrow().iter() {
            let name = a.name.local.as_ref();
            if name.starts_with("pp-") || name.starts_with("data-pp-") || name.starts_with("__pp") {
                continue;
            }
            props.insert(prop_field(name), coerce_attr(&a.value));
        }
    }
    for b in cm.bindings {
        props.insert(prop_field(b.arg), eval_src(b.expr_src, parent_state));
    }
    Value::Object(props)
}

/// kebab-case attribute → snake_case field (mirrors `normalize_prop_name`).
fn prop_field(name: &str) -> String {
    name.replace('-', "_")
}

/// Coerce a static HTML attribute string to a JSON value: bool / number
/// literals parse, everything else stays a string. (Loosely mirrors
/// `coerce_attr_value`'s Auto kind — host-side we lack the per-prop
/// declared kind, so a `String` prop whose value is literally `"true"`
/// would coerce to a bool; rare, documented.)
fn coerce_attr(raw: &str) -> Value {
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(raw.to_string())),
    }
}

/// `<script type="application/json" data-pp-state>JSON</script>` element
/// node carrying the (script-safe) state for a server-rendered child
/// root, so the client's `hydrate_root` can claim it.
fn state_island_node(state: &Value) -> Handle {
    let script = element_node(
        "script",
        &[("type", "application/json"), ("data-pp-state", "")],
    );
    let text = text_node(&script_safe_json(state));
    text.parent.set(Some(Rc::downgrade(&script)));
    *script.children.borrow_mut() = vec![text];
    script
}

/// Create an html5ever element node with the given local name + attrs.
fn element_node(name: &str, attrs: &[(&str, &str)]) -> Handle {
    Node::new(NodeData::Element {
        name: QualName::new(None, ns!(html), LocalName::from(name)),
        attrs: RefCell::new(
            attrs
                .iter()
                .map(|(n, v)| Attribute {
                    name: attr_qual(n),
                    value: StrTendril::from(*v),
                })
                .collect(),
        ),
        template_contents: RefCell::new(None),
        mathml_annotation_xml_integration_point: false,
    })
}

// ─── attribute helpers ─────────────────────────────────────────────

fn attr_qual(name: &str) -> QualName {
    QualName::new(None, ns!(), LocalName::from(name))
}

fn set_attribute(node: &Handle, name: &str, value: &str) {
    if let NodeData::Element { ref attrs, .. } = node.data {
        let qn = attr_qual(name);
        let mut attrs = attrs.borrow_mut();
        if let Some(existing) = attrs.iter_mut().find(|a| a.name == qn) {
            existing.value = StrTendril::from(value);
        } else {
            attrs.push(Attribute {
                name: qn,
                value: StrTendril::from(value),
            });
        }
    }
}

fn remove_attribute(node: &Handle, name: &str) {
    if let NodeData::Element { ref attrs, .. } = node.data {
        let qn = attr_qual(name);
        attrs.borrow_mut().retain(|a| a.name != qn);
    }
}

fn get_attribute(node: &Handle, name: &str) -> Option<String> {
    if let NodeData::Element { ref attrs, .. } = node.data {
        let qn = attr_qual(name);
        return attrs
            .borrow()
            .iter()
            .find(|a| a.name == qn)
            .map(|a| a.value.to_string());
    }
    None
}

// ─── value → string coercion + eval ────────────────────────────────

/// Display coercion for text / interp — mirrors `directives` `js_to_string`:
/// null → "", string → itself, number → JS formatter, bool →
/// "true"/"false", array/object → JSON.
fn display_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(js_number::to_js_string).unwrap_or_default(),
        Value::Bool(b) => b.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// JS truthiness (mirrors `host_eval`'s, which mirrors `JsValue::is_falsy`).
fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f == 0.0),
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}

fn eval_src(src: &str, state: &Value) -> Value {
    match expr::parse(src) {
        Ok(ast) => host_eval::eval(&ast, state),
        Err(_) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::stamp;
    use pocopine_core::directives::for_plan::{BindingKind, StaticBinding, StaticInterp};
    use pocopine_core::directives::interp::PlannedSegment;
    use serde_json::json;

    #[test]
    fn stamps_text_bind_show_and_interp() {
        // root <div>; element children: [0]=<span>, [1]=<p>, [2]=<b>.
        let html = r#"<div data-pp-scope-id="demo"><span class="t"></span><p style="color:red"></p><b>placeholder</b></div>"#;
        let bindings: &[StaticBinding] = &[
            StaticBinding {
                node_path: &[0],
                kind: BindingKind::Text,
                expr_src: "title",
                compiled: None,
            },
            StaticBinding {
                node_path: &[1],
                kind: BindingKind::Bind { arg: "data-n" },
                expr_src: "count",
                compiled: None,
            },
            StaticBinding {
                node_path: &[1],
                kind: BindingKind::Show,
                expr_src: "visible",
                compiled: None,
            },
        ];
        let interps: &[StaticInterp] = &[StaticInterp {
            node_path: &[2],
            text_index: 0,
            segments: &[
                PlannedSegment::Static("hello "),
                PlannedSegment::Dynamic("name"),
            ],
        }];
        let state = json!({ "title": "Hi & bye", "count": 5, "visible": false, "name": "world" });
        let out = stamp(html, bindings, interps, &state);

        // pp-text: textContent escaped on serialize
        assert!(
            out.contains("<span class=\"t\">Hi &amp; bye</span>"),
            "text: {out}"
        );
        // pp-bind: data-n attribute (Rust number formatting)
        assert!(out.contains("data-n=\"5\""), "bind: {out}");
        // pp-show falsy: display:none merged into existing style
        assert!(out.contains("display:none"), "show: {out}");
        // interp: static + dynamic, replacing the placeholder text node
        assert!(out.contains("<b>hello world</b>"), "interp: {out}");
    }

    #[test]
    fn bind_removes_attr_on_falsy_and_serialises_class_object() {
        let html = r#"<div><a href="/old" class="base"></a></div>"#;
        let bindings: &[StaticBinding] = &[
            // null → remove `href`
            StaticBinding {
                node_path: &[0],
                kind: BindingKind::Bind { arg: "href" },
                expr_src: "link",
                compiled: None,
            },
            // class object → space-joined truthy keys
            StaticBinding {
                node_path: &[0],
                kind: BindingKind::Bind { arg: "class" },
                expr_src: "classes",
                compiled: None,
            },
        ];
        let state =
            json!({ "link": null, "classes": { "active": true, "muted": false, "lg": true } });
        let out = stamp(html, bindings, &[], &state);
        assert!(!out.contains("href"), "href should be removed: {out}");
        // BTreeMap key order (active, lg) — both truthy, `muted` dropped.
        assert!(out.contains("class=\"active lg\""), "class object: {out}");
    }

    #[test]
    fn render_produces_body_and_script_safe_state_island() {
        let html = r#"<div><span></span></div>"#;
        let bindings: &[StaticBinding] = &[StaticBinding {
            node_path: &[0],
            kind: BindingKind::Text,
            expr_src: "msg",
            compiled: None,
        }];
        let state = json!({ "msg": "hi", "evil": "</script><script>alert(1)" });
        let page = super::render(html, bindings, &[], &state);

        assert!(page.body.contains("<span>hi</span>"), "body: {}", page.body);
        // The breakout attempt is neutralised: no literal `</script>` and
        // every `<` is the JSON unicode escape.
        assert!(
            !page.state.contains("</script>"),
            "state must be script-safe: {}",
            page.state
        );
        assert!(
            page.state.contains("\\u003c"),
            "`<` escaped: {}",
            page.state
        );

        let island = page.state_island();
        assert!(island.starts_with("<script type=\"application/json\" data-pp-state>"));
        assert!(island.ends_with("</script>"));
        // The only `</script>` is the legitimate closing tag.
        assert_eq!(island.matches("</script>").count(), 1, "breakout: {island}");
    }
}
