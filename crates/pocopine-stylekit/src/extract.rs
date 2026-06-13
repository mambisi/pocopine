//! `.poco` class extraction (RFC 092 D5).
//!
//! Extraction runs on the **real Pocopine parser** (the shared
//! `pocopine-template-parser` crate, the same AST `#[component]` uses),
//! not a text scan — so a `class="…"` inside a comment or string
//! literal is never mistaken for a real attribute. v1 recognises:
//!
//! - static `class="…"` attributes (full support, with spans),
//! - `pp-bind:class` / `:class` **only** when the class set is
//!   statically discoverable (quoted literals in the expression).
//!
//! Opaque dynamic construction yields a [`Diagnostic`] pointing at a
//! migration path to a static class map — never a silent miss.
//!
//! Attribute-value spans are derived by locating the attribute within
//! the element's `opening_tag_range`; the parser does not yet track
//! per-attribute spans (a follow-up that this crate's needs motivate).

use pocopine_template_parser::{Element, Node, parse};

use crate::diagnostics::{Diagnostic, Span};

/// One class token discovered in a source file, with its location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedClass {
    /// Verbatim class literal, e.g. `hover:bg-surface` or `text-[13px]`.
    pub value: String,
    /// Where it appears, for diagnostics.
    pub span: Span,
}

impl UsedClass {
    pub fn new(value: impl Into<String>, span: Span) -> Self {
        Self {
            value: value.into(),
            span,
        }
    }
}

/// Everything one `.poco` file yields: the discovered classes plus any
/// extraction-time diagnostics (opaque dynamic bindings, parse errors).
#[derive(Debug, Default)]
pub struct Extraction {
    pub classes: Vec<UsedClass>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Extract used classes from one `.poco` source file. `file` is the
/// caller's file-table id, threaded into every [`Span`].
pub fn extract_poco(file: u32, source: &str) -> Extraction {
    let mut out = Extraction::default();
    let (ast, errors) = parse(source, "<stylekit>");
    for err in errors {
        // Surface only framework-anchored parse errors (non-empty byte
        // range); html5ever recovery notices (`0..0`) are noise for
        // class extraction.
        if err.byte_range != (0..0) {
            out.diagnostics.push(
                Diagnostic::error(format!("parse error: {}", err.message)).at(Span {
                    file,
                    start: err.byte_range.start as u32,
                    end: err.byte_range.end as u32,
                }),
            );
        }
    }
    for node in &ast.roots {
        walk(file, source, node, &mut out);
    }
    out
}

fn walk(file: u32, source: &str, node: &Node, out: &mut Extraction) {
    let Node::Element(el) = node else { return };
    collect_attrs(file, source, el, out);
    for child in &el.children {
        walk(file, source, child, out);
    }
}

fn collect_attrs(file: u32, source: &str, el: &Element, out: &mut Extraction) {
    for (name, value) in &el.attrs {
        match name.as_str() {
            "class" => {
                let base =
                    attr_value_offset(source, el, value).unwrap_or(el.opening_tag_range.start);
                push_tokens(file, value, base, out);
            }
            "pp-bind:class" | ":class" => extract_dynamic(file, source, el, value, out),
            _ => {}
        }
    }
}

/// Split a static class string into tokens, each with a span derived
/// from its offset within the attribute value.
fn push_tokens(file: u32, value: &str, value_base: usize, out: &mut Extraction) {
    let mut cursor = 0;
    for token in value.split_whitespace() {
        // Advance the cursor to this token (split_whitespace collapses
        // runs, so re-find from the running cursor).
        if let Some(rel) = value[cursor..].find(token) {
            let start = value_base + cursor + rel;
            cursor += rel + token.len();
            out.classes.push(UsedClass {
                value: token.to_string(),
                span: Span {
                    file,
                    start: start as u32,
                    end: (start + token.len()) as u32,
                },
            });
        }
    }
}

/// Best-effort static discovery inside a class binding. Pulls quoted
/// string literals (the keys of a static class map); if none are found
/// the binding is opaque and gets a migration diagnostic (RFC 092 D5).
fn extract_dynamic(file: u32, source: &str, el: &Element, expr: &str, out: &mut Extraction) {
    let base = attr_value_offset(source, el, expr).unwrap_or(el.opening_tag_range.start);
    let mut found = false;
    for (rel, lit) in string_literals(expr) {
        found = true;
        let start = base + rel;
        // A literal may itself hold multiple space-separated classes.
        push_tokens(file, lit, start, out);
    }
    if !found {
        out.diagnostics.push(
            Diagnostic::error("dynamic class binding has no statically discoverable classes")
                .with_help("move classes into a static map, e.g. :class=\"{ \"px-2\": cond }\"")
                .at(Span {
                    file,
                    start: el.opening_tag_range.start as u32,
                    end: el.opening_tag_range.end as u32,
                }),
        );
    }
}

/// Yield `(relative_offset, contents)` for every single- or
/// double-quoted string literal in an expression.
fn string_literals(expr: &str) -> Vec<(usize, &str)> {
    let bytes = expr.as_bytes();
    let mut lits = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let q = bytes[i];
        if q == b'"' || q == b'\'' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != q {
                j += 1;
            }
            if j <= bytes.len() {
                lits.push((start, &expr[start..j.min(expr.len())]));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    lits
}

/// Locate the byte offset of an attribute's value within the source, by
/// searching the element's opening tag for `="value"` / `='value'`.
fn attr_value_offset(source: &str, el: &Element, value: &str) -> Option<usize> {
    let range = el.opening_tag_range.clone();
    let tag = source.get(range.clone())?;
    for quote in ['"', '\''] {
        let needle = format!("={quote}{value}{quote}");
        if let Some(pos) = tag.find(&needle) {
            return Some(range.start + pos + 2); // skip `=` and opening quote
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(src: &str) -> Vec<String> {
        extract_poco(0, src)
            .classes
            .into_iter()
            .map(|c| c.value)
            .collect()
    }

    #[test]
    fn static_class_attribute() {
        let got = classes(r#"<div class="flex items-center gap-2"></div>"#);
        assert_eq!(got, ["flex", "items-center", "gap-2"]);
    }

    #[test]
    fn nested_elements() {
        let src = r#"<section class="p-4"><button class="bg-surface">x</button></section>"#;
        let got = classes(src);
        assert!(got.contains(&"p-4".to_string()));
        assert!(got.contains(&"bg-surface".to_string()));
    }

    #[test]
    fn variant_classes_with_brackets() {
        let got = classes(r#"<div class="hover:bg-ink-10 data-[state=on]:text-accent"></div>"#);
        assert_eq!(got, ["hover:bg-ink-10", "data-[state=on]:text-accent"]);
    }

    #[test]
    fn spans_point_at_each_token() {
        let src = r#"<i class="a bc"></i>"#;
        let ex = extract_poco(7, src);
        // `a` starts at the value offset; `bc` two chars later.
        assert_eq!(ex.classes[0].value, "a");
        assert_eq!(ex.classes[0].span.file, 7);
        let a = &ex.classes[0].span;
        assert_eq!(&src[a.start as usize..a.end as usize], "a");
        let bc = &ex.classes[1].span;
        assert_eq!(&src[bc.start as usize..bc.end as usize], "bc");
    }

    #[test]
    fn static_map_in_binding_is_discovered() {
        // Inner literals are single-quoted inside the double-quoted attr
        // (the only well-formed nesting in HTML).
        let got = classes(r#"<div :class="{ 'px-2': cond, 'bg-surface': active }"></div>"#);
        assert!(got.contains(&"px-2".to_string()));
        assert!(got.contains(&"bg-surface".to_string()));
    }

    #[test]
    fn opaque_binding_diagnoses() {
        let ex = extract_poco(0, r#"<div :class="dynamic_classes(state)"></div>"#);
        assert!(ex.classes.is_empty());
        assert_eq!(ex.diagnostics.len(), 1);
        assert!(
            ex.diagnostics[0]
                .message
                .contains("statically discoverable")
        );
    }

    #[test]
    fn class_in_comment_is_ignored() {
        // Real parser, not text scan: the commented class must not leak.
        let got = classes(r#"<div class="flex"><!-- class="leaked" --></div>"#);
        assert_eq!(got, ["flex"]);
    }
}
