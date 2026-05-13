//! Issue #76 — reject `<template pp-for>` rows whose direct
//! element child is another `<template>`.
//!
//! Background: `pp-for` repeats a single concrete row root.
//! Authors coming from Vue/Svelte naturally reach for nested
//! `<template>` blocks (`<template pp-for><template
//! pp-if>...</template></template>`) to filter or conditionally
//! render rows. The compiled row-plan IR and keyed reconciler
//! both assume one concrete row root per iteration, so the
//! nested-template shape silently breaks row identity and
//! row-plan eligibility.
//!
//! For v1 we forbid the shape with a Rust-style diagnostic
//! anchored at the offending inner `<template>`, instead of
//! growing the directive surface with `pp-filter` or supporting
//! fragment rows. The recommended migration is to hoist the
//! condition onto the concrete row element with `pp-show` (or
//! precompute the filtered list in Rust state) — see the
//! companion `keep` example for the canonical shape.
//!
//! Implementation: walks the AST once, descends through
//! synthetic html5ever wrappers (`<tbody>`-style), and at every
//! `<template pp-for>` checks each direct element child for a
//! `<template>` tag. Each violation emits one
//! `compile_error!(<rendered snippet>)` token. Detection is
//! per-attribute string compare on a handful of names —
//! negligible at macro-expansion time.

use proc_macro2::TokenStream;
use quote::quote;

use crate::diagnostics;
use crate::template_parser::{Element, Node, TemplateAst};

/// Scan `ast` for `<template pp-for>` rows whose direct element
/// child is another `<template>` and emit a `compile_error!`
/// per violation. Returns an empty stream when nothing is
/// wrong.
pub(crate) fn emit_diagnostics(ast: &TemplateAst) -> TokenStream {
    let mut out = TokenStream::new();
    for node in &ast.roots {
        if let Node::Element(el) = node {
            walk(el, ast, &mut out);
        }
    }
    out
}

fn walk(el: &Element, ast: &TemplateAst, out: &mut TokenStream) {
    if is_pp_for_template(el) {
        for child in &el.children {
            if let Node::Element(child_el) = child {
                check_row_child(child_el, ast, out);
            }
        }
    }
    for child in &el.children {
        if let Node::Element(child_el) = child {
            walk(child_el, ast, out);
        }
    }
}

/// Inspect one direct element child of a `<template pp-for>`.
/// If it's a `<template>` (with or without further directives),
/// emit the diagnostic. Synthetic html5ever-inserted wrappers
/// (`<tbody>`, etc.) are transparent — descend through them so
/// `<template pp-for>` inside `<table>` is checked correctly.
fn check_row_child(child: &Element, ast: &TemplateAst, out: &mut TokenStream) {
    if child.synthetic {
        for grand in &child.children {
            if let Node::Element(grand_el) = grand {
                check_row_child(grand_el, ast, out);
            }
        }
        return;
    }
    if child.tag != "template" {
        return;
    }
    let range = if child.opening_tag_range.end > child.opening_tag_range.start {
        child.opening_tag_range.clone()
    } else {
        // No source mapping — skip silently. The parser would
        // already have surfaced a structural error if the
        // markup was malformed enough to lose position info.
        return;
    };
    let rendered = diagnostics::render_template_error_with_footers(
        &ast.source,
        &ast.file_path,
        range,
        "`<template pp-for>` rows must start with a concrete element",
        "nested `<template>` cannot be used as the row root",
        Some(
            "move the condition onto the concrete row element with `pp-show`, \
             or precompute a filtered list in Rust state and iterate it directly",
        ),
        Some(
            "`pp-for` requires exactly one concrete row root so keyed \
             reconciliation and compiled row plans can track row identity \
             directly (issue #76)",
        ),
    );
    out.extend(quote! { ::core::compile_error!(#rendered); });
}

fn is_pp_for_template(el: &Element) -> bool {
    el.tag == "template" && el.attrs.iter().any(|(name, _)| name == "pp-for")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template_parser::parse_strict;

    fn run(source: &str) -> String {
        let ast = parse_strict(source, "test.poco").expect("test source parses");
        emit_diagnostics(&ast).to_string()
    }

    #[test]
    fn nested_template_pp_if_inside_pp_for_emits_compile_error() {
        let out = run(r#"<div>
  <template pp-for="lbl in labels" pp-key="lbl">
    <template pp-if="visible">
      <label>x</label>
    </template>
  </template>
</div>"#);
        assert!(
            out.contains("compile_error"),
            "nested <template> inside <template pp-for> must surface a compile_error: {out}"
        );
        assert!(
            out.contains("pp-for"),
            "diagnostic must name the directive so authors find the call site"
        );
        assert!(
            out.contains("pp-show"),
            "diagnostic must suggest pp-show as the migration"
        );
    }

    #[test]
    fn plain_nested_template_inside_pp_for_also_fires() {
        // Even without an inner pp-if/pp-else, a bare
        // <template> as the row root is rejected — the row
        // body must be a concrete element.
        let out = run(r#"<ul>
  <template pp-for="x in xs" pp-key="x.id">
    <template>
      <li>plain</li>
    </template>
  </template>
</ul>"#);
        assert!(
            out.contains("compile_error"),
            "bare nested <template> as the row root must still emit a diagnostic"
        );
    }

    #[test]
    fn concrete_element_row_root_passes() {
        let out = run(r#"<ul>
  <template pp-for="x in xs" pp-key="x.id">
    <li pp-show="x.visible">{{ x.name }}</li>
  </template>
</ul>"#);
        assert!(
            !out.contains("compile_error"),
            "the recommended shape (pp-show on a concrete row root) must pass cleanly: {out}"
        );
    }

    #[test]
    fn nested_template_outside_pp_for_is_not_flagged() {
        // The rule is specifically about <template pp-for>
        // rows. Nested <template> elsewhere in the document
        // (e.g. an unrelated content block) must not be
        // flagged by this scan.
        let out = run(r#"<section>
  <template>
    <p>just inert markup</p>
  </template>
  <ul>
    <template pp-for="x in xs" pp-key="x.id">
      <li>{{ x }}</li>
    </template>
  </ul>
</section>"#);
        assert!(
            !out.contains("compile_error"),
            "<template> outside a <template pp-for> must not be flagged: {out}"
        );
    }

    #[test]
    fn nested_template_grandchild_is_not_flagged() {
        // Only the **direct** element child of <template
        // pp-for> needs to be concrete. A nested <template>
        // deeper inside the row body is the row author's
        // problem (and runtime/parser concerns), not this
        // diagnostic's.
        let out = run(r#"<ul>
  <template pp-for="x in xs" pp-key="x.id">
    <li>
      <template>nested-grandchild</template>
    </li>
  </template>
</ul>"#);
        assert!(
            !out.contains("compile_error"),
            "a deeper <template> inside the concrete row root must not be flagged: {out}"
        );
    }

    #[test]
    fn nested_pp_for_with_template_row_is_flagged() {
        // Catches the "loop-of-loops via nested template"
        // anti-pattern — both the inner and outer pp-for
        // disqualify, but only the inner <template pp-for>
        // (the row root) violates the rule the diagnostic
        // teaches.
        let out = run(r#"<table>
  <template pp-for="row in rows" pp-key="row.id">
    <template pp-for="cell in row.cells" pp-key="cell.id">
      <td>{{ cell }}</td>
    </template>
  </template>
</table>"#);
        assert!(
            out.contains("compile_error"),
            "nested <template pp-for> as the row root must be flagged: {out}"
        );
    }

    #[test]
    fn diagnostic_renders_one_per_violation() {
        // Two separate <template pp-for> rows, each with a
        // <template> row root — both should fire. The
        // forbidden-directives scanner dedupes by directive
        // name; this scanner targets *positions*, so each
        // distinct site is its own diagnostic.
        let out = run(r#"<div>
  <template pp-for="a in xs" pp-key="a">
    <template pp-if="a.ok"><span>a</span></template>
  </template>
  <template pp-for="b in ys" pp-key="b">
    <template pp-if="b.ok"><span>b</span></template>
  </template>
</div>"#);
        let count = out.matches("compile_error").count();
        assert_eq!(
            count, 2,
            "each distinct violation site must emit its own diagnostic, got {count}: {out}"
        );
    }
}
