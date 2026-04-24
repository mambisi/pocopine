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
//! ## Maturity marker — INFRASTRUCTURE ONLY
//!
//! This is **not** a release-ready API for safety-critical
//! compile-time checks. Two invariants the RFC promises aren't
//! yet delivered:
//!
//! * [`Element::byte_range`] and [`Element::opening_tag_range`]
//!   are placeholder `0..0` values. No diagnostic renderer
//!   should consume them yet.
//! * Duplicate-attribute detection is currently surfaced via
//!   html5ever's own error list rather than a framework-owned
//!   rule; its wording is not stable.
//!
//! Callers must not build user-facing error paths on top of
//! this module until those land. RFC 045's single-root
//! migration and RFC 049's consumer-side scan both block on
//! real byte ranges — see §5 milestones on the
//! `rfc-050-html5ever-parser` branch.
//!
//! ## Contracts the module does uphold in v1
//!
//! * Tag names are **lowercase HTML local names**. `<DIV>` and
//!   `<div>` both produce `"div"`.
//! * Attribute ordering on an element reflects source order.
//! * `TemplateAst.roots` contains **all** top-level nodes —
//!   elements, text, comments — in source order. Callers that
//!   only care about element roots filter with
//!   [`Node::as_element`] or the convenience helper
//!   [`TemplateAst::element_roots`].
//! * The fragment-parse wrapper `<html>` is unwrapped before
//!   the caller sees anything.
//! * Elements the author did **not** write (html5ever's
//!   auto-inserted `<tbody>` etc.) are marked
//!   [`Element::synthetic = true`]. Callers running structural
//!   checks **must** ignore synthetic nodes; they are retained
//!   only so tree shape round-trips for diagnostic purposes.
//!   The heuristic used is a source-scan for `<tagname` — good
//!   enough for v1, to be replaced by authoritative tracking
//!   from a custom `TreeSink` in the byte-range milestone.
//!
//! ## Parse-error policy
//!
//! Per RFC 050 §4.8, any `ParseError` is **fatal in
//! `#[component]`**. This module offers two entry points:
//!
//! * [`parse`] returns `(TemplateAst, Vec<ParseError>)` — the
//!   AST is populated even on error so diagnostic renderers can
//!   reason about the author's intended shape. Callers must
//!   inspect `errors.is_empty()` before proceeding with any
//!   structural validation.
//! * [`parse_strict`] returns `Result<TemplateAst,
//!   Vec<ParseError>>` — use when the caller's policy is
//!   "errors terminate the build." `#[component]` will use
//!   this variant once it migrates.

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
    /// **Includes synthetic elements** — callers running
    /// structural checks must filter with
    /// [`TemplateAst::element_roots`] or equivalent.
    pub roots: Vec<Node>,
}

impl TemplateAst {
    /// Iterator over top-level nodes that are **authored
    /// elements** — non-synthetic, non-text, non-comment.
    ///
    /// RFC 045's "exactly one root" rule is defined as
    /// `element_roots().count() == 1`, not `roots.len() == 1`:
    /// the former excludes top-level text / comments / html5ever's
    /// synthetic insertions.
    pub fn element_roots(&self) -> impl Iterator<Item = &Element> {
        self.roots
            .iter()
            .filter_map(Node::as_element)
            .filter(|e| !e.synthetic)
    }
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
    /// `true` if html5ever inserted this element during tree
    /// construction and the author did **not** write it (e.g.
    /// `<tbody>` auto-inserted inside `<table>`).
    ///
    /// Detected in v1 by a case-insensitive source-scan for
    /// `<tagname` — good enough for the compound-authoring
    /// patterns `.poco` files typically use. An authoritative
    /// signal will come from a custom `TreeSink` when byte-
    /// range tracking lands.
    pub synthetic: bool,
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
/// markup is malformed.
///
/// **Callers running structural validation must treat a non-
/// empty error list as fatal** (RFC 050 §4.8). Prefer
/// [`parse_strict`] when that is the policy; use this variant
/// only when a diagnostic path genuinely needs the AST
/// alongside the errors.
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
    let roots = collect_fragment_roots(&dom.document, source, &mut errors);

    let ast = TemplateAst {
        source: source.to_string(),
        file_path: file_path.to_string(),
        roots,
    };

    (ast, errors)
}

/// Same as [`parse`] but enforces the RFC 050 §4.8 policy at
/// the type level: any parse error terminates with `Err`. The
/// AST is discarded on error; callers that want the AST for
/// diagnostic rendering use [`parse`] directly.
///
/// This is the variant `#[component]` will adopt when the byte-
/// range milestone lands and the parser starts gating
/// structural validation.
pub fn parse_strict(source: &str, file_path: &str) -> Result<TemplateAst, Vec<ParseError>> {
    let (ast, errors) = parse(source, file_path);
    if errors.is_empty() {
        Ok(ast)
    } else {
        Err(errors)
    }
}

/// RcDom's fragment parsing nests the user's content inside an
/// implicit `<html>` wrapper under `document`. This peels that
/// wrapper so `TemplateAst::roots` reflects exactly what the
/// author wrote at the template top level.
fn collect_fragment_roots(
    document: &Handle,
    source: &str,
    errors: &mut Vec<ParseError>,
) -> Vec<Node> {
    let doc = document.children.borrow();
    for doc_child in doc.iter() {
        if let NodeData::Element { ref name, .. } = doc_child.data {
            if name.local == local_name!("html") {
                let html_children = doc_child.children.borrow();
                let mut out = Vec::with_capacity(html_children.len());
                for child in html_children.iter() {
                    if let Some(node) = convert_node(child, source, errors) {
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
        if let Some(node) = convert_node(child, source, errors) {
            out.push(node);
        }
    }
    out
}

fn convert_node(
    handle: &Handle,
    source: &str,
    errors: &mut Vec<ParseError>,
) -> Option<Node> {
    match &handle.data {
        NodeData::Element {
            name,
            attrs: attrs_ref,
            ..
        } => {
            let tag = name.local.to_string().to_ascii_lowercase();

            // Attribute list as html5ever saw it (already deduped
            // by the tokenizer — duplicates show up in `errors`
            // from html5ever itself in v1; a framework-owned
            // duplicate rule needs byte ranges and lands with
            // the TreeSink milestone).
            let attrs: Vec<(String, String)> = attrs_ref
                .borrow()
                .iter()
                .map(|a| (
                    a.name.local.to_string().to_ascii_lowercase(),
                    a.value.to_string(),
                ))
                .collect();

            let mut children: Vec<Node> = Vec::new();
            for child in handle.children.borrow().iter() {
                if let Some(c) = convert_node(child, source, errors) {
                    children.push(c);
                }
            }

            let synthetic = !source_contains_tag_opener(source, &tag);

            Some(Node::Element(Element {
                tag,
                attrs,
                children,
                byte_range: 0..0,
                opening_tag_range: 0..0,
                synthetic,
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

/// Case-insensitive scan for `<tagname` followed by a
/// tag-delimiter byte (whitespace, `>`, `/`). Used as a v1
/// heuristic for synthetic-element detection until a custom
/// `TreeSink` gives us authoritative authored-vs-inserted
/// information.
fn source_contains_tag_opener(source: &str, tag: &str) -> bool {
    let src = source.as_bytes();
    let needle = format!("<{tag}");
    let needle = needle.as_bytes();
    let n = needle.len();
    if n == 0 || src.len() < n {
        return false;
    }
    let mut i = 0;
    while i + n <= src.len() {
        let candidate = &src[i..i + n];
        if candidate
            .iter()
            .zip(needle)
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
        {
            // Confirm next byte is a tag delimiter.
            match src.get(i + n) {
                None => return true,
                Some(&b) => {
                    if b.is_ascii_whitespace()
                        || b == b'>'
                        || b == b'/'
                    {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
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
        assert_eq!(ast.element_roots().count(), 1);
        assert_eq!(ast.element_roots().next().unwrap().tag, "div");
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
    fn duplicate_attribute_is_reported_by_parser() {
        // v1 note: duplicate-attr detection is currently sourced
        // from html5ever's own error list rather than a
        // framework-owned rule. A stable-wording pocopine
        // diagnostic will land with byte ranges; this test only
        // pins down that we *do* surface an error today.
        let (ast, errors) = parse(r#"<div a="1" a="2">x</div>"#, "test.poco");
        assert!(!errors.is_empty(), "expected parser error for duplicate attr");
        let root = elem(&ast.roots[0]);
        // html5ever retains the first occurrence; we don't
        // invent additional deduping on top.
        assert_eq!(root.attrs, vec![("a".into(), "1".into())]);
    }

    #[test]
    fn parse_strict_errors_on_malformed() {
        let err = parse_strict(r#"<div a="1" a="2">x</div>"#, "test.poco").err();
        assert!(err.is_some(), "parse_strict should reject duplicate-attr input");
    }

    #[test]
    fn parse_strict_passes_on_clean_input() {
        let ast = parse_strict("<div>hi</div>", "test.poco").expect("clean input");
        assert_eq!(ast.element_roots().count(), 1);
    }

    #[test]
    fn multiple_roots_surface_independently() {
        let ast = parse_ok("<div>a</div><span>b</span>");
        assert_eq!(ast.element_roots().count(), 2);
        let tags: Vec<_> = ast.element_roots().map(|e| e.tag.as_str()).collect();
        assert_eq!(tags, vec!["div", "span"]);
    }

    #[test]
    fn zero_element_roots_on_comment_only() {
        let ast = parse_ok("<!-- nothing here -->");
        assert_eq!(ast.element_roots().count(), 0);
        // Raw `roots` still contains the comment.
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

    // ── Synthetic-node handling ───────────────────────────────

    #[test]
    fn authored_element_is_not_synthetic() {
        let ast = parse_ok("<div>hi</div>");
        let root = elem(&ast.roots[0]);
        assert!(!root.synthetic, "div was written by the author");
    }

    #[test]
    fn html5ever_inserted_tbody_is_synthetic() {
        // <table><tr>...</tr></table> causes html5ever to
        // insert <tbody> between <table> and <tr>. The author
        // did not write <tbody>, so it must be flagged
        // synthetic so compile-time checks can skip it.
        let ast = parse_ok("<table><tr><td>x</td></tr></table>");
        let table = elem(&ast.roots[0]);
        assert_eq!(table.tag, "table");
        assert!(!table.synthetic, "author wrote <table>");
        // Expect a <tbody> child that was inserted by the parser.
        let tbody = elem(&table.children[0]);
        assert_eq!(tbody.tag, "tbody");
        assert!(tbody.synthetic, "tbody was inserted, not authored");
        // The <tr> below it is authored.
        let tr = elem(&tbody.children[0]);
        assert_eq!(tr.tag, "tr");
        assert!(!tr.synthetic);
    }

    #[test]
    fn element_roots_filters_synthetic_and_non_elements() {
        // Mixed top level: text, authored element, comment.
        // element_roots() returns only the authored element.
        let ast = parse_ok("hello<div>x</div><!-- end -->");
        let els: Vec<_> = ast.element_roots().collect();
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].tag, "div");
    }
}
