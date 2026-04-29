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
//! Pending follow-up commits will add `pp-data` (rename to
//! private marker) and the convergence directives `pp-let` /
//! `pp-key` / `pp-stagger`.
//!
//! The scan walks the parsed AST once per `#[component]`
//! invocation; cost is per-attribute string compare on a
//! handful of names. Negligible at macro-expansion time.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;

use crate::template_parser::{Element, Node, TemplateAst};

/// Each entry: `(attr_name, message)`. Adding a new directive to
/// the table is the entire migration step — the walker handles
/// the rest.
const FORBIDDEN: &[(&str, &str)] = &[
    (
        "pp-cloak",
        "`pp-cloak` was removed in v2 (RFC 063 §4.1.2). Mount is \
         synchronous post-RFC-061; the FOUC the directive guarded \
         against no longer happens. Drop the attribute.",
    ),
    (
        "pp-init",
        "`pp-init` was removed in v2 (RFC 063 §4.1.1). Use the \
         `on_setup` lifecycle hook instead:\n\n  \
         #[handlers]\n  \
         impl MyComponent {\n      \
             fn on_setup(&mut self) {\n          \
                 // your init code here\n      \
             }\n  \
         }",
    ),
];

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
        if let Some(&(name, message)) = FORBIDDEN.iter().find(|(n, _)| *n == attr_name) {
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
