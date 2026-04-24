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
//! ## Maturity marker
//!
//! This API is **safe to consume for RFC 045 single-root
//! validation and RFC 049 consumer-side scans**. What it
//! delivers:
//!
//! * [`Element::opening_tag_range`] and [`Element::byte_range`]
//!   are populated by the source-mapping pass. Ranges are
//!   exact for every authored element; synthetic elements
//!   keep `0..0` and must be filtered before any diagnostic
//!   rendering (use [`TemplateAst::element_roots`]).
//!
//! Still outstanding:
//!
//! * Duplicate-attribute detection is currently surfaced via
//!   html5ever's own error list rather than a framework-owned
//!   rule; the specific error wording is not stable, though
//!   the fact that an error appears in `Vec<ParseError>` is.
//! * Text / comment node ranges are still `0..0`; not needed
//!   by the current consumers.
//! * `Element.synthetic` uses a case-insensitive source-scan
//!   heuristic. Authoritative tracking via a custom TreeSink
//!   is deferred.
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
//! `ParseError` values fall into two categories distinguished
//! by whether they carry an anchored byte range:
//!
//! 1. **Framework-owned errors** (`byte_range.end > start`) —
//!    produced by our own pre-parse validators (the
//!    `<tagname/>` self-close rule today, future pocopine
//!    checks). These are **fatal** in `parse_strict` and must
//!    terminate the build in `#[component]`.
//! 2. **html5ever spec-recovery notices** (`byte_range ==
//!    0..0`) — produced when html5ever's tree builder applies
//!    HTML5 recovery rules (foster-parenting, "Unexpected
//!    token", duplicate-attr deduping, etc.). These come
//!    through `parse()`'s error list for diagnostic purposes
//!    but **do not** gate compilation in `parse_strict`;
//!    they represent html5ever-internal observations, not
//!    pocopine-owned correctness rules. Callers running
//!    strict validation over their own invariants must install
//!    framework-owned rules to gate those — matching an
//!    html5ever wording string is not a stable contract.
//!
//! This module offers two entry points:
//!
//! * [`parse`] returns `(TemplateAst, Vec<ParseError>)` —
//!   surfaces **all** parse issues (both categories) alongside
//!   the AST. Use for diagnostic rendering that wants to show
//!   every observation.
//! * [`parse_strict`] returns `Result<TemplateAst,
//!   Vec<ParseError>>` — only rejects on framework-owned
//!   errors per the rule above. `#[component]` uses this
//!   variant for the RFC 045 single-root migration.
//!
//! The rationale: pocopine should not turn html5ever into a
//! general HTML validity gate mid-migration. RFC 049 and
//! future consumers will install their own framework-owned
//! rules (duplicate-attr wording, specific template-shape
//! constraints) and those will flow through the fatal path.

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
/// file. RFC 045 doesn't currently read them — RFC 049 will —
/// so we document the intent and silence dead-code warnings
/// at the struct level.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields are public AST contract; RFC 049 consumes them
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
    /// elements that map to an actual source position** —
    /// non-synthetic, non-text, non-comment, **and with a
    /// non-zero byte range**.
    ///
    /// Rationale: html5ever's spec-mandated recovery can
    /// foster-parent content out of its authored parent
    /// (e.g. `<slot>` inside `<tr>` gets hoisted to the tree
    /// root). Foster-parented elements get mapped to `0..0`
    /// by the source-mapping pass (their position in the tree
    /// doesn't match their position in source), so filtering
    /// on a non-zero byte range excludes them from root
    /// counts.
    ///
    /// RFC 045's "exactly one root" rule is defined as
    /// `element_roots().count() == 1`, not `roots.len() == 1`:
    /// the former excludes top-level text / comments /
    /// html5ever's synthetic insertions / foster-parented
    /// descendants that bubbled up out of their authored
    /// parent.
    pub fn element_roots(&self) -> impl Iterator<Item = &Element> {
        self.roots
            .iter()
            .filter_map(Node::as_element)
            .filter(|e| !e.synthetic)
            .filter(|e| e.opening_tag_range.end > e.opening_tag_range.start)
    }
}

/// One element in the template tree.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields are public AST contract; RFC 049 consumes them
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
#[allow(dead_code)] // variant payloads are public AST contract; RFC 049 consumes them
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
/// Returns the structural AST alongside **every** parser
/// observation — both framework-owned errors (anchored byte
/// range) and html5ever spec-recovery notices (unanchored,
/// `byte_range == 0..0`). See the module-level "Parse-error
/// policy" section for the distinction.
///
/// The tree is populated regardless of errors so diagnostic
/// paths can reason about the author's intended shape even
/// when the markup is malformed.
///
/// Callers that want to gate compilation on errors must
/// filter the returned `Vec<ParseError>` by byte range —
/// anchored entries are framework-owned and safe to treat as
/// fatal; unanchored entries are html5ever recovery and
/// should not drive hard errors. Most consumers should use
/// [`parse_strict`], which already applies this rule.
pub fn parse(source: &str, file_path: &str) -> (TemplateAst, Vec<ParseError>) {
    // Pre-parse: pocopine-specific syntax rules that html5ever
    // would either accept permissively or reject with wording
    // that doesn't map to our semantics. Running these first
    // gives authors a framework-owned diagnostic on the exact
    // tag (with byte range), instead of relying on html5ever's
    // generic "Unacknowledged self-closing tag" message that
    // has no position anchor.
    let mut errors: Vec<ParseError> = Vec::new();
    detect_forbidden_self_close(source, &mut errors);

    let opts = ParseOpts {
        tree_builder: TreeBuilderOpts {
            drop_doctype: true,
            ..TreeBuilderOpts::default()
        },
        ..ParseOpts::default()
    };

    // Fragment-parse in a context element chosen based on the
    // source's first real opening tag. HTML5's fragment parser
    // picks an insertion mode from the context element; using
    // a blanket `<div>` or `<template>` would reject `<tr>`,
    // `<td>`, `<tbody>` etc. at the root because those only
    // parse inside a table-ish ancestor.
    //
    // The pocopine walker calls `set_inner_html` on whatever
    // custom-element tag the component uses, and the browser
    // picks the right insertion mode from that. We mirror that
    // by detecting the first tag and choosing an equivalent
    // context: `<tr>` root → `<tbody>` context, `<td>/<th>` →
    // `<tr>`, `<tbody>/<thead>/<tfoot>/<caption>/<colgroup>` →
    // `<table>`, everything else → `<div>`.
    let context = detect_fragment_context(source);
    let dom = parse_fragment(RcDom::default(), opts, context, Vec::new()).one(source);

    for e in dom.errors.iter() {
        let msg = e.as_ref().to_string();
        // Suppress html5ever's own "Unacknowledged self-closing
        // tag" wording — our pre-scan already emitted a
        // framework-owned error with an exact byte range.
        if msg == "Unacknowledged self-closing tag" {
            continue;
        }
        errors.push(ParseError {
            message: msg,
            byte_range: 0..0,
        });
    }

    // Fragment parsing wraps the content in a single synthetic
    // root; walk into it to find the real top-level nodes.
    let mut roots = collect_fragment_roots(&dom.document, source, &mut errors);

    // Post-parse: reconcile html5ever's tree with source byte
    // positions. Populates `opening_tag_range` + `byte_range`
    // on every authored Element.
    map_positions(source, &mut roots);

    let ast = TemplateAst {
        source: source.to_string(),
        file_path: file_path.to_string(),
        roots,
    };

    (ast, errors)
}

/// Same as [`parse`] but enforces the RFC 050 §4.8 policy at
/// the type level: a **framework-owned** parse error
/// terminates with `Err`. html5ever's own spec-mandated
/// recovery notices (foster-parenting, "Unexpected token"
/// etc.) come back with `byte_range = 0..0` and are treated
/// as informational — they'd reject templates that Pine uses
/// in practice (`<tr>` / `<th>` roots that trigger table-
/// insertion-mode recovery), without any actionable author
/// signal. Framework-owned rules (our self-close check,
/// future pocopine diagnostics) always ship with a non-zero
/// byte range and DO fail strict parsing.
///
/// This is the variant `#[component]` uses to gate RFC 045
/// structural validation.
pub fn parse_strict(source: &str, file_path: &str) -> Result<TemplateAst, Vec<ParseError>> {
    let (ast, errors) = parse(source, file_path);
    let fatal: Vec<_> = errors
        .iter()
        .filter(|e| e.byte_range.end > e.byte_range.start)
        .cloned()
        .collect();
    if fatal.is_empty() {
        Ok(ast)
    } else {
        Err(fatal)
    }
}

// ── Source-mapping pass — populate byte ranges ──────────────────

/// Walk the AST in DFS order alongside a source-byte cursor,
/// populating `opening_tag_range` and `byte_range` on every
/// authored [`Element`]. Synthetic elements keep the placeholder
/// `0..0` ranges; their `children` are still walked because the
/// author's real markup is nested inside them.
///
/// This isn't a full HTML parser — html5ever owns the structure;
/// this pass reconciles that structure with source positions
/// using the same quote-respecting byte walks already proven in
/// `pocopine-core/src/templates.rs`.
fn map_positions(source: &str, roots: &mut [Node]) {
    let mut mapper = Mapper {
        source: source.as_bytes(),
        cursor: 0,
    };
    mapper.map_nodes(roots);
}

struct Mapper<'a> {
    source: &'a [u8],
    cursor: usize,
}

impl<'a> Mapper<'a> {
    fn map_nodes(&mut self, nodes: &mut [Node]) {
        for node in nodes {
            match node {
                Node::Element(e) if e.synthetic => {
                    // Synthetic elements weren't written by the
                    // author. Don't advance the source cursor
                    // for them — but recurse so their authored
                    // descendants still get mapped.
                    self.map_nodes(&mut e.children);
                }
                Node::Element(e) => self.map_element(e),
                Node::Text(_, _) | Node::Comment(_, _) => {
                    // v1: don't populate ranges on text/comment
                    // nodes. They're not used by RFC 045 / RFC 049
                    // diagnostics today. The cursor is advanced
                    // implicitly when we skip insignificant bytes
                    // before the next element.
                }
            }
        }
    }

    fn map_element(&mut self, el: &mut Element) {
        self.skip_insignificant_and_text();
        let open_lt = self.cursor;
        if open_lt >= self.source.len() || self.source[open_lt] != b'<' {
            return;
        }
        let Some(close_gt) = find_tag_end(self.source, open_lt) else {
            return;
        };

        // Verify the tag at `cursor` is actually this element's
        // opening. Foster-parenting / html5ever reparenting can
        // put tree-DFS order out of sync with source order; in
        // that case we'd otherwise claim someone else's range.
        //
        // Safer contract: if the source tag doesn't match, leave
        // ranges at 0..0 and don't advance the cursor. Callers
        // filtering by `byte_range != 0..0` get only elements
        // whose positions are trustworthy.
        if !source_tag_matches(self.source, open_lt, &el.tag) {
            return;
        }

        el.opening_tag_range = open_lt..close_gt + 1;
        self.cursor = close_gt + 1;

        let self_closing = close_gt > open_lt && self.source[close_gt - 1] == b'/';
        if self_closing || is_void_element(&el.tag) {
            el.byte_range = el.opening_tag_range.clone();
            return;
        }

        self.map_nodes(&mut el.children);

        // Find the matching `</tagname>` close. We've already
        // recursed through children, so any nested same-named
        // close tags are behind us; the next `</tagname>` is
        // ours.
        match find_close_tag(self.source, self.cursor, &el.tag) {
            Some(close_range) => {
                el.byte_range = open_lt..close_range.end;
                self.cursor = close_range.end;
            }
            None => {
                // Malformed-but-recovered input (html5ever
                // auto-closed at end of input). Best-effort
                // range: opening tag through wherever we are.
                el.byte_range = open_lt..self.cursor;
            }
        }
    }

    /// Advance past whitespace, comments, doctype, PI, and stray
    /// text content until we either hit end-of-source or an
    /// opening `<tagname` that corresponds to a real element.
    fn skip_insignificant_and_text(&mut self) {
        loop {
            while self.cursor < self.source.len()
                && self.source[self.cursor].is_ascii_whitespace()
            {
                self.cursor += 1;
            }
            if !self.peek_seq(b"<") {
                // Stray text content — advance until next `<`.
                while self.cursor < self.source.len()
                    && self.source[self.cursor] != b'<'
                {
                    self.cursor += 1;
                }
                if self.cursor >= self.source.len() {
                    return;
                }
            }
            if self.peek_seq(b"<!--") {
                match find_seq(self.source, self.cursor + 4, b"-->") {
                    Some(end) => self.cursor = end + 3,
                    None => return,
                }
                continue;
            }
            if self.peek_seq(b"<!") {
                match find_byte(self.source, self.cursor, b'>') {
                    Some(end) => self.cursor = end + 1,
                    None => return,
                }
                continue;
            }
            if self.peek_seq(b"<?") {
                match find_seq(self.source, self.cursor + 2, b"?>") {
                    Some(end) => self.cursor = end + 2,
                    None => return,
                }
                continue;
            }
            // If we see a closing tag here, we've reached the
            // end of our parent's children list — bail and let
            // the caller find the matching close.
            if self.peek_seq(b"</") {
                return;
            }
            // Opening tag of a child element — we're done
            // skipping.
            return;
        }
    }

    fn peek_seq(&self, needle: &[u8]) -> bool {
        self.cursor + needle.len() <= self.source.len()
            && &self.source[self.cursor..self.cursor + needle.len()] == needle
    }
}

// ── Byte-level helpers (mirrors of the ones in pocopine-core) ──

fn find_byte(bytes: &[u8], start: usize, needle: u8) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|&b| b == needle)
        .map(|p| start + p)
}

fn find_seq(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || start + needle.len() > bytes.len() {
        return None;
    }
    (start..=bytes.len() - needle.len())
        .find(|&i| &bytes[i..i + needle.len()] == needle)
}

/// Find the `>` that closes the opening tag starting at
/// `tag_start` (a `<`). Respects attribute-value quoting.
fn find_tag_end(bytes: &[u8], tag_start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = tag_start + 1;
    let mut quote: Option<u8> = None;
    while i < len {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(i),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Find the next `</tagname>` close tag starting at or after
/// `start` (case-insensitive tag match). Returns the byte range
/// of the entire close tag (`<` through `>` inclusive-exclusive).
fn find_close_tag(bytes: &[u8], start: usize, tag: &str) -> Option<Range<usize>> {
    let tag_bytes = tag.as_bytes();
    let mut i = start;
    while i + 2 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'/' {
            // Check tag name matches (case-insensitive).
            let name_start = i + 2;
            if name_start + tag_bytes.len() <= bytes.len() {
                let candidate = &bytes[name_start..name_start + tag_bytes.len()];
                let matches = candidate.iter().zip(tag_bytes).all(|(a, b)| {
                    a.to_ascii_lowercase() == b.to_ascii_lowercase()
                });
                if matches {
                    // Next byte should be whitespace or `>`.
                    let next = bytes.get(name_start + tag_bytes.len()).copied();
                    if matches!(next, Some(c) if c == b'>' || c.is_ascii_whitespace()) {
                        if let Some(gt) = find_byte(bytes, name_start, b'>') {
                            return Some(i..gt + 1);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// HTML void-element list — tags that implicitly self-close and
/// never carry a `</tag>` close. Kept in sync with the list in
/// `pocopine-core/src/templates.rs`.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
    "source", "track", "wbr",
];

fn is_void_element(tag: &str) -> bool {
    VOID_ELEMENTS.iter().any(|v| v.eq_ignore_ascii_case(tag))
}

/// Tags whose contents are parsed as foreign-namespace content
/// (SVG / MathML). Inside them, self-closing syntax is valid
/// for any element per HTML5; we skip self-close checks while
/// the walker's depth counter says we're inside one.
const FOREIGN_CONTENT_ROOTS: &[&str] = &["svg", "math"];

fn is_foreign_content_root(tag: &str) -> bool {
    FOREIGN_CONTENT_ROOTS.iter().any(|v| v.eq_ignore_ascii_case(tag))
}

/// RFC 050 explicit rule: `<tagname/>` self-close is only valid
/// for HTML void elements (`<br/>`, `<img/>`, etc.) and foreign-
/// content elements (anywhere inside `<svg>` / `<math>`). Using
/// it on a regular element or a pocopine pseudo-element
/// (`<slot/>`, `<root/>`, `<pine-foo/>`) is a framework-owned
/// parse error with an exact byte range.
///
/// Authors who want an empty element must write the explicit
/// close: `<slot></slot>` instead of `<slot/>`. All 241
/// `.poco` files already follow this convention (audited on
/// RFC 050 implementation) — this rule prevents regression
/// from a permissive parser interpretation.
fn detect_forbidden_self_close(source: &str, errors: &mut Vec<ParseError>) {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut foreign_depth: u32 = 0;

    while i < len {
        // Skip comments so `<!-- <slot/> -->` doesn't false-flag.
        if i + 4 <= len && &bytes[i..i + 4] == b"<!--" {
            match find_seq(bytes, i + 4, b"-->") {
                Some(end) => {
                    i = end + 3;
                    continue;
                }
                None => return,
            }
        }
        // Doctype / PI — skip.
        if i + 2 <= len && bytes[i] == b'<' && bytes[i + 1] == b'!' {
            match find_byte(bytes, i, b'>') {
                Some(end) => {
                    i = end + 1;
                    continue;
                }
                None => return,
            }
        }
        if i + 2 <= len && bytes[i] == b'<' && bytes[i + 1] == b'?' {
            match find_seq(bytes, i + 2, b"?>") {
                Some(end) => {
                    i = end + 2;
                    continue;
                }
                None => return,
            }
        }

        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Closing tag — track foreign-content exit, then skip.
        if i + 1 < len && bytes[i + 1] == b'/' {
            let name_start = i + 2;
            let name_end = read_tag_name_end(bytes, name_start);
            if let Ok(name) = std::str::from_utf8(&bytes[name_start..name_end]) {
                if is_foreign_content_root(name) && foreign_depth > 0 {
                    foreign_depth -= 1;
                }
            }
            i = find_byte(bytes, i, b'>').map(|e| e + 1).unwrap_or(i + 1);
            continue;
        }
        // Opening / self-closing tag.
        let name_start = i + 1;
        if name_start >= len || !bytes[name_start].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let name_end = read_tag_name_end(bytes, name_start);
        let tag = std::str::from_utf8(&bytes[name_start..name_end]).unwrap_or("");
        let Some(gt) = find_tag_end(bytes, i) else {
            return;
        };
        let self_closing = gt > i && bytes[gt - 1] == b'/';

        if self_closing && foreign_depth == 0 && !is_void_element(tag) {
            errors.push(ParseError {
                message: format!(
                    "self-closing syntax `<{tag}/>` is not allowed for `<{tag}>` in .poco templates — write `<{tag}></{tag}>` instead"
                ),
                byte_range: i..gt + 1,
            });
        }

        if !self_closing && is_foreign_content_root(tag) {
            foreign_depth += 1;
        }
        i = gt + 1;
    }
}

/// Read from `start` until a tag-delimiter byte (whitespace,
/// `/`, `>`) or end-of-input.
fn read_tag_name_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() {
        let b = bytes[end];
        if b.is_ascii_whitespace() || b == b'/' || b == b'>' {
            break;
        }
        end += 1;
    }
    end
}

/// Pick a fragment-parsing context element based on the
/// source's first real opening tag. See the call site in
/// `parse` for rationale.
fn detect_fragment_context(source: &str) -> QualName {
    let bytes = source.as_bytes();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'<' {
            break;
        }
        // Comment / doctype / PI — skip.
        if i + 4 <= bytes.len() && &bytes[i..i + 4] == b"<!--" {
            match find_seq(bytes, i + 4, b"-->") {
                Some(end) => {
                    i = end + 3;
                    continue;
                }
                None => break,
            }
        }
        if i + 2 <= bytes.len() && bytes[i + 1] == b'!' {
            match find_byte(bytes, i, b'>') {
                Some(end) => {
                    i = end + 1;
                    continue;
                }
                None => break,
            }
        }
        if i + 2 <= bytes.len() && bytes[i + 1] == b'?' {
            match find_seq(bytes, i + 2, b"?>") {
                Some(end) => {
                    i = end + 2;
                    continue;
                }
                None => break,
            }
        }
        break;
    }

    let default_ctx = || QualName::new(None, ns!(html), local_name!("div"));
    if i >= bytes.len() || bytes[i] != b'<' {
        return default_ctx();
    }
    let name_start = i + 1;
    let name_end = read_tag_name_end(bytes, name_start);
    if name_end <= name_start {
        return default_ctx();
    }
    let tag = std::str::from_utf8(&bytes[name_start..name_end])
        .unwrap_or("")
        .to_ascii_lowercase();
    match tag.as_str() {
        "tr" => QualName::new(None, ns!(html), local_name!("tbody")),
        "td" | "th" => QualName::new(None, ns!(html), local_name!("tr")),
        "tbody" | "thead" | "tfoot" | "caption" | "colgroup" => {
            QualName::new(None, ns!(html), local_name!("table"))
        }
        "col" => QualName::new(None, ns!(html), local_name!("colgroup")),
        _ => default_ctx(),
    }
}

/// `true` if the opening tag beginning at `open_lt` (a `<`)
/// names the given tag (case-insensitive). Used to sanity-check
/// the source-mapping pass before claiming a range.
fn source_tag_matches(bytes: &[u8], open_lt: usize, tag: &str) -> bool {
    let name_start = open_lt + 1;
    if name_start >= bytes.len() || bytes[open_lt] != b'<' {
        return false;
    }
    let name_end = read_tag_name_end(bytes, name_start);
    if name_end - name_start != tag.len() {
        return false;
    }
    let tag_bytes = tag.as_bytes();
    bytes[name_start..name_end]
        .iter()
        .zip(tag_bytes)
        .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
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
            template_contents,
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

            // `<template>` element contents live in a separate
            // document fragment at `template_contents`, not in
            // the normal `children` collection — that's how
            // HTML5 fragment parsing handles the "inert clone"
            // semantics of `<template>`. For structural checks
            // (RFC 049 `pp-slot` detection, etc.) we want to
            // treat those as if they were direct children.
            if let Some(contents) = template_contents.borrow().as_ref() {
                for child in contents.children.borrow().iter() {
                    if let Some(c) = convert_node(child, source, errors) {
                        children.push(c);
                    }
                }
            }

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
    fn parse_strict_only_rejects_framework_owned_errors() {
        // Duplicate-attr is an html5ever spec-recovery error
        // with no byte range in v1 — parse_strict tolerates it
        // (the tree is useful; downstream consumers decide
        // what to surface). Framework-owned errors like the
        // forbidden self-close DO fail.
        let dup_ok = parse_strict(r#"<div a="1" a="2">x</div>"#, "test.poco");
        assert!(
            dup_ok.is_ok(),
            "parse_strict tolerates html5ever recovery errors (no byte range)"
        );
        let self_close_err = parse_strict("<root><slot/></root>", "test.poco");
        assert!(
            self_close_err.is_err(),
            "parse_strict rejects framework-owned self-close error"
        );
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

    // ── Byte-range fidelity (RFC 050 §4.3) ────────────────────

    /// Helper: assert that the substring carved out by a range
    /// matches `expected` in the source.
    fn slice<'a>(src: &'a str, range: &Range<usize>) -> &'a str {
        &src[range.clone()]
    }

    #[test]
    fn byte_range_opening_tag_single_root() {
        let src = "<div>hi</div>";
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.opening_tag_range), "<div>");
        assert_eq!(slice(src, &root.byte_range), "<div>hi</div>");
    }

    #[test]
    fn byte_range_with_leading_whitespace_and_comment() {
        let src = "  <!-- hi -->\n<div>x</div>";
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.opening_tag_range), "<div>");
        assert_eq!(slice(src, &root.byte_range), "<div>x</div>");
    }

    #[test]
    fn byte_range_nested() {
        let src = "<div><span>inner</span></div>";
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.byte_range), "<div><span>inner</span></div>");
        let span = root.children[0].as_element().unwrap();
        assert_eq!(slice(src, &span.opening_tag_range), "<span>");
        assert_eq!(slice(src, &span.byte_range), "<span>inner</span>");
    }

    #[test]
    fn byte_range_self_closing() {
        let src = r#"<input type="text"/>"#;
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.opening_tag_range), r#"<input type="text"/>"#);
        // Self-closing: byte_range == opening_tag_range.
        assert_eq!(root.byte_range, root.opening_tag_range);
    }

    #[test]
    fn byte_range_void_element_without_slash() {
        let src = r#"<div><br>after</div>"#;
        let ast = parse_ok(src);
        let div = ast.element_roots().next().unwrap();
        let br = div.children[0].as_element().unwrap();
        assert_eq!(slice(src, &br.opening_tag_range), "<br>");
        assert_eq!(br.byte_range, br.opening_tag_range);
    }

    #[test]
    fn byte_range_attribute_with_gt_inside_quotes() {
        // `title="a > b"` — naïve byte walker would stop at the
        // inner `>`. The range must still span the real tag end.
        let src = r#"<div title="a > b">x</div>"#;
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(
            slice(src, &root.opening_tag_range),
            r#"<div title="a > b">"#
        );
    }

    #[test]
    fn byte_range_siblings() {
        let src = "<div>a</div><span>b</span>";
        let ast = parse_ok(src);
        let els: Vec<_> = ast.element_roots().collect();
        assert_eq!(slice(src, &els[0].byte_range), "<div>a</div>");
        assert_eq!(slice(src, &els[1].byte_range), "<span>b</span>");
    }

    #[test]
    fn byte_range_tags_across_line_boundaries() {
        let src = "<div\n  class=\"x\"\n  id=\"y\"\n>\n  inside\n</div>";
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.opening_tag_range), "<div\n  class=\"x\"\n  id=\"y\"\n>");
        assert_eq!(slice(src, &root.byte_range), src);
    }

    #[test]
    fn byte_range_multibyte_utf8_in_attribute() {
        let src = r#"<div title="café">x</div>"#;
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.opening_tag_range), r#"<div title="café">"#);
        assert_eq!(slice(src, &root.byte_range), r#"<div title="café">x</div>"#);
    }

    #[test]
    fn byte_range_multibyte_utf8_in_text() {
        let src = "<div>héllo café</div>";
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.byte_range), "<div>héllo café</div>");
    }

    #[test]
    fn byte_range_role_placeholder() {
        // The <root> placeholder element used by RFC 033 —
        // position tracking must work for unknown element names.
        //
        // Note: html5ever rejects `<slot/>` as an "unacknowledged
        // self-closing tag" because HTML5 only permits self-close
        // on void / foreign-content elements, not regular custom
        // elements. Pocopine's existing `.poco` templates use
        // `<slot/>` syntax — reconciling that is an open
        // question for the RFC 045 migration (source pre-pass
        // rewriting `<tag/>` → `<tag></tag>` for pocopine's
        // known pseudo-elements, or tightening the template
        // convention). Covered by the test matrix either way.
        let src = "<root class=\"x\"><slot></slot></root>";
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.opening_tag_range), "<root class=\"x\">");
        assert_eq!(slice(src, &root.byte_range), src);
    }

    #[test]
    fn synthetic_tbody_has_zero_range_but_authored_children_mapped() {
        // <tbody> was inserted by html5ever — its ranges stay
        // 0..0, but the authored <tr> / <td> inside it still
        // get their source positions.
        let src = "<table><tr><td>x</td></tr></table>";
        let ast = parse_ok(src);
        let table = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &table.byte_range), src);
        let tbody = table.children[0].as_element().unwrap();
        assert!(tbody.synthetic);
        assert_eq!(tbody.byte_range, 0..0);
        assert_eq!(tbody.opening_tag_range, 0..0);
        let tr = tbody.children[0].as_element().unwrap();
        assert!(!tr.synthetic);
        assert_eq!(slice(src, &tr.byte_range), "<tr><td>x</td></tr>");
    }

    #[test]
    fn byte_range_nested_same_tag_disambiguates() {
        // `<div><div></div></div>` — the outer div's close tag
        // is the *second* `</div>`. The mapper must not stop
        // at the inner one.
        let src = "<div><div></div></div>";
        let ast = parse_ok(src);
        let outer = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &outer.byte_range), src);
        let inner = outer.children[0].as_element().unwrap();
        assert_eq!(slice(src, &inner.byte_range), "<div></div>");
    }

    #[test]
    fn byte_range_crlf_line_endings() {
        // Tolerate Windows-style line endings.
        let src = "<div>\r\n  <span>x</span>\r\n</div>";
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.byte_range), src);
    }

    #[test]
    fn byte_range_unquoted_attribute_value() {
        let src = "<div class=x id=y>ok</div>";
        let ast = parse_ok(src);
        let root = ast.element_roots().next().unwrap();
        assert_eq!(slice(src, &root.opening_tag_range), "<div class=x id=y>");
        assert_eq!(slice(src, &root.byte_range), src);
    }

    // ── Foster-parenting / reparenting fixtures (Codex #1) ────

    #[test]
    fn foster_parented_element_keeps_ranges_safe() {
        // html5ever's spec-mandated recovery: a `<p>` inside
        // `<table>` but outside a table-cell is relocated to
        // **before** the `<table>` in the tree. The mapper's
        // DFS assumption breaks — the `<p>` is tree-first but
        // source-second (or vice-versa).
        //
        // Contract: when the source tag at the cursor doesn't
        // match the tree element's name, the mapper bails on
        // that element and leaves its ranges as 0..0. The AST
        // is never handed a bogus range that slices out of a
        // different tag. This is what makes byte ranges safe
        // for diagnostic renderers: a non-zero range is a
        // trustworthy range.
        let src = "<table><p>bad</p></table>";
        let (ast, _errors) = parse(src, "test.poco");

        // Every non-zero opening_tag_range slices a substring
        // whose first byte is `<` AND whose tag name actually
        // matches the element's tag. This is the invariant we
        // care about.
        fn check(el: &Element, src: &str) {
            if el.opening_tag_range != (0..0) {
                let text = &src[el.opening_tag_range.clone()];
                assert!(
                    text.starts_with(&format!("<{}", el.tag)[..])
                        || text
                            .to_ascii_lowercase()
                            .starts_with(&format!("<{}", el.tag)),
                    "mapped opening range {:?} for <{}> doesn't actually name <{}>: {:?}",
                    el.opening_tag_range,
                    el.tag,
                    el.tag,
                    text,
                );
            }
            for c in &el.children {
                if let Node::Element(child) = c {
                    check(child, src);
                }
            }
        }
        for node in &ast.roots {
            if let Node::Element(el) = node {
                check(el, src);
            }
        }
    }

    #[test]
    fn foster_parented_stray_text_emits_parser_error() {
        // "Non-space table text" is a spec-mandated html5ever
        // diagnostic — we surface it via `errors`, not drop
        // it. It comes without a byte range, so `parse_strict`
        // tolerates it per the framework-owned-error-only
        // policy; callers that want to be strict here inspect
        // `errors` from `parse` directly.
        let src = "<table>stray<tr><td>x</td></tr></table>";
        let (_ast, errors) = parse(src, "test.poco");
        assert!(
            !errors.is_empty(),
            "stray text in <table> must surface as a parse error"
        );
    }

    // ── Self-close rule (Codex #3) ───────────────────────────

    #[test]
    fn self_close_on_pocopine_pseudo_tag_is_rejected() {
        // `<slot/>` used to be incidentally rejected by
        // html5ever with an un-anchored "unacknowledged self-
        // closing tag" message. RFC 050 requires an explicit
        // framework-owned error with a byte range at the tag.
        let src = "<root><slot/></root>";
        let (_ast, errors) = parse(src, "test.poco");
        assert!(
            errors.iter().any(|e| e.message.contains("`<slot/>`")),
            "expected framework-owned self-close error, got: {errors:?}"
        );
        let err = errors
            .iter()
            .find(|e| e.message.contains("`<slot/>`"))
            .unwrap();
        // Byte range must point at the offending tag.
        assert_eq!(slice(src, &err.byte_range), "<slot/>");
    }

    #[test]
    fn self_close_on_custom_element_is_rejected() {
        let src = "<div><pine-foo/></div>";
        let (_ast, errors) = parse(src, "test.poco");
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("`<pine-foo/>`")),
            "expected self-close error on custom element, got: {errors:?}"
        );
    }

    #[test]
    fn self_close_on_void_element_is_allowed() {
        // `<br/>` / `<img/>` / `<input/>` etc. — HTML void
        // elements can self-close; don't flag.
        let src = r#"<div><br/><img src="x"/><input type="text"/></div>"#;
        let (_ast, errors) = parse(src, "test.poco");
        let self_close_errs: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("self-closing"))
            .collect();
        assert!(
            self_close_errs.is_empty(),
            "void-element self-close incorrectly flagged: {self_close_errs:?}"
        );
    }

    #[test]
    fn self_close_inside_svg_is_allowed() {
        // Inside `<svg>` the parser uses foreign-content rules
        // and self-close is valid for any element.
        let src = r#"<svg><circle r="5"/><rect/></svg>"#;
        let (_ast, errors) = parse(src, "test.poco");
        let self_close_errs: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("self-closing"))
            .collect();
        assert!(
            self_close_errs.is_empty(),
            "SVG content self-close incorrectly flagged: {self_close_errs:?}"
        );
    }

    #[test]
    fn self_close_in_comment_is_ignored() {
        // A literal `<slot/>` inside a comment must NOT be
        // flagged — it's not real markup.
        let src = "<!-- <slot/> --><div>x</div>";
        let (_ast, errors) = parse(src, "test.poco");
        assert!(
            errors.is_empty(),
            "comment-commented self-close incorrectly flagged: {errors:?}"
        );
    }

    #[test]
    fn parse_strict_rejects_forbidden_self_close() {
        let src = "<root><slot/></root>";
        let err = parse_strict(src, "test.poco").err();
        assert!(err.is_some(), "parse_strict must reject forbidden self-close");
    }

    // ── Table-rooted templates (Pine calendar regression) ───

    #[test]
    fn tr_as_root_single_element_root() {
        // `<tr class="foo"><slot></slot></tr>` is a real
        // PineCalendarGridRow template. html5ever's "in row"
        // insertion mode foster-parents the `<slot>` out of
        // the `<tr>` — it becomes a sibling at the tree root,
        // but with a `0..0` byte range because its source
        // position doesn't match the reparented location.
        // element_roots() filters on non-zero byte range, so
        // only the `<tr>` counts as a root.
        let src = "<tr class=\"foo\">\n  <slot></slot>\n</tr>\n";
        let (ast, _errors) = parse(src, "test.poco");
        assert_eq!(
            ast.element_roots().count(),
            1,
            "tr-rooted template must register as single-root"
        );
        let tr = ast.element_roots().next().unwrap();
        assert_eq!(tr.tag, "tr");
        assert_eq!(slice(src, &tr.opening_tag_range), "<tr class=\"foo\">");
    }

    #[test]
    fn th_as_root_single_element_root() {
        // PineCalendarHeadCell — `<th scope="col">`.
        let src = "<th scope=\"col\"><slot></slot></th>";
        let (ast, _errors) = parse(src, "test.poco");
        assert_eq!(ast.element_roots().count(), 1);
        assert_eq!(ast.element_roots().next().unwrap().tag, "th");
    }
}
