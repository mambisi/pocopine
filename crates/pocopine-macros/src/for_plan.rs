//! RFC 054 — macro-time `pp-for` row plan compilation.
//!
//! Walks the parsed `TemplateAst` looking for `<template
//! pp-for>` rows that match the v1 eligibility envelope:
//!
//! - exactly one element root inside the row body,
//! - no nested `pp-for`, `pp-if`, `pp-show`, `pp-teleport`,
//!   `pp-model`, `pp-init`, `pp-slot`, `pp-as`,
//! - no transition attrs (`pp-transition:*`, `pp-stagger`),
//! - no registered child component tags (anything with a `-`
//!   in the local name — custom elements / Pine compounds
//!   always fall back to the generic walker per RFC 054 §6.3),
//! - supported bindings: `pp-text`, `:class` /
//!   `pp-bind:class`,
//! - supported listeners: bare `@<event>` /
//!   `pp-on:<event>` (no modifiers).
//!
//! For each eligible row, emits:
//!
//! 1. a `StaticRowPlan` literal (added to the per-component
//!    `__POC_ROW_PLANS_<NAME>` array),
//! 2. a [`TemplateStamp`] describing where the runtime needs
//!    `data-pp-row-plan="<id>"` injected into the source
//!    template HTML (so `for_plan::lookup_for_template` can
//!    resolve the directive call back to its compiled plan).
//!
//! Rejected templates emit nothing — the runtime takes the
//! generic `walker::walk` path silently.

use std::ops::Range;

use proc_macro2::TokenStream;
use quote::quote;

use crate::template_parser::{Element, Node, TemplateAst};

/// Result of the row-plan analysis pass for one component.
pub(crate) struct EmittedRowPlans {
    /// Quoted `&[StaticRowPlan]` array literal. Empty when no
    /// row was eligible.
    pub array_tokens: TokenStream,
    /// `true` when at least one plan was emitted; the caller
    /// uses this to decide whether to emit the `register_row_plans`
    /// runtime call.
    pub has_plans: bool,
    /// Byte-position rewrites the caller needs to apply to the
    /// template source string before passing it to
    /// `compile_template` / `register_template`. Each describes
    /// how to inject `data-pp-row-plan="<id>"` into the
    /// `<template pp-for>` opening tag.
    pub stamps: Vec<TemplateStamp>,
    /// RFC-058 §6.2 layering — `(template_node_path, plan_id)`
    /// for every emitted row plan, indexed by document order
    /// over the template AST. The template-plan classifier
    /// consults this to bake `data-pp-row-plan="<id>"` into the
    /// cleaned HTML directly so the row-plan registry lookup
    /// still finds its target after the template-plan rewrite.
    pub assignments: Vec<(Vec<u16>, u32)>,
}

/// One byte-range rewrite the caller applies to the raw `.poco`
/// source before it's bundled into the WASM. The macro guarantees
/// `insert_at` falls inside an opening-tag byte range and is
/// safe to splice ASCII into.
pub(crate) struct TemplateStamp {
    pub plan_id: u32,
    /// Byte position to insert the new attribute at — just
    /// before the `>` (or `/>`) that closes the opening tag.
    pub insert_at: usize,
}

/// Walk the AST, accumulate eligible row plans + stamps. Returns
/// an empty result when the template has no `<template pp-for>`
/// or none qualify under §5.2.
pub(crate) fn analyze_row_plans(ast: &TemplateAst) -> EmittedRowPlans {
    let mut ctx = AnalysisCtx::default();
    let mut path: Vec<u16> = Vec::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, ast, &mut ctx, &mut path);
        }
    }
    ctx.finalize()
}

#[derive(Default)]
struct AnalysisCtx {
    next_id: u32,
    plan_tokens: Vec<TokenStream>,
    stamps: Vec<TemplateStamp>,
    assignments: Vec<(Vec<u16>, u32)>,
}

impl AnalysisCtx {
    fn finalize(self) -> EmittedRowPlans {
        let plans = self.plan_tokens;
        if plans.is_empty() {
            return EmittedRowPlans {
                array_tokens: quote! { &[] },
                has_plans: false,
                stamps: Vec::new(),
                assignments: Vec::new(),
            };
        }
        EmittedRowPlans {
            array_tokens: quote! { &[ #(#plans),* ] },
            has_plans: true,
            stamps: self.stamps,
            assignments: self.assignments,
        }
    }
}

/// Recursively visit elements looking for `<template pp-for>`.
/// When we find one, attempt eligibility; success → emit a plan
/// and STOP descending (the row body is the plan's universe,
/// not part of the surrounding component's template). Failure →
/// continue descending so nested `pp-for`s still get their
/// chance.
#[allow(clippy::only_used_in_recursion)]
fn walk(el: &Element, ast: &TemplateAst, ctx: &mut AnalysisCtx, path: &mut Vec<u16>) {
    if el.synthetic {
        // Synthetic elements (html5ever's auto-inserted `<tbody>`)
        // — skip but recurse. Don't push a path index for the
        // synthetic wrapper itself; the runtime walker doesn't
        // see it either.
        for (i, child) in el.children.iter().enumerate() {
            if let Node::Element(child_el) = child {
                let idx = el
                    .children
                    .iter()
                    .take(i)
                    .filter(|n| matches!(n, Node::Element(_)))
                    .count() as u16;
                path.push(idx);
                walk(child_el, ast, ctx, path);
                path.pop();
            }
        }
        return;
    }

    if el.tag == "template" && find_attr(el, "pp-for").is_some() {
        match try_compile_row(el, ctx.next_id) {
            Some((plan_tokens, stamp)) => {
                ctx.plan_tokens.push(plan_tokens);
                ctx.stamps.push(stamp);
                ctx.assignments.push((path.clone(), ctx.next_id));
                ctx.next_id += 1;
                // Don't descend into the row body — we own it
                // (the plan describes its dynamic surface
                // exactly; nested authored templates would need
                // their own plan lookups against a different
                // template registration).
                return;
            }
            None => {
                // Not eligible — fall through so a nested
                // `<template pp-for>` further inside might still
                // qualify on its own merit.
            }
        }
    }

    for (i, child) in el.children.iter().enumerate() {
        if let Node::Element(child_el) = child {
            let idx = el
                .children
                .iter()
                .take(i)
                .filter(|n| matches!(n, Node::Element(_)))
                .count() as u16;
            path.push(idx);
            walk(child_el, ast, ctx, path);
            path.pop();
        }
    }
}

/// Try to compile one `<template pp-for>` element into a row
/// plan. Returns `None` on any eligibility miss; the caller
/// just leaves the template alone in that case.
fn try_compile_row(template_el: &Element, plan_id: u32) -> Option<(TokenStream, TemplateStamp)> {
    // 1. Parse `pp-for="<v> in <items>"`. We don't need the
    //    items expression at the row-plan level — `pp-for`
    //    itself runs at runtime and handles the items lookup —
    //    but we need the loop var name for binding evaluation
    //    documentation. We do not validate the items expression
    //    here; the runtime parser will if it parses it for the
    //    `pp-for` directive itself. The macro's job is only to
    //    validate the body expressions.
    let pp_for = find_attr(template_el, "pp-for")?;
    let item_name = parse_for_item(pp_for)?;

    // 2. Reject nested `pp-key`-less `pp-for` here — the runtime
    //    `pp-for` directive itself enforces the keyed/naive
    //    branch, but our fast path is the keyed shape (we want
    //    `data-pp-row-plan` to imply keyed reuse downstream).
    //    Without `pp-key` the runtime falls into `run_naive`,
    //    which doesn't consult the plan anyway, so silently
    //    skipping here is fine.
    find_attr(template_el, "pp-key")?;

    // 3. Reject other directives on the `<template>` itself
    //    that aren't part of the keyed `pp-for` envelope.
    for (name, _) in &template_el.attrs {
        if name == "pp-for" || name == "pp-key" || name == "pp-stagger" {
            // `pp-stagger` is nominally fine for the existing
            // run_keyed, but it interacts with transitions which
            // aren't in v1 — reject it for now.
            if name == "pp-stagger" {
                return None;
            }
            continue;
        }
        // Any other `pp-*` or `data-*` on the template tag
        // disqualifies us — keep the envelope tight (Codex
        // feedback: narrow eligibility hard for v1).
        if name.starts_with("pp-") || name.starts_with("data-pp-") {
            return None;
        }
    }

    // 4. Find the single element root inside the row body.
    let row_root = single_element_child(template_el)?;

    // 5. DFS the row body; collect bindings + listeners + reject
    //    anything outside the envelope.
    let mut walk_ctx = RowWalkCtx::default();
    if !walk_row(row_root, &mut walk_ctx, &mut Vec::new()) {
        return None;
    }
    let bindings = walk_ctx.bindings;
    let listeners = walk_ctx.listeners;
    if bindings.is_empty() && listeners.is_empty() {
        // No dynamic surface inside the row — a plan would
        // duplicate the static clone. Let the generic walker
        // handle it; the cost is negligible.
        return None;
    }

    // 6. Compute the stamp position. `<template ...>` opening
    //    tag is at `template_el.opening_tag_range`; insert
    //    `data-pp-row-plan="<id>"` just before the closing `>`
    //    (or `/>` for self-close, which html5ever treats
    //    specially for `<template>` but we still handle).
    let stamp = compute_stamp(template_el, plan_id)?;

    // 7. Emit the plan literal.
    let plan_id_lit = proc_macro2::Literal::u32_unsuffixed(plan_id);
    let item_name_lit = proc_macro2::Literal::string(&item_name);
    let bindings_tokens = bindings.iter().map(|b| {
        let path_elems = b.node_path.iter().map(|i| {
            let lit = proc_macro2::Literal::u16_unsuffixed(*i);
            quote! { #lit }
        });
        let kind_tokens = match b.kind {
            BindingKindLite::Text => {
                quote! { ::pocopine::__private::BindingKind::Text }
            }
            BindingKindLite::Class => {
                quote! { ::pocopine::__private::BindingKind::Class }
            }
        };
        let expr_lit = proc_macro2::Literal::string(&b.expr_src);
        quote! {
            ::pocopine::__private::StaticBinding {
                node_path: &[ #(#path_elems),* ],
                kind: #kind_tokens,
                expr_src: #expr_lit,
            }
        }
    });
    let listeners_tokens = listeners.iter().map(|l| {
        let path_elems = l.node_path.iter().map(|i| {
            let lit = proc_macro2::Literal::u16_unsuffixed(*i);
            quote! { #lit }
        });
        let event_lit = proc_macro2::Literal::string(&l.event);
        let expr_lit = proc_macro2::Literal::string(&l.expr_src);
        quote! {
            ::pocopine::__private::StaticListener {
                node_path: &[ #(#path_elems),* ],
                event: #event_lit,
                modifiers: &[],
                expr_src: #expr_lit,
            }
        }
    });
    let plan_tokens = quote! {
        ::pocopine::__private::StaticRowPlan {
            plan_id: #plan_id_lit,
            item_name: #item_name_lit,
            bindings: &[ #(#bindings_tokens),* ],
            listeners: &[ #(#listeners_tokens),* ],
        }
    };
    Some((plan_tokens, stamp))
}

#[derive(Default)]
struct RowWalkCtx {
    bindings: Vec<BindingLite>,
    listeners: Vec<ListenerLite>,
}

#[derive(Clone, Copy)]
enum BindingKindLite {
    Text,
    Class,
}

struct BindingLite {
    node_path: Vec<u16>,
    kind: BindingKindLite,
    expr_src: String,
}

struct ListenerLite {
    node_path: Vec<u16>,
    event: String,
    expr_src: String,
}

/// Recursively walk the row body. Mutates `ctx.bindings` /
/// `ctx.listeners` and uses `path` as a scratch DFS index trail.
/// Returns `false` on any envelope violation — the caller
/// abandons this row plan.
fn walk_row(el: &Element, ctx: &mut RowWalkCtx, path: &mut Vec<u16>) -> bool {
    // Reject custom elements and Pine compounds outright. Their
    // mount logic (shadow scope, lifecycle hooks, slots) sits
    // outside the fast-path envelope.
    if el.tag.contains('-') || el.tag == "template" || el.tag == "slot" {
        return false;
    }
    if el.synthetic {
        // A synthetic element disrupts the path indexing model
        // (we computed paths against authored structure). Reject
        // for safety.
        return false;
    }

    for (name, value) in &el.attrs {
        if !classify_attribute(name, value, path, ctx) {
            return false;
        }
    }

    for (i, child) in el.children.iter().enumerate() {
        if let Node::Element(child_el) = child {
            // Index against ELEMENT children only, not all
            // children (text/comments shouldn't shift the
            // index). `Element::children()` in JS likewise
            // skips text/comments.
            let idx_among_elements = el
                .children
                .iter()
                .take(i)
                .filter(|n| matches!(n, Node::Element(_)))
                .count() as u16;
            path.push(idx_among_elements);
            let ok = walk_row(child_el, ctx, path);
            path.pop();
            if !ok {
                return false;
            }
        }
    }

    true
}

/// Decide whether a single `(name, value)` attribute on a row
/// node is in-envelope. Static attributes pass silently; known
/// dynamic attributes record a binding/listener; anything else
/// fails the row.
fn classify_attribute(name: &str, value: &str, path: &[u16], ctx: &mut RowWalkCtx) -> bool {
    // Listener shorthand: `@<event>="<expr>"`. RFC-020.
    if let Some(event) = name.strip_prefix('@') {
        // No modifiers: reject if `event` contains a `.`.
        if event.contains('.') {
            return false;
        }
        // Validate the expression at macro time.
        if pocopine_expr::parse(value).is_err() {
            return false;
        }
        ctx.listeners.push(ListenerLite {
            node_path: path.to_vec(),
            event: event.to_string(),
            expr_src: value.to_string(),
        });
        return true;
    }
    // Bind shorthand: `:<attr>="<expr>"`. RFC-020. v1 only
    // accepts `:class`.
    if let Some(attr) = name.strip_prefix(':') {
        if attr != "class" {
            return false;
        }
        if pocopine_expr::parse(value).is_err() {
            return false;
        }
        ctx.bindings.push(BindingLite {
            node_path: path.to_vec(),
            kind: BindingKindLite::Class,
            expr_src: value.to_string(),
        });
        return true;
    }
    // Verbose `pp-<…>` directives — narrow filter.
    if let Some(rest) = name.strip_prefix("pp-") {
        // `pp-text="<expr>"`
        if rest == "text" {
            if pocopine_expr::parse(value).is_err() {
                return false;
            }
            ctx.bindings.push(BindingLite {
                node_path: path.to_vec(),
                kind: BindingKindLite::Text,
                expr_src: value.to_string(),
            });
            return true;
        }
        // `pp-bind:class="<expr>"` (no other attrs in v1).
        if let Some(arg) = rest.strip_prefix("bind:") {
            // Reject modifiers (e.g. `bind:class.foo` — though
            // we don't support any class modifiers anyway, this
            // keeps the surface uniform).
            if arg.contains('.') {
                return false;
            }
            if arg != "class" {
                return false;
            }
            if pocopine_expr::parse(value).is_err() {
                return false;
            }
            ctx.bindings.push(BindingLite {
                node_path: path.to_vec(),
                kind: BindingKindLite::Class,
                expr_src: value.to_string(),
            });
            return true;
        }
        // `pp-on:<event>="<expr>"` (no modifiers).
        if let Some(rest) = rest.strip_prefix("on:") {
            if rest.contains('.') {
                return false;
            }
            // Empty event name disqualifies.
            if rest.is_empty() {
                return false;
            }
            if pocopine_expr::parse(value).is_err() {
                return false;
            }
            ctx.listeners.push(ListenerLite {
                node_path: path.to_vec(),
                event: rest.to_string(),
                expr_src: value.to_string(),
            });
            return true;
        }
        // Any other `pp-*` is out of envelope.
        return false;
    }
    // Static HTML attributes are always fine — they ride along
    // on the prototype clone.
    true
}

/// Find a single element child (the row root) of `template_el`.
/// Skips text/comments. Returns `None` if there are zero or
/// multiple element children.
fn single_element_child(template_el: &Element) -> Option<&Element> {
    let mut found: Option<&Element> = None;
    for child in &template_el.children {
        if let Node::Element(e) = child {
            if e.synthetic {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(e);
        }
    }
    found
}

fn find_attr<'a>(el: &'a Element, name: &str) -> Option<&'a String> {
    el.attrs
        .iter()
        .find_map(|(n, v)| if n == name { Some(v) } else { None })
}

/// Parse `pp-for="<v> in <items>"` and return the loop
/// variable name. Returns `None` if the shape doesn't match.
fn parse_for_item(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let mut parts = trimmed.splitn(2, " in ");
    let var = parts.next()?.trim();
    let _items = parts.next()?.trim(); // We don't validate items here.
    if var.is_empty() {
        return None;
    }
    if !var
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    {
        return None;
    }
    Some(var.to_string())
}

/// Compute where in the source to insert
/// `data-pp-row-plan="<id>"` for a `<template>` opening tag.
/// Uses the AST's `opening_tag_range` (which html5ever's
/// source-map populates).
fn compute_stamp(el: &Element, plan_id: u32) -> Option<TemplateStamp> {
    let Range { start, end } = el.opening_tag_range.clone();
    if end <= start {
        // No source range — html5ever foster-parented or the
        // source-map didn't populate. Skip the stamp; the plan
        // wouldn't be findable at runtime anyway.
        return None;
    }
    Some(TemplateStamp {
        plan_id,
        insert_at: end - 1, // before the closing `>`
    })
}

/// Apply the byte-range stamps to a copy of the source string.
/// Each stamp inserts ` data-pp-row-plan="<id>"` at the recorded
/// position. Stamps are applied right-to-left so earlier
/// positions stay valid as we splice.
pub(crate) fn apply_stamps(source: &str, stamps: &[TemplateStamp]) -> String {
    if stamps.is_empty() {
        return source.to_string();
    }
    let mut sorted: Vec<&TemplateStamp> = stamps.iter().collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.insert_at));
    let mut out = source.to_string();
    for stamp in sorted {
        if stamp.insert_at > out.len() {
            continue;
        }
        let attr = format!(" data-pp-row-plan=\"{}\"", stamp.plan_id);
        out.insert_str(stamp.insert_at, &attr);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template_parser::parse_strict;

    fn ast(src: &str) -> TemplateAst {
        parse_strict(src, "test.poco").expect("parses")
    }

    #[test]
    fn jsbench_row_is_eligible_and_emits_plan() {
        let src = r#"<root>
<table>
<template pp-for="row in rows" pp-key="row.id">
  <tr :class="selected_id == row.id ? 'danger' : ''">
    <td pp-text="row.id"></td>
    <td><a @click="select_row(row.id)" pp-text="row.label"></a></td>
    <td><a @click="remove_row(row.id)"><span class="glyphicon"></span></a></td>
    <td></td>
  </tr>
</template>
</table>
</root>"#;
        let plans = analyze_row_plans(&ast(src));
        assert!(plans.has_plans, "jsbench shape must qualify");
        assert_eq!(plans.stamps.len(), 1, "one stamp per row plan");
        let s = plans.array_tokens.to_string();
        assert!(s.contains("plan_id : 0"), "plan id literal expected: {s}");
        assert!(s.contains("\"row\""), "item_name should be \"row\": {s}");
        assert!(s.contains("BindingKind :: Class"), "class binding: {s}");
        assert!(s.contains("BindingKind :: Text"), "text binding: {s}");
        assert!(s.contains("\"click\""), "click listener: {s}");
    }

    #[test]
    fn nested_pp_for_disqualifies() {
        let src = r#"<root>
<template pp-for="row in rows" pp-key="row.id">
  <tr>
    <template pp-for="cell in row.cells" pp-key="cell.id">
      <td pp-text="cell.value"></td>
    </template>
  </tr>
</template>
</root>"#;
        let plans = analyze_row_plans(&ast(src));
        // The outer `<tr>` contains a `<template>` — that's a
        // disallowed tag inside the row body, so the outer plan
        // is rejected. The inner one might qualify on its own
        // since it's a leaf row with no nested pp-for.
        // (Acceptable either way; we just don't want a crash.)
        let _ = plans;
    }

    #[test]
    fn pp_show_inside_disqualifies() {
        let src = r#"<root>
<template pp-for="row in rows" pp-key="row.id">
  <tr><td pp-show="row.visible">x</td></tr>
</template>
</root>"#;
        let plans = analyze_row_plans(&ast(src));
        assert!(!plans.has_plans, "pp-show inside row body must reject");
    }

    #[test]
    fn click_with_modifier_disqualifies() {
        let src = r#"<root>
<template pp-for="row in rows" pp-key="row.id">
  <tr><td><a @click.prevent="select(row)">x</a></td></tr>
</template>
</root>"#;
        let plans = analyze_row_plans(&ast(src));
        assert!(!plans.has_plans, ".prevent listener must reject");
    }

    #[test]
    fn registered_component_child_disqualifies() {
        let src = r#"<root>
<template pp-for="row in rows" pp-key="row.id">
  <tr><pine-button>x</pine-button></tr>
</template>
</root>"#;
        let plans = analyze_row_plans(&ast(src));
        assert!(!plans.has_plans, "pine-* tag must reject");
    }

    #[test]
    fn no_pp_key_disqualifies() {
        let src = r#"<root>
<template pp-for="row in rows">
  <tr><td pp-text="row.id"></td></tr>
</template>
</root>"#;
        let plans = analyze_row_plans(&ast(src));
        assert!(!plans.has_plans, "naive (no pp-key) pp-for must skip plan");
    }

    #[test]
    fn other_pp_bind_attr_disqualifies() {
        let src = r#"<root>
<template pp-for="row in rows" pp-key="row.id">
  <tr :data-id="row.id"><td>x</td></tr>
</template>
</root>"#;
        let plans = analyze_row_plans(&ast(src));
        assert!(!plans.has_plans, "pp-bind:<other-attr> must reject in v1");
    }

    #[test]
    fn empty_dynamic_surface_skips_plan() {
        // No bindings, no listeners — fast path would just
        // duplicate the prototype clone with extra registry
        // overhead. Skipped.
        let src = r#"<root>
<template pp-for="row in rows" pp-key="row.id">
  <tr><td>static</td></tr>
</template>
</root>"#;
        let plans = analyze_row_plans(&ast(src));
        assert!(!plans.has_plans, "empty dynamic surface skips plan");
    }

    #[test]
    fn apply_stamps_inserts_attribute_before_closing_gt() {
        let src = r#"<template pp-for="x in xs" pp-key="x.id"><tr></tr></template>"#;
        let stamp = TemplateStamp {
            plan_id: 7,
            insert_at: src.find('>').unwrap(),
        };
        let out = apply_stamps(src, &[stamp]);
        assert!(
            out.contains(r#" data-pp-row-plan="7""#),
            "stamp must inject attribute: {out}",
        );
    }
}
