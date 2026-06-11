//! RFC 063 — hard-error scan for directives the framework no
//! longer accepts.
//!
//! Each entry below names a directive RFC 063 §4.1 deletes,
//! the macro detects it in the consumer's template AST, and
//! emits a `compile_error!` with the documented migration
//! story. The runtime still has whatever support code the
//! detector itself doesn't touch — those deletions land in
//! follow-up commits per directive.
//!
//! Current entries:
//!
//! - `pp-cloak` — runtime style was deleted (mount is
//!   synchronous post-RFC-061; FOUC no longer exists). RFC 063
//!   §4.1.2.
//! - `pp-init` — `directives::init` module + `StaticInit` plan
//!   IR + macro emit pass deleted. RFC 063 §4.1.1. Replacement:
//!   `#[handlers] impl Foo { fn on_setup(&mut self) { ... } }`.
//! - `pp-data` — author-facing surface deleted. RFC 063 §4.1.3.
//!   The macro auto-stamps the scope marker on every component
//!   root automatically; authors never needed to type it. The
//!   internal marker rename to `data-pp-scope-id` is a separate
//!   follow-up cleanup PR.
//!
//! **Explicitly excluded** (kept by design):
//!
//! - `pp-html` — every modern web framework ships an HTML-string
//!   injection primitive (Vue `v-html`, React
//!   `dangerouslySetInnerHTML`, Svelte `{@html}`, Solid
//!   `innerHTML`, Yew `Html::from_html_unchecked`). Removing
//!   puts pocopine at parity with no one. RFC 063 §1 + §4.4
//!   spec the pine-icons rewrite that retires the only current
//!   workspace consumer.
//!
//! Pending follow-up commits will add the convergence
//! directives `pp-let` / `pp-key` / `pp-stagger`.
//!
//! The scan walks the parsed AST once per `#[component]`
//! invocation; cost is per-attribute string compare on a
//! handful of names. Negligible at macro-expansion time.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;

use crate::template_parser::{Element, Node, TemplateAst};

// The removed-directive table itself now lives in `pocopine-directives`
// ([`pocopine_directives::REMOVED`]) so this macro's hard-error scan and the
// `pocopine lsp` language server validate the exact same surface. This module
// is just the AST walk that turns a match into a `compile_error!`.

pub(crate) fn emit_diagnostics(ast: &TemplateAst) -> TokenStream {
    let mut out = TokenStream::new();
    let mut seen: HashSet<&'static str> = HashSet::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, &mut out, &mut seen);
        }
    }
    out
}

fn walk(el: &Element, out: &mut TokenStream, seen: &mut HashSet<&'static str>) {
    for (attr_name, _) in &el.attrs {
        // Removed directives are author-facing `pp-*` attributes; strip the
        // prefix and consult the shared registry by head name.
        let Some(head) = attr_name.strip_prefix("pp-") else {
            continue;
        };
        if let Some(&(name, message)) = pocopine_directives::REMOVED
            .iter()
            .find(|(n, _)| *n == head)
        {
            if seen.insert(name) {
                let msg = format!("pocopine: {message}");
                out.extend(quote! { ::core::compile_error!(#msg); });
            }
        }
    }
    for child in &el.children {
        if let Node::Element(child_el) = child {
            walk(child_el, out, seen);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template_parser::parse_strict;

    /// Runs `emit_diagnostics` against `source` and returns the
    /// raw token-stream string for substring assertions. The
    /// stringified `compile_error!(...)` carries the diagnostic
    /// message as a literal, so substring checks are stable
    /// across `proc_macro2` formatting changes.
    fn run(source: &str) -> String {
        let ast = parse_strict(source, "<test>").expect("test source parses");
        emit_diagnostics(&ast).to_string()
    }

    #[test]
    fn pp_cloak_emits_compile_error_with_migration_text() {
        let out = run(r#"<div pp-cloak>x</div>"#);
        assert!(
            out.contains("compile_error"),
            "pp-cloak must surface a compile_error invocation: {out}"
        );
        assert!(
            out.contains("pp-cloak"),
            "diagnostic must name the directive so authors find the call site"
        );
        assert!(
            out.contains("RFC 063"),
            "diagnostic must cite RFC 063 so authors find the migration spec"
        );
        assert!(
            out.contains("synchronous"),
            "diagnostic must explain why the directive is gone (synchronous mount)"
        );
    }

    #[test]
    fn pp_init_emits_compile_error_with_on_setup_replacement() {
        let out = run(r#"<div pp-init="seed">x</div>"#);
        assert!(out.contains("compile_error"));
        assert!(out.contains("pp-init"));
        assert!(out.contains("RFC 063"));
        assert!(
            out.contains("on_setup"),
            "diagnostic must point authors at the on_setup lifecycle hook replacement"
        );
        assert!(
            out.contains("handlers"),
            "diagnostic must reference the #[handlers] block where on_setup lives"
        );
    }

    #[test]
    fn pp_data_emits_compile_error_explaining_auto_stamp() {
        let out = run(r#"<div pp-data="counter">x</div>"#);
        assert!(out.contains("compile_error"));
        assert!(out.contains("pp-data"));
        assert!(out.contains("RFC 063"));
        assert!(
            out.contains("auto-stamps"),
            "diagnostic must explain that the marker is auto-injected so authors don't try to manually add it back"
        );
    }

    #[test]
    fn pp_html_is_not_forbidden() {
        // RFC 063 §1: pp-html stays. Surveyed every other modern
        // web framework — all ship an HTML-string injection
        // primitive. This test pins that decision: a future
        // careless edit that adds pp-html to FORBIDDEN fails
        // here.
        let out = run(r#"<div pp-html="markdown">x</div>"#);
        assert!(
            !out.contains("compile_error"),
            "pp-html must not be forbidden — every modern web framework \
             ships an HTML-string injection primitive (Vue v-html, React \
             dangerouslySetInnerHTML, Svelte @html, Solid innerHTML, Yew \
             Html::from_html_unchecked). RFC 063 §1 + §4.4."
        );
    }

    #[test]
    fn nested_forbidden_attr_emits_compile_error() {
        let out = run(r#"<section><span pp-cloak>x</span></section>"#);
        assert!(
            out.contains("compile_error"),
            "scanner must descend into children, not only check root attrs"
        );
    }

    #[test]
    fn duplicate_forbidden_attr_emits_one_diagnostic() {
        // The `seen` set dedupes by directive name across the
        // whole template — useful when a template has many sites
        // of the same forbidden attr and the author shouldn't
        // get spammed with N copies of the same migration text.
        let out = run(r#"<div><span pp-cloak>a</span><span pp-cloak>b</span></div>"#);
        let count = out.matches("compile_error").count();
        assert_eq!(
            count, 1,
            "duplicate forbidden attrs must emit one diagnostic, got {count}"
        );
    }

    #[test]
    fn unrelated_pp_attrs_pass_through() {
        // Sanity: only the three forbidden directives should
        // trigger. pp-text / pp-bind / pp-on / pp-html / pp-show
        // / pp-model / pp-ref / pp-as / pp-for / pp-if /
        // pp-teleport / pp-slot all stay.
        let out = run(r#"<div pp-text="x" pp-show="open" :title="t" @click="bump">
                <span pp-html="md"></span>
            </div>"#);
        assert!(
            !out.contains("compile_error"),
            "the kept directives must pass through the forbidden scanner cleanly: {out}"
        );
    }
}
