//! Compile-time validation of template expression roots.
//!
//! Harvests every expression the template can evaluate — plan-
//! eligible AND walker-fallback alike — and emits one anonymous
//! marker reference per root identifier:
//!
//! ```ignore
//! const _: fn() = <Counter>::__poc_bindable_count;   // field / #[computed]
//! const _: fn() = <Counter>::__poc_handler_reset;    // pp-on handler
//! ```
//!
//! The markers themselves are emitted by `#[component]` (struct
//! fields + explicit-list flatten leaves) and `#[handlers]`
//! (`#[computed]` fields + dispatchable methods), so rustc's
//! ordinary item resolution performs the cross-macro join: a
//! typo'd or renamed root becomes "no function or associated item
//! named `__poc_bindable_countt`" — with the compiler's own
//! did-you-mean pointing at the fix — instead of a silent
//! runtime `undefined`.
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

use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::template_parser::{Element, Node, TemplateAst};

/// Which marker family a harvested root resolves against.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum RootKind {
    /// Readable/writable scope key: struct field, explicit
    /// flatten leaf, or `#[computed]` field.
    Bindable,
    /// `pp-on` dispatch target: a `#[handlers]` method.
    Handler,
}

/// Harvest the template and emit the marker-reference items.
/// `skip_bindable` disables field/computed checks (bare-flatten
/// components) while keeping handler checks.
pub fn emit_path_assertions(
    ast: &TemplateAst,
    struct_ident: &syn::Ident,
    skip_bindable: bool,
) -> TokenStream {
    let mut roots: BTreeSet<(RootKind, String)> = BTreeSet::new();
    let mut scope: Vec<String> = Vec::new();
    for node in &ast.roots {
        harvest_node(node, &mut scope, &mut roots);
    }

    let refs = roots.iter().filter_map(|(kind, root)| {
        if *kind == RootKind::Bindable && skip_bindable {
            return None;
        }
        let marker = match kind {
            RootKind::Bindable => format_ident!("__poc_bindable_{root}"),
            RootKind::Handler => format_ident!("__poc_handler_{root}"),
        };
        Some(quote! { const _: fn() = <#struct_ident>::#marker; })
    });
    quote! { #(#refs)* }
}

/// Marker items for the component macro: one hidden fn per
/// bindable name the struct itself declares (fields + explicit
/// flatten leaves). `#[handlers]` emits the computed/handler
/// side via [`handler_marker_items`]-shaped tokens of its own.
pub fn bindable_marker_items(names: impl Iterator<Item = String>) -> TokenStream {
    let fns = names.filter(|n| is_ident_safe(n)).map(|n| {
        let marker = format_ident!("__poc_bindable_{n}");
        quote! {
            #[doc(hidden)]
            #[allow(non_snake_case, dead_code)]
            pub fn #marker() {}
        }
    });
    quote! { #(#fns)* }
}

/// Marker items for `#[handlers]`: dispatchable method names.
pub fn handler_marker_items(names: impl Iterator<Item = String>) -> TokenStream {
    let fns = names.filter(|n| is_ident_safe(n)).map(|n| {
        let marker = format_ident!("__poc_handler_{n}");
        quote! {
            #[doc(hidden)]
            #[allow(non_snake_case, dead_code)]
            pub fn #marker() {}
        }
    });
    quote! { #(#fns)* }
}

fn harvest_node(node: &Node, scope: &mut Vec<String>, out: &mut BTreeSet<(RootKind, String)>) {
    match node {
        Node::Element(el) => harvest_element(el, scope, out),
        Node::Text(text, _) => harvest_interps(text, scope, out),
        Node::Comment(..) => {}
    }
}

fn harvest_element(el: &Element, scope: &mut Vec<String>, out: &mut BTreeSet<(RootKind, String)>) {
    // Locals this element introduces for its subtree: the pp-for
    // item and any pp-let ident (slot content, pp-case binds).
    let mut introduced = 0usize;
    for (name, value) in &el.attrs {
        if name == "pp-for" {
            // `item in items` — items evaluates in the OUTER
            // scope; harvest it before the item name binds.
            if let Some((item, items_expr)) = parse_pp_for(value) {
                harvest_expr_src(&items_expr, scope, out, false);
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
            harvest_expr_src(value, scope, out, kind == AttrKind::Listener);
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
fn harvest_interps(text: &str, scope: &[String], out: &mut BTreeSet<(RootKind, String)>) {
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
            harvest_expr_src(&text[start..start + rel], scope, out, false);
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
fn harvest_expr_src(
    src: &str,
    scope: &[String],
    out: &mut BTreeSet<(RootKind, String)>,
    listener: bool,
) {
    let Ok(ast) = pocopine_expr::parse(src) else {
        return;
    };
    // Listener backfill (RFC-024): a bare single identifier is a
    // handler reference (`@click="reset"` → `reset($event)`).
    if listener
        && let pocopine_expr::Expr::Path(segs) = &ast.value
        && segs.len() == 1
    {
        push_root(RootKind::Handler, &segs[0], scope, out);
        return;
    }
    harvest_expr(&ast.value, scope, out);
}

fn harvest_expr(
    expr: &pocopine_expr::Expr,
    scope: &[String],
    out: &mut BTreeSet<(RootKind, String)>,
) {
    use pocopine_expr::Expr;
    match expr {
        Expr::Literal(_) => {}
        Expr::Path(segs) => {
            if let Some(root) = segs.first() {
                push_root(RootKind::Bindable, root, scope, out);
            }
        }
        Expr::Not(inner) => harvest_expr(&inner.value, scope, out),
        Expr::BinOp(_, l, r) => {
            harvest_expr(&l.value, scope, out);
            harvest_expr(&r.value, scope, out);
        }
        Expr::Ternary(c, a, b) => {
            harvest_expr(&c.value, scope, out);
            harvest_expr(&a.value, scope, out);
            harvest_expr(&b.value, scope, out);
        }
        Expr::Call(name, args) => {
            push_root(RootKind::Handler, name, scope, out);
            for arg in args {
                harvest_expr(&arg.value, scope, out);
            }
        }
        Expr::Assign(path, rhs) => {
            if let Some(root) = path.first() {
                push_root(RootKind::Bindable, root, scope, out);
            }
            harvest_expr(&rhs.value, scope, out);
        }
        Expr::Seq(stmts) => {
            for s in stmts {
                harvest_expr(&s.value, scope, out);
            }
        }
    }
}

fn push_root(kind: RootKind, root: &str, scope: &[String], out: &mut BTreeSet<(RootKind, String)>) {
    if root.contains('$') {
        return; // $store/$route/$event/$index/… — runtime territory
    }
    if scope.iter().any(|s| s == root) {
        return; // pp-for item / pp-let ident
    }
    if !is_ident_safe(root) {
        return;
    }
    out.insert((kind, root.to_string()));
}

/// Conservative "can become a Rust ident suffix" check.
fn is_ident_safe(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::format_ident;

    fn assertions(template: &str) -> String {
        let ast = pocopine_template_parser::parse_strict(template, "test.poco").expect("parses");
        let ident = format_ident!("Demo");
        emit_path_assertions(&ast, &ident, false).to_string()
    }

    #[test]
    fn harvests_field_and_handler_roots() {
        let out = assertions(
            r#"<div><span pp-text="count"></span>
            <button pp-on:click="reset">x</button>
            <input pp-model="name" /></div>"#,
        );
        assert!(out.contains("__poc_bindable_count"), "{out}");
        assert!(out.contains("__poc_bindable_name"), "{out}");
        assert!(out.contains("__poc_handler_reset"), "{out}");
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
        assert!(!out.contains("__poc_bindable_item ;"), "{out}");
        assert!(!out.contains("$"), "{out}");
        assert!(out.contains("__poc_bindable_items"), "{out}");
    }

    #[test]
    fn loop_scope_pops_after_body() {
        let out = assertions(
            r#"<div><template pp-for="row in rows"><b pp-text="row.x"></b></template>
            <i pp-text="row"></i></div>"#,
        );
        // `row` outside the loop body IS a component-scope read.
        assert!(out.contains("__poc_bindable_row"), "{out}");
        assert!(out.contains("__poc_bindable_rows"), "{out}");
    }

    #[test]
    fn pp_let_binds_slot_scope() {
        let out = assertions(
            r#"<pine-list><template pp-let="entry">
                <span pp-text="entry.title"></span>
            </template></pine-list>"#,
        );
        assert!(!out.contains("__poc_bindable_entry"), "{out}");
    }

    #[test]
    fn listener_bare_path_is_handler_and_compound_reads_are_fields() {
        let out = assertions(r#"<button pp-on:click="open = !open">t</button>"#);
        assert!(out.contains("__poc_bindable_open"), "{out}");
        let out = assertions(r#"<button @click="pick(item, count)">t</button>"#);
        assert!(out.contains("__poc_handler_pick"), "{out}");
        assert!(out.contains("__poc_bindable_count"), "{out}");
    }

    #[test]
    fn interps_and_bind_shorthand_harvest() {
        let out = assertions(r#"<div :class="theme"><p>hello {{ user.name }}!</p></div>"#);
        assert!(out.contains("__poc_bindable_theme"), "{out}");
        assert!(out.contains("__poc_bindable_user"), "{out}");
    }

    #[test]
    fn skip_bindable_keeps_handlers() {
        let ast = pocopine_template_parser::parse_strict(
            r#"<button pp-on:click="save" pp-text="label"></button>"#,
            "test.poco",
        )
        .expect("parses");
        let ident = format_ident!("Demo");
        let out = emit_path_assertions(&ast, &ident, true).to_string();
        assert!(!out.contains("__poc_bindable_label"), "{out}");
        assert!(out.contains("__poc_handler_save"), "{out}");
    }
}
