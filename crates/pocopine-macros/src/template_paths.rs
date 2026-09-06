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
//! expansion time, it can be a full annotate-snippets render of
//! the `.poco` source (the strict validator's house style, via
//! `Renderer::plain()` so const-panic output stays ANSI-free),
//! with an strsim-suggested similar name in the `help:` footer:
//!
//! ```text
//! error[E0080]: evaluation panicked:
//! error: unknown template path root `countt`
//!  --> Counter.poco:5:22
//!   |
//! 5 |   <span pp-text="countt"></span>
//!   |                  ^^^^^^ `Counter` has no field or #[computed] value with that name
//!   |
//!   = help: a field with a similar name exists: `count`
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
use std::ops::Range;

use annotate_snippets::{Level, Renderer, Snippet};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned};

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

/// The first source context a root was harvested from — carries
/// both the display form for fallback messages and the raw text
/// to locate the root in the template source for the snippet.
struct RootCtx {
    display: String,
    needle: Needle,
}

enum Needle {
    /// An attribute value — located as `"…value…"` in source.
    Attr(String),
    /// A raw `{{ … }}` interpolation body — located verbatim.
    Interp(String),
}

/// Harvested roots: `(kind, root) → first source context`.
type Roots = BTreeMap<(RootKind, String), RootCtx>;

/// Harvest the template and emit the const-eval checks.
/// `skip_bindable` disables field/computed checks (bare-flatten
/// components) while keeping handler checks; `own_fields` feeds
/// the similar-name suggestion; `span` anchors the errors
/// (the `template` argument’s literal, or the `poco!` body).
pub fn emit_path_assertions(
    ast: &TemplateAst,
    struct_ident: &syn::Ident,
    skip_bindable: bool,
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
            let title = format!("unknown template path root `{root}`");
            let label = format!("`{struct_name}` has no field or #[computed] value with that name");
            let help = if let Some(near) = nearest(own_fields, root) {
                Some(format!("a field with a similar name exists: `{near}`"))
            } else if !own_fields.is_empty() {
                Some(format!(
                    "available fields are: {}",
                    field_listing(own_fields)
                ))
            } else {
                None
            };
            let msg = render_check_message(ast, ctx, root, &title, &label, help.as_deref());
            Some(quote_spanned! {span=>
                const _: () = {
                    if !(::pocopine::__private::template_key_listed(
                        <#struct_ident>::__POC_TEMPLATE_FIELDS,
                        #root,
                    ) || ::pocopine::__private::template_key_listed(
                        <#struct_ident>::__POC_COMPUTED_KEYS,
                        #root,
                    )) {
                        // `"{}"` form: the message embeds template
                        // source, whose braces must not be read as
                        // format args (const panic allows exactly
                        // this one-`&str`-arg shape).
                        ::core::panic!("{}", #msg);
                    }
                };
            })
        }
        RootKind::Handler => {
            let title = format!("unknown template handler `{root}`");
            let label = format!("no #[handlers] method of `{struct_name}` has that name");
            let msg = render_check_message(ast, ctx, root, &title, &label, None);
            Some(quote_spanned! {span=>
                const _: () = {
                    if !::pocopine::__private::template_key_listed(
                        <#struct_ident>::__POC_HANDLER_KEYS,
                        #root,
                    ) {
                        // `"{}"` form: the message embeds template
                        // source, whose braces must not be read as
                        // format args (const panic allows exactly
                        // this one-`&str`-arg shape).
                        ::core::panic!("{}", #msg);
                    }
                };
            })
        }
    });
    quote! { #(#checks)* }
}

/// Predictable marker emitted by `#[handlers]` for a computed value's return
/// type. The component macro references the same marker when a bare computed
/// name is bound to `<pp-component :is>`.
pub(crate) fn computed_type_marker_ident(
    struct_ident: &syn::Ident,
    computed_name: &str,
) -> syn::Ident {
    format_ident!(
        "__PocComputedType_{}_{}",
        struct_ident,
        computed_name,
        span = struct_ident.span(),
    )
}

/// Enforce the typed dynamic-component selection lane wherever the component
/// macro can see the Rust type directly.
///
/// Bare local fields are asserted through field access. Bare computed names
/// resolve through the type witness emitted by `#[handlers]`. Dotted paths
/// (notably `$store.foo.selection` and loop-item fields) cross a dynamic scope
/// boundary the proc macro cannot type-resolve, so the runtime's strict
/// `ComponentRef<Host>` wire-shape check remains their backstop.
pub fn emit_dynamic_component_selection_assertions(
    ast: &TemplateAst,
    struct_ident: &syn::Ident,
    field_idents: &[syn::Ident],
    span: Span,
) -> TokenStream {
    let mut expressions = std::collections::BTreeSet::new();
    for node in &ast.roots {
        collect_dynamic_component_selections(node, &mut expressions);
    }

    let checks = expressions.into_iter().filter_map(|source| {
        let parsed = pocopine_expr::parse(&source).ok()?;
        let pocopine_expr::Expr::Path(segments) = parsed.value else {
            let message = "`<pp-component :is>` must be a path to a typed \
                           `ComponentRef<Host>` or `Option<ComponentRef<Host>>`; \
                           construct the selection with \
                           `ComponentRef::of::<Child>()` in a `ComponentRef<Host>` \
                           context (or host-scoped \
                           `from_registered_name` at an external \
                           boundary) and move conditional logic into a typed Rust field or \
                           `#[computed]` value";
            return Some(quote_spanned! {span=>
                ::core::compile_error!(#message);
            });
        };

        if segments.len() != 1 || segments[0].starts_with('$') {
            return None;
        }
        let root = &segments[0];
        if let Some(field_ident) = field_idents
            .iter()
            .find(|ident| ident.to_string().trim_start_matches("r#") == root.as_str())
        {
            return Some(quote_spanned! {span=>
                const _: fn(&#struct_ident) = |__pocopine_state| {
                    ::pocopine::__private::assert_dynamic_component_selection::<
                        #struct_ident,
                        _,
                    >(
                        &__pocopine_state.#field_ident,
                    );
                };
            });
        }

        let marker = computed_type_marker_ident(struct_ident, root);
        Some(quote_spanned! {span=>
            const _: fn() = || {
                ::pocopine::__private::assert_dynamic_component_selection_type::<
                    #struct_ident,
                    <#marker as ::pocopine::__private::ComputedTypeWitness>::Output,
                >();
            };
        })
    });

    quote! { #(#checks)* }
}

fn collect_dynamic_component_selections(
    node: &Node,
    expressions: &mut std::collections::BTreeSet<String>,
) {
    let Node::Element(el) = node else {
        return;
    };
    if el.tag == "pp-component" {
        for (name, value) in &el.attrs {
            if matches!(name.as_str(), ":is" | "pp-bind:is") {
                expressions.insert(value.trim().to_string());
            }
        }
    }
    for child in &el.children {
        collect_dynamic_component_selections(child, expressions);
    }
}

/// Pre-render the panic message at expansion time. When the root
/// can be located in the template source, render a full
/// annotate-snippets block (source excerpt + caret + `help:`
/// footer — the same shape the strict template validator emits,
/// but through `Renderer::plain()` so const-panic output and
/// trybuild snapshots stay ANSI-free). Falls back to a compact
/// text form when the location search misses. The leading
/// newline detaches the block from rustc's
/// "evaluation panicked:" prefix.
fn render_check_message(
    ast: &TemplateAst,
    ctx: &RootCtx,
    root: &str,
    title: &str,
    label: &str,
    help: Option<&str>,
) -> String {
    if let Some(range) = locate_root(&ast.source, ctx, root) {
        let mut message = Level::Error.title(title).snippet(
            Snippet::source(&ast.source)
                .origin(&ast.file_path)
                .fold(true)
                .annotation(Level::Error.span(range).label(label)),
        );
        if let Some(h) = help {
            message = message.footer(Level::Help.title(h));
        }
        return format!("\n{}", Renderer::plain().render(message));
    }
    let mut msg = format!(
        "{title}: {label}\n --> from `{}` in {}",
        ctx.display, ast.file_path
    );
    if let Some(h) = help {
        msg.push_str(&format!("\nhelp: {h}"));
    }
    msg
}

/// Byte range of the root identifier inside the template source,
/// via the first occurrence of its enclosing attribute value /
/// interpolation body. Best-effort: duplicated values may point
/// at an earlier identical occurrence, which still shows the
/// right expression.
fn locate_root(src: &str, ctx: &RootCtx, root: &str) -> Option<Range<usize>> {
    let (hay_start, hay) = match &ctx.needle {
        Needle::Attr(value) => {
            let quoted = format!("\"{value}\"");
            (src.find(&quoted)? + 1, value.as_str())
        }
        Needle::Interp(inner) => (src.find(inner.as_str())?, inner.as_str()),
    };
    let off = hay.find(root)?;
    let start = hay_start + off;
    Some(start..start + root.len())
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
                let ctx = RootCtx {
                    display: format!("pp-for=\"{value}\""),
                    needle: Needle::Attr(value.clone()),
                };
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
            let ctx = RootCtx {
                display: format!("{name}=\"{value}\""),
                needle: Needle::Attr(value.clone()),
            };
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
    let Ok(segments) = pocopine_template_parser::parse_interpolations(text) else {
        return; // The compiled-plan parser reports malformed interpolation.
    };
    for segment in segments {
        if let pocopine_template_parser::InterpolationSegment::Dynamic(src) = segment {
            let ctx = RootCtx {
                display: format!("{{{{ {} }}}}", src),
                needle: Needle::Interp(src.clone()),
            };
            harvest_expr_src(&src, &ctx, scope, out, false);
        }
    }
}

/// Parse one expression source and collect its root identifiers.
/// Unparseable sources are skipped — `emit_compiled_expr_option`
/// already turns plan-eligible parse errors into `compile_error!`,
/// and preserved attributes are the runtime's jurisdiction.
fn harvest_expr_src(src: &str, ctx: &RootCtx, scope: &[String], out: &mut Roots, listener: bool) {
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

fn harvest_expr(expr: &pocopine_expr::Expr, ctx: &RootCtx, scope: &[String], out: &mut Roots) {
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

fn push_root(kind: RootKind, root: &str, ctx: &RootCtx, scope: &[String], out: &mut Roots) {
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
        .or_insert_with(|| RootCtx {
            display: ctx.display.clone(),
            needle: match &ctx.needle {
                Needle::Attr(v) => Needle::Attr(v.clone()),
                Needle::Interp(v) => Needle::Interp(v.clone()),
            },
        });
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

/// Similar own-field name for the help line — the house
/// did-you-mean idiom (`strsim::jaro_winkler > 0.75`, same as
/// pine-icons-macros / stylekit suggestions).
fn nearest<'a>(candidates: &'a [String], root: &str) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (strsim::jaro_winkler(root, c), c.as_str()))
        .filter(|(score, _)| *score > 0.75 && *score < 1.0)
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, c)| c)
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
    fn message_carries_snippet_and_suggestion() {
        let out = assertions(r#"<span pp-text="countt"></span>"#);
        assert!(out.contains("unknown template path root `countt`"), "{out}");
        // The annotate-snippets block: origin + excerpt + caret.
        assert!(out.contains("--> test.poco:1:16"), "{out}");
        assert!(out.contains("pp-text=\\\"countt\\\""), "{out}");
        assert!(out.contains("^^^^^^"), "{out}");
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
        let out = emit_path_assertions(&ast, &ident, true, &[], Span::call_site()).to_string();
        assert!(!out.contains("\"label\""), "{out}");
        assert!(out.contains("__POC_HANDLER_KEYS , \"save\""), "{out}");
    }

    #[test]
    fn similar_name_suggestion_bounds() {
        assert_eq!(
            nearest(&["count".into(), "label".into()], "countt"),
            Some("count")
        );
        assert_eq!(nearest(&["count".into()], "zzz"), None);
        // An exact match is not a "suggestion" (score 1.0 filtered).
        assert_eq!(nearest(&["count".into()], "count"), None);
    }
}
