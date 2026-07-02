//! Compile-time validation of template expression roots (RFC-111).
//!
//! Harvests every expression the template can evaluate — plan-
//! eligible AND walker-fallback alike — and emits one const-eval
//! check per root identifier:
//!
//! ```ignore
//! const _: () = {
//!     if !(::pocopine::__private::template_key_listed(<Counter>::__POC_TEMPLATE_FIELDS, "countt")
//!         || ::pocopine::__private::template_key_listed(<Counter>::__POC_COMPUTED_KEYS, "countt"))
//!     {
//!         ::core::panic!("unknown template path root `countt` … (from `pp-text=\"countt\"` …)");
//!     }
//! };
//! ```
//!
//! The name lists are consts: `__POC_TEMPLATE_FIELDS` from
//! `#[component]` (struct fields + explicit-list flatten leaves),
//! `__POC_COMPUTED_KEYS` / `__POC_HANDLER_KEYS` from `#[handlers]`.
//! Rustc's const evaluation performs the cross-macro join — and
//! because the panic message is a literal this macro formats at
//! expansion time, the error carries the offending expression, the
//! directive, the template name, and a nearest-field suggestion,
//! anchored on the `template = "…"` argument's span:
//!
//! ```text
//! error[E0080]: … 'unknown template path root `countt`: not a field or
//! #[computed] value of `Counter` — from `pp-text="countt"` in
//! Counter.poco; nearest field: `count`'
//! ```
//!
//! What is deliberately NOT validated:
//!
//! * `$`-rooted names (`$store`, `$route`, `$event`, loop magics)
//!   — not locally checkable; the runtime warn path owns them.
//! * Locally-bound names — `pp-for` items, `pp-let` slot idents
//!   (including `pp-case pp-let` binds) — whitelisted by a scope
//!   stack threaded through the walk.
//! * Nested segments (`user.name` checks `user` only) — the
//!   macro cannot see into field types.
//! * Components with a bare `#[prop(flatten)]` field — its leaf
//!   names resolve at runtime through the `Props` trait, so any
//!   unknown root might be a flatten leaf. Bindable checks are
//!   skipped for those components (handler checks remain).
//! * Anything under `unchecked_paths` (`#[component(...,
//!   unchecked_paths = "true")]`), the escape hatch.

use std::collections::BTreeMap;

use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};

use crate::template_parser::{Element, Node, TemplateAst};

/// Which name list a harvested root resolves against.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum RootKind {
    /// Readable/writable scope key: struct field, explicit
    /// flatten leaf, or `#[computed]` field.
    Bindable,
    /// `pp-on` dispatch target: a `#[handlers]` method.
    Handler,
}

/// Harvested roots: `(kind, root) → first source context` (the
/// attribute or interpolation the root came from, for the error
/// message).
type Roots = BTreeMap<(RootKind, String), String>;

/// Harvest the template and emit the const-eval checks.
/// `skip_bindable` disables field/computed checks (bare-flatten
/// components) while keeping handler checks; `own_fields` feeds
/// the nearest-field suggestion; `span` anchors the errors
/// (the `template` / `template_inline` argument's literal).
pub fn emit_path_assertions(
    ast: &TemplateAst,
    struct_ident: &syn::Ident,
    skip_bindable: bool,
    template_display: &str,
    own_fields: &[String],
    span: Span,
) -> TokenStream {
    let mut roots: Roots = BTreeMap::new();
    let mut scope: Vec<String> = Vec::new();
    for node in &ast.roots {
        harvest_node(node, &mut scope, &mut roots);
    }

    let struct_name = struct_ident.to_string();
    let checks = roots.iter().filter_map(|((kind, root), ctx)| match kind {
        RootKind::Bindable => {
            if skip_bindable {
                return None;
            }
            let mut msg = format!(
                "unknown template path root `{root}`: `{struct_name}` has no field or \
                 #[computed] value with that name\n \
                 --> from `{ctx}` in {template_display}"
            );
            if let Some(near) = nearest(own_fields, root) {
                msg.push_str(&format!(
                    "\nhelp: a field with a similar name exists: `{near}`"
                ));
            } else if !own_fields.is_empty() {
                msg.push_str(&format!(
                    "\nhelp: available fields are: {}",
                    field_listing(own_fields)
                ));
            }
            Some(quote_spanned! {span=>
                const _: () = {
                    if !(::pocopine::__private::template_key_listed(
                        <#struct_ident>::__POC_TEMPLATE_FIELDS,
                        #root,
                    ) || ::pocopine::__private::template_key_listed(
                        <#struct_ident>::__POC_COMPUTED_KEYS,
                        #root,
                    )) {
                        ::core::panic!(#msg);
                    }
                };
            })
        }
        RootKind::Handler => {
            let msg = format!(
                "unknown template handler `{root}`: no #[handlers] method of \
                 `{struct_name}` has that name\n \
                 --> from `{ctx}` in {template_display}"
            );
            Some(quote_spanned! {span=>
                const _: () = {
                    if !::pocopine::__private::template_key_listed(
                        <#struct_ident>::__POC_HANDLER_KEYS,
                        #root,
                    ) {
                        ::core::panic!(#msg);
                    }
                };
            })
        }
    });
    quote! { #(#checks)* }
}

/// `__POC_TEMPLATE_FIELDS` const for the component macro: the
/// bindable names the struct itself declares (fields + explicit
/// flatten leaves).
pub fn field_keys_const(names: impl Iterator<Item = String>) -> TokenStream {
    let names: Vec<String> = names.collect();
    quote! {
        #[doc(hidden)]
        #[allow(dead_code)]
        pub const __POC_TEMPLATE_FIELDS: &'static [&'static str] = &[#(#names),*];
    }
}

/// `__POC_COMPUTED_KEYS` + `__POC_HANDLER_KEYS` consts for
/// `#[handlers]` — the two lists the component macro cannot see.
pub fn handlers_keys_consts(
    computed: impl Iterator<Item = String>,
    handlers: impl Iterator<Item = String>,
) -> TokenStream {
    let computed: Vec<String> = computed.collect();
    let handlers: Vec<String> = handlers.collect();
    quote! {
        #[doc(hidden)]
        #[allow(dead_code)]
        pub const __POC_COMPUTED_KEYS: &'static [&'static str] = &[#(#computed),*];
        #[doc(hidden)]
        #[allow(dead_code)]
        pub const __POC_HANDLER_KEYS: &'static [&'static str] = &[#(#handlers),*];
    }
}

fn harvest_node(node: &Node, scope: &mut Vec<String>, out: &mut Roots) {
    match node {
        Node::Element(el) => harvest_element(el, scope, out),
        Node::Text(text, _) => harvest_interps(text, scope, out),
        Node::Comment(..) => {}
    }
}

fn harvest_element(el: &Element, scope: &mut Vec<String>, out: &mut Roots) {
    // Locals this element introduces for its subtree: the pp-for
    // item and any pp-let ident (slot content, pp-case binds).
    let mut introduced = 0usize;
    for (name, value) in &el.attrs {
        if name == "pp-for" {
            // `item in items` — items evaluates in the OUTER
            // scope; harvest it before the item name binds.
            if let Some((item, items_expr)) = parse_pp_for(value) {
                let ctx = format!("pp-for=\"{value}\"");
                harvest_expr_src(&items_expr, &ctx, scope, out, false);
                scope.push(item);
                introduced += 1;
            }
        } else if name == "pp-let" {
            let ident = value.trim();
            if !ident.is_empty() {
                scope.push(ident.to_string());
                introduced += 1;
            }
        }
    }

    for (name, value) in &el.attrs {
        if let Some(kind) = attr_expr_kind(name) {
            let ctx = format!("{name}=\"{value}\"");
            harvest_expr_src(value, &ctx, scope, out, kind == AttrKind::Listener);
        }
    }
    for child in &el.children {
        harvest_node(child, scope, out);
    }
    scope.truncate(scope.len() - introduced);
}

#[derive(PartialEq)]
enum AttrKind {
    Expr,
    Listener,
}

/// Does this attribute's value evaluate as a template expression
/// in the component's scope — and in which context? Everything
/// unrecognised is skipped: under-validation is safe,
/// over-validation breaks builds (`pp-ref` names, teleport
/// selectors, transition presets, `pp-route` paths, plain HTML
/// attributes are all non-expressions).
fn attr_expr_kind(name: &str) -> Option<AttrKind> {
    if let Some(rest) = name
        .strip_prefix("pp-on:")
        .or_else(|| name.strip_prefix('@'))
    {
        if rest.is_empty() {
            return None;
        }
        return Some(AttrKind::Listener);
    }
    if name.strip_prefix("pp-bind:").is_some_and(|r| !r.is_empty())
        || (name.starts_with(':') && name.len() > 1)
    {
        return Some(AttrKind::Expr);
    }
    // `pp-model`, `pp-model:field`, `pp-model.number`, …
    if name == "pp-model"
        || name.strip_prefix("pp-model:").is_some()
        || name.strip_prefix("pp-model.").is_some()
    {
        return Some(AttrKind::Expr);
    }
    matches!(
        name,
        "pp-text" | "pp-html" | "pp-show" | "pp-if" | "pp-else-if" | "pp-match"
    )
    .then_some(AttrKind::Expr)
}

/// Mirror of `template_plan::parse_pp_for` (kept local so this
/// module stays a pure function of the AST).
fn parse_pp_for(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let (lhs, rhs) = s.split_once(" in ")?;
    let ident = lhs.trim();
    let items = rhs.trim();
    if ident.is_empty() || items.is_empty() {
        return None;
    }
    if !ident.chars().all(|c| c.is_alphanumeric() || c == '_')
        || ident.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((ident.to_string(), items.to_string()))
}

/// Harvest `{{ expr }}` interpolation segments from a text node.
/// Escapes (`\{{`, `\}}`) hide the braces from interp — mirror
/// that by skipping the escaped pair.
fn harvest_interps(text: &str, scope: &[String], out: &mut Roots) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 2 < bytes.len() {
            let (n1, n2) = (bytes[i + 1], bytes[i + 2]);
            if (n1 == b'{' && n2 == b'{') || (n1 == b'}' && n2 == b'}') {
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i + 2;
            let Some(rel) = text[start..].find("}}") else {
                return; // unclosed — the strict validator owns the error
            };
            let src = &text[start..start + rel];
            let ctx = format!("{{{{ {} }}}}", src.trim());
            harvest_expr_src(src, &ctx, scope, out, false);
            i = start + rel + 2;
            continue;
        }
        i += 1;
    }
}

/// Parse one expression source and collect its root identifiers.
/// Unparseable sources are skipped — `emit_compiled_expr_option`
/// already turns plan-eligible parse errors into `compile_error!`,
/// and preserved attributes are the runtime's jurisdiction.
fn harvest_expr_src(src: &str, ctx: &str, scope: &[String], out: &mut Roots, listener: bool) {
    let Ok(ast) = pocopine_expr::parse(src) else {
        return;
    };
    // Listener backfill (RFC-024): a bare single identifier is a
    // handler reference (`@click="reset"` → `reset($event)`).
    if listener
        && let pocopine_expr::Expr::Path(segs) = &ast.value
        && segs.len() == 1
    {
        push_root(RootKind::Handler, &segs[0], ctx, scope, out);
        return;
    }
    harvest_expr(&ast.value, ctx, scope, out);
}

fn harvest_expr(expr: &pocopine_expr::Expr, ctx: &str, scope: &[String], out: &mut Roots) {
    use pocopine_expr::Expr;
    match expr {
        Expr::Literal(_) => {}
        Expr::Path(segs) => {
            if let Some(root) = segs.first() {
                push_root(RootKind::Bindable, root, ctx, scope, out);
            }
        }
        Expr::Not(inner) => harvest_expr(&inner.value, ctx, scope, out),
        Expr::BinOp(_, l, r) => {
            harvest_expr(&l.value, ctx, scope, out);
            harvest_expr(&r.value, ctx, scope, out);
        }
        Expr::Ternary(c, a, b) => {
            harvest_expr(&c.value, ctx, scope, out);
            harvest_expr(&a.value, ctx, scope, out);
            harvest_expr(&b.value, ctx, scope, out);
        }
        Expr::Call(name, args) => {
            push_root(RootKind::Handler, name, ctx, scope, out);
            for arg in args {
                harvest_expr(&arg.value, ctx, scope, out);
            }
        }
        Expr::Assign(path, rhs) => {
            if let Some(root) = path.first() {
                push_root(RootKind::Bindable, root, ctx, scope, out);
            }
            harvest_expr(&rhs.value, ctx, scope, out);
        }
        Expr::Seq(stmts) => {
            for s in stmts {
                harvest_expr(&s.value, ctx, scope, out);
            }
        }
    }
}

fn push_root(kind: RootKind, root: &str, ctx: &str, scope: &[String], out: &mut Roots) {
    if root.contains('$') {
        return; // $store/$route/$event/$index/… — runtime territory
    }
    if scope.iter().any(|s| s == root) {
        return; // pp-for item / pp-let ident
    }
    if !is_ident_safe(root) {
        return;
    }
    out.entry((kind, root.to_string()))
        .or_insert_with(|| ctx.to_string());
}

/// Conservative "plain identifier" check.
fn is_ident_safe(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Rustc-style truncated name listing for the no-similar-name
/// help line: `` `a`, `b`, `c` and 4 others ``.
fn field_listing(names: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut out = names
        .iter()
        .take(SHOWN)
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        out.push_str(&format!(" and {} others", names.len() - SHOWN));
    }
    out
}

/// Nearest own-field name within edit distance 2 — the macro-time
/// did-you-mean for the panic message.
fn nearest<'a>(candidates: &'a [String], root: &str) -> Option<&'a str> {
    candidates
        .iter()
        .filter_map(|c| {
            let d = edit_distance(c, root);
            (d > 0 && d <= 2).then_some((d, c.as_str()))
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::format_ident;

    fn assertions(template: &str) -> String {
        let ast = pocopine_template_parser::parse_strict(template, "test.poco").expect("parses");
        let ident = format_ident!("Demo");
        emit_path_assertions(
            &ast,
            &ident,
            false,
            "Demo.poco",
            &["count".to_string(), "name".to_string()],
            Span::call_site(),
        )
        .to_string()
    }

    #[test]
    fn harvests_field_and_handler_roots() {
        let out = assertions(
            r#"<div><span pp-text="count"></span>
            <button pp-on:click="reset">x</button>
            <input pp-model="name" /></div>"#,
        );
        assert!(out.contains("__POC_TEMPLATE_FIELDS , \"count\""), "{out}");
        assert!(out.contains("__POC_TEMPLATE_FIELDS , \"name\""), "{out}");
        assert!(out.contains("__POC_HANDLER_KEYS , \"reset\""), "{out}");
        assert!(out.contains("unknown template handler `reset`"), "{out}");
    }

    #[test]
    fn message_carries_context_and_suggestion() {
        let out = assertions(r#"<span pp-text="countt"></span>"#);
        assert!(out.contains("unknown template path root `countt`"), "{out}");
        assert!(out.contains("pp-text=\\\"countt\\\""), "{out}");
        assert!(out.contains("in Demo.poco"), "{out}");
        assert!(
            out.contains("help: a field with a similar name exists: `count`"),
            "{out}"
        );
    }

    #[test]
    fn no_similar_name_lists_available_fields() {
        let out = assertions(r#"<span pp-text="zzzzzz"></span>"#);
        assert!(
            out.contains("help: available fields are: `count`, `name`"),
            "{out}"
        );
    }

    #[test]
    fn field_listing_truncates() {
        let names: Vec<String> = (0..8).map(|i| format!("f{i}")).collect();
        assert_eq!(
            field_listing(&names),
            "`f0`, `f1`, `f2`, `f3`, `f4` and 3 others"
        );
        assert_eq!(field_listing(&names[..2]), "`f0`, `f1`");
    }

    #[test]
    fn skips_magic_and_loop_locals() {
        let out = assertions(
            r#"<div>
            <span pp-text="$store.prefs.theme"></span>
            <template pp-for="item in items">
                <span pp-text="item.label"></span>
                <span pp-text="$index"></span>
            </template>
            <span pp-text="items"></span></div>"#,
        );
        assert!(!out.contains("\"item\""), "{out}");
        assert!(!out.contains("$"), "{out}");
        assert!(out.contains("\"items\""), "{out}");
    }

    #[test]
    fn loop_scope_pops_after_body() {
        let out = assertions(
            r#"<div><template pp-for="row in rows"><b pp-text="row.x"></b></template>
            <i pp-text="row"></i></div>"#,
        );
        // `row` outside the loop body IS a component-scope read.
        assert!(out.contains("\"row\""), "{out}");
        assert!(out.contains("\"rows\""), "{out}");
    }

    #[test]
    fn pp_let_binds_slot_scope() {
        let out = assertions(
            r#"<pine-list><template pp-let="entry">
                <span pp-text="entry.title"></span>
            </template></pine-list>"#,
        );
        assert!(!out.contains("\"entry\""), "{out}");
    }

    #[test]
    fn listener_bare_path_is_handler_and_compound_reads_are_fields() {
        let out = assertions(r#"<button pp-on:click="open = !open">t</button>"#);
        assert!(out.contains("__POC_TEMPLATE_FIELDS , \"open\""), "{out}");
        let out = assertions(r#"<button @click="pick(item, count)">t</button>"#);
        assert!(out.contains("__POC_HANDLER_KEYS , \"pick\""), "{out}");
        assert!(out.contains("__POC_TEMPLATE_FIELDS , \"count\""), "{out}");
    }

    #[test]
    fn interps_and_bind_shorthand_harvest() {
        let out = assertions(r#"<div :class="theme"><p>hello {{ user.name }}!</p></div>"#);
        assert!(out.contains("\"theme\""), "{out}");
        assert!(out.contains("\"user\""), "{out}");
        assert!(out.contains("{{ user.name }}"), "{out}");
    }

    #[test]
    fn skip_bindable_keeps_handlers() {
        let ast = pocopine_template_parser::parse_strict(
            r#"<button pp-on:click="save" pp-text="label"></button>"#,
            "test.poco",
        )
        .expect("parses");
        let ident = format_ident!("Demo");
        let out = emit_path_assertions(&ast, &ident, true, "Demo.poco", &[], Span::call_site())
            .to_string();
        assert!(!out.contains("\"label\""), "{out}");
        assert!(out.contains("__POC_HANDLER_KEYS , \"save\""), "{out}");
    }

    #[test]
    fn edit_distance_suggestion_bounds() {
        assert_eq!(
            nearest(&["count".into(), "label".into()], "countt"),
            Some("count")
        );
        assert_eq!(nearest(&["count".into()], "zzz"), None);
        // Exact match is not a "suggestion" (distance 0 filtered).
        assert_eq!(nearest(&["count".into()], "count"), None);
    }
}
