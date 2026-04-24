//! Compile-time `.poco` template parser (RFC 050).
//!
//! Wraps `html5ever` + `markup5ever_rcdom` to produce a
//! [`TemplateAst`] that downstream `#[component]` machinery walks
//! for structural validation (single-root check, slot-contract
//! assertions, future static checks).
//!
//! **Host-only.** This module lives in the proc-macro crate and
//! runs inside rustc during `#[component]` expansion. None of its
//! dependencies are linked into the consumer's wasm output.
//!
//! Public API — consumers call [`parse`] and walk the returned
//! tree; nothing from the `html5ever` surface leaks out.
//!
//! ## Contracts this module upholds (RFC 050)
//!
//! * Tag names are **lowercase HTML local names**. `<DIV>` and
//!   `<div>` both produce `"div"`.
//! * Attribute ordering on an element reflects source order.
//! * Duplicate attributes on the same element are reported as a
//!   `ParseError` (fatal at the caller's discretion).
//! * `Element.byte_range` spans the opening `<` of the element
//!   through the `>` of its closing tag (inclusive). For void or
//!   self-closing elements it spans just the opening tag.
//! * `Element.opening_tag_range` spans `<` through `>` of the
//!   opening tag only.
//! * `TemplateAst.roots` contains **all** top-level nodes —
//!   elements, text, comments — in source order. Callers that
//!   only care about element roots filter with
//!   [`Node::as_element`].
//! * Implicit tags produced by html5ever's fragment parsing
//!   (synthetic `<html>`, `<head>`, `<body>`) are **not**
//!   surfaced; the walker unwraps them.
//!
//! Byte-range population is scheduled for a follow-up within the
//! RFC 050 branch: the v1 module emits structurally correct ASTs
//! with placeholder `0..0` ranges. RFC 045's single-root check
//! (the first consumer) doesn't need ranges; RFC 049's consumer-
//! side scan does, and lands together with range support.

use std::ops::Range;

use html5ever::driver::ParseOpts;
use html5ever::tendril::TendrilSink;
use html5ever::tree_builder::TreeBuilderOpts;
use html5ever::{local_name, namespace_url, ns, parse_fragment, QualName};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// Parsed `.poco` template AST.
///
/// `source` and `file_path` are carried alongside the tree so
/// diagnostic renderers (RFC 049's `annotate-snippets` path)
/// can build source-highlighted errors without re-reading the
/// file.
#[derive(Debug, Clone)]
pub struct TemplateAst {
    /// Raw `.poco` bytes, verbatim. Retained for diagnostic
    /// rendering against byte-range annotations.
    pub source: String,
    /// Path string to show in errors (typically the template's
    /// path relative to the consumer crate's manifest dir).
    pub file_path: String,
    /// All top-level nodes in source order. Not restricted to
    /// elements — text and comments at the template root are
    /// preserved so downstream checks can reason about them.
    pub roots: Vec<Node>,
}

/// One element in the template tree.
#[derive(Debug, Clone)]
pub struct Element {
    /// Lowercase HTML local name (`"div"`, `"pine-context-menu-item"`).
    pub tag: String,
    /// Attributes in source order.
    pub attrs: Vec<(String, String)>,
    /// Direct children, in source order.
    pub children: Vec<Node>,
    /// Byte range covering the whole element in `source`
    /// (opening `<` through `>` of closing tag).
    ///
    /// Placeholder `0..0` until byte-range population lands
    /// (RFC 050 §4.3 golden-test matrix).
    pub byte_range: Range<usize>,
    /// Byte range covering just the opening tag (`<foo ...>`
    /// or `<foo ... />`).
    pub opening_tag_range: Range<usize>,
}

/// Any top-level or nested template node.
#[derive(Debug, Clone)]
pub enum Node {
    Element(Element),
    Text(String, Range<usize>),
    Comment(String, Range<usize>),
}

impl Node {
    /// Borrow the inner [`Element`] if this node is one.
    pub fn as_element(&self) -> Option<&Element> {
        match self {
            Node::Element(e) => Some(e),
            _ => None,
        }
    }
}

/// Recoverable parse issue reported by the underlying parser.
///
/// Per RFC 050 §4.8 these are **fatal** at the caller's
/// discretion — `#[component]` treats a non-empty error list as
/// a hard build failure after rendering each one with
/// `annotate-snippets`.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub byte_range: Range<usize>,
}

/// Parse a `.poco` template source.
///
/// Returns the structural AST alongside any parser errors. The
/// tree is populated regardless of errors so diagnostic paths
/// can reason about the author's intended shape even when the
/// markup is malformed; consumers decide whether to proceed on
/// non-empty `Vec<ParseError>`.
pub fn parse(source: &str, file_path: &str) -> (TemplateAst, Vec<ParseError>) {
    let opts = ParseOpts {
        tree_builder: TreeBuilderOpts {
            drop_doctype: true,
            ..TreeBuilderOpts::default()
        },
        ..ParseOpts::default()
    };

    // Fragment-parse in the context of a `<template>` element so
    // html5ever doesn't insert implicit `<html>/<head>/<body>`
    // wrappers around our tree.
    let dom = parse_fragment(
        RcDom::default(),
        opts,
        QualName::new(None, ns!(html), local_name!("template")),
        Vec::new(),
    )
    .one(source);

    let mut errors: Vec<ParseError> = dom
        .errors
        .iter()
        .map(|e| ParseError {
            message: e.as_ref().to_string(),
            byte_range: 0..0,
        })
        .collect();

    // Fragment parsing wraps the content in a single synthetic
    // root; walk into it to find the real top-level nodes.
    let roots = collect_fragment_roots(&dom.document, &mut errors);

    let ast = TemplateAst {
        source: source.to_string(),
        file_path: file_path.to_string(),
        roots,
    };

    (ast, errors)
}

/// RcDom's fragment parsing nests the user's content inside an
/// implicit `<html>` wrapper under `document`. This peels that
/// wrapper so `TemplateAst::roots` reflects exactly what the
/// author wrote at the template top level.
fn collect_fragment_roots(document: &Handle, errors: &mut Vec<ParseError>) -> Vec<Node> {
    let doc = document.children.borrow();
    for doc_child in doc.iter() {
        if let NodeData::Element { ref name, .. } = doc_child.data {
            if name.local == local_name!("html") {
                let html_children = doc_child.children.borrow();
                let mut out = Vec::with_capacity(html_children.len());
                for child in html_children.iter() {
                    if let Some(node) = convert_node(child, errors) {
                        out.push(node);
                    }
                }
                return out;
            }
        }
    }
    // Defensive fallback — document contained no `<html>` wrapper.
    let mut out = Vec::new();
    for child in doc.iter() {
        if let Some(node) = convert_node(child, errors) {
            out.push(node);
        }
    }
    out
}

fn convert_node(handle: &Handle, errors: &mut Vec<ParseError>) -> Option<Node> {
    match &handle.data {
        NodeData::Element {
            name,
            attrs: attrs_ref,
            ..
        } => {
            let tag = name.local.to_string().to_ascii_lowercase();

            let mut attrs: Vec<(String, String)> = Vec::new();
            let mut seen: Vec<String> = Vec::new();
            for a in attrs_ref.borrow().iter() {
                let attr_name = a.name.local.to_string().to_ascii_lowercase();
                if seen.iter().any(|s| s == &attr_name) {
                    errors.push(ParseError {
                        message: format!(
                            "duplicate attribute `{attr_name}` on <{tag}>"
                        ),
                        byte_range: 0..0,
                    });
                    continue;
                }
                seen.push(attr_name.clone());
                attrs.push((attr_name, a.value.to_string()));
            }

            let mut children: Vec<Node> = Vec::new();
            for child in handle.children.borrow().iter() {
                if let Some(c) = convert_node(child, errors) {
                    children.push(c);
                }
            }

            Some(Node::Element(Element {
                tag,
                attrs,
                children,
                byte_range: 0..0,
                opening_tag_range: 0..0,
            }))
        }
        NodeData::Text { contents } => {
            let text = contents.borrow().to_string();
            Some(Node::Text(text, 0..0))
        }
        NodeData::Comment { contents } => {
            Some(Node::Comment(contents.to_string(), 0..0))
        }
        // Doctype / ProcessingInstruction / Document are either
        // filtered by parse options (doctype) or don't occur
        // inside a fragment tree.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> TemplateAst {
        let (ast, errors) = parse(src, "test.poco");
        assert!(errors.is_empty(), "unexpected parse errors: {errors:?}");
        ast
    }

    fn elem<'a>(node: &'a Node) -> &'a Element {
        node.as_element().expect("expected element node")
    }

    #[test]
    fn single_root_element() {
        let ast = parse_ok("<div>hi</div>");
        let els: Vec<_> = ast
            .roots
            .iter()
            .filter_map(Node::as_element)
            .collect();
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].tag, "div");
    }

    #[test]
    fn tag_names_are_lowercased() {
        // Case-insensitive matching across an open-then-close
        // pair — HTML spec behaviour that `.poco` inherits.
        let ast = parse_ok("<DIV><Span></Span></DIV>");
        let root = elem(&ast.roots[0]);
        assert_eq!(root.tag, "div");
        let child = elem(&root.children[0]);
        assert_eq!(child.tag, "span");
    }

    #[test]
    fn attributes_preserve_source_order() {
        let ast = parse_ok(r#"<div a="1" b="2" c="3">x</div>"#);
        let root = elem(&ast.roots[0]);
        assert_eq!(
            root.attrs,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "3".into()),
            ]
        );
    }

    #[test]
    fn duplicate_attribute_is_reported() {
        let (ast, errors) = parse(r#"<div a="1" a="2">x</div>"#, "test.poco");
        // html5ever itself reports duplicate-attribute too; we
        // augment with our own diagnostic via the walker.
        assert!(!errors.is_empty(), "expected duplicate-attr error");
        let root = elem(&ast.roots[0]);
        // The retained attribute is the first one seen.
        assert_eq!(root.attrs, vec![("a".into(), "1".into())]);
    }

    #[test]
    fn multiple_roots_surface_independently() {
        let ast = parse_ok("<div>a</div><span>b</span>");
        let els: Vec<_> = ast
            .roots
            .iter()
            .filter_map(Node::as_element)
            .collect();
        assert_eq!(els.len(), 2);
        assert_eq!(els[0].tag, "div");
        assert_eq!(els[1].tag, "span");
    }

    #[test]
    fn zero_roots_on_comment_only() {
        let ast = parse_ok("<!-- nothing here -->");
        let els: Vec<_> = ast
            .roots
            .iter()
            .filter_map(Node::as_element)
            .collect();
        assert!(els.is_empty());
        // The comment itself is preserved at top level.
        assert!(matches!(ast.roots.first(), Some(Node::Comment(_, _))));
    }

    #[test]
    fn nested_children_walk() {
        let ast = parse_ok("<div><span><b>deep</b></span></div>");
        let root = elem(&ast.roots[0]);
        let span = elem(&root.children[0]);
        let bold = elem(&span.children[0]);
        assert_eq!(bold.tag, "b");
    }

    #[test]
    fn void_element_has_no_children() {
        let ast = parse_ok("<div><br>x<img src=\"y\"></div>");
        let root = elem(&ast.roots[0]);
        // <div> keeps br + text + img as siblings; br/img void
        // elements carry no children.
        let br = elem(&root.children[0]);
        assert_eq!(br.tag, "br");
        assert!(br.children.is_empty());
    }

    #[test]
    fn self_closing_is_structural() {
        let ast = parse_ok("<input type=\"text\" />");
        let root = elem(&ast.roots[0]);
        assert_eq!(root.tag, "input");
        assert!(root.children.is_empty());
    }

    #[test]
    fn role_placeholder_tag_round_trips() {
        // `<root>` is pocopine's role placeholder (RFC 033).
        // html5ever treats it as an unknown element; we just
        // need the parser to surface it as tag "root".
        let ast = parse_ok("<root class=\"x\"><slot></slot></root>");
        let root = elem(&ast.roots[0]);
        assert_eq!(root.tag, "root");
        assert_eq!(root.attrs, vec![("class".into(), "x".into())]);
        let slot = elem(&root.children[0]);
        assert_eq!(slot.tag, "slot");
    }
}
