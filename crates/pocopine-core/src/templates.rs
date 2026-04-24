//! Component template registry.
//!
//! The `#[component]` macro emits a call to [`register_template`] with the
//! component's compiled HTML. The macro pipes the raw `.poco` contents
//! through [`inject_pp_data`] so the walker can recognise the template
//! root by its `pp-data` attribute without authors having to type one.

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static TEMPLATES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Register a component's rewritten template under its runtime name.
pub fn register_template(name: impl Into<String>, html: impl Into<String>) {
    TEMPLATES.with(|t| t.borrow_mut().insert(name.into(), html.into()));
}

/// Fetch a registered template. Returns an owned `String` so the walker can
/// clone into the DOM without locking the registry.
pub fn template_for(name: &str) -> Option<String> {
    TEMPLATES.with(|t| t.borrow().get(name).cloned())
}

pub fn is_registered(name: &str) -> bool {
    TEMPLATES.with(|t| t.borrow().contains_key(name))
}

/// Full template compilation entry-point for the macro.
///
/// - `role = None` → just injects `pp-data` (the classic path).
/// - `role = Some((tag, role_name))` → treats the template root as a
///   `<root>` placeholder (see [RFC-033](../../rfcs/rfc-033-primitive-roles.md)).
///   Rewrites `<root>` / `</root>` to `<tag>` / `</tag>`, splicing
///   `data-pine-role="<role_name>"` into the rewritten opening tag
///   (so the role attribute lands on the primitive's real root even
///   when it's wrapped in `<template pp-if="...">`). For `tag == "button"`
///   and no existing `type` attribute, also injects `type="button"`.
///   Finally runs [`inject_pp_data`] so `pp-data` lands on the outermost
///   opening tag as usual.
pub fn compile_template(raw: &str, name: &str, role: Option<(&str, &str)>) -> String {
    let Some((tag, role_name)) = role else {
        return inject_pp_data(raw, name);
    };
    let mut prefix = format!(r#"data-pine-role="{role_name}""#);
    if tag == "button" && !root_placeholder_has_attr(raw, "type") {
        prefix.push_str(r#" type="button""#);
    }
    let renamed = rewrite_root_placeholder(raw, tag, &prefix);
    inject_pp_data(&renamed, name)
}

/// Rewrite `<root>` / `<root ...>` / `<root/>` to `<tag ... prefix_attrs>`
/// and `</root>` to `</tag>`. `root` isn't a real HTML element, so
/// every occurrence in a `.poco` file is unambiguously the placeholder.
fn rewrite_root_placeholder(raw: &str, tag: &str, prefix_attrs: &str) -> String {
    let attrs = prefix_attrs.trim();
    let step1 = raw.replace("<root>", &format!("<{tag} {attrs}>"));
    let step2 = step1.replace("<root ", &format!("<{tag} {attrs} "));
    let step3 = step2.replace("<root/>", &format!("<{tag} {attrs}/>"));
    step3.replace("</root>", &format!("</{tag}>"))
}

/// True when the `<root>` placeholder tag has an attribute named
/// `needle` (case-insensitive). Only looks inside the placeholder's
/// own opening tag — ignores siblings and children.
fn root_placeholder_has_attr(raw: &str, needle: &str) -> bool {
    let Some(pos) = raw.find("<root") else {
        return false;
    };
    let after = pos + "<root".len();
    let boundary = raw.as_bytes().get(after).copied();
    if !matches!(
        boundary,
        Some(b' ') | Some(b'>') | Some(b'/') | Some(b'\n') | Some(b'\t') | Some(b'\r')
    ) {
        return false;
    }
    let bytes = raw.as_bytes();
    let Some(close) = find_tag_end(bytes, pos) else {
        return false;
    };
    let tag_slice = &raw[pos + 1..close];
    for chunk in tag_slice.split_ascii_whitespace().skip(1) {
        let name_end = chunk.find('=').unwrap_or(chunk.len());
        if chunk[..name_end].eq_ignore_ascii_case(needle) {
            return true;
        }
    }
    false
}

/// Insert `pp-data="<name>"` into the first element's opening tag of
/// `raw`. The caller guarantees the template has a real element root;
/// comments, doctypes, and leading whitespace are skipped.
///
/// The parser is deliberately minimal — enough for plain HTML-with-
/// directives as authored in `.poco` files. A full HTML parser is
/// overkill for a compile-time rewrite of author-controlled input.
pub fn inject_pp_data(raw: &str, name: &str) -> String {
    // Walk to the first opening tag, skipping comments / doctypes.
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Skip whitespace between chunks.
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        if bytes[i] != b'<' {
            // Stray text before the root — emit as-is and give up on
            // rewriting. The walker will fail gracefully when it can't
            // find `pp-data`.
            return raw.to_owned();
        }
        // `<!--` comment
        if i + 4 <= len && &bytes[i..i + 4] == b"<!--" {
            if let Some(end) = find_seq(bytes, i + 4, b"-->") {
                i = end + 3;
                continue;
            }
            return raw.to_owned();
        }
        // `<!DOCTYPE ...>`
        if i + 2 <= len && bytes[i + 1] == b'!' {
            if let Some(end) = find_byte(bytes, i, b'>') {
                i = end + 1;
                continue;
            }
            return raw.to_owned();
        }
        // `<?xml ... ?>`
        if i + 2 <= len && bytes[i + 1] == b'?' {
            if let Some(end) = find_seq(bytes, i + 2, b"?>") {
                i = end + 2;
                continue;
            }
            return raw.to_owned();
        }
        // A real opening tag. Find its end (`>` or `/>`), respecting
        // attribute-value quoting.
        let Some(close) = find_tag_end(bytes, i) else {
            return raw.to_owned();
        };
        // Splice ` pp-data="<name>"` before the closing char(s).
        // If the tag is self-closing (`<foo />`), keep the `/>`.
        let self_closing = close > 0 && bytes[close - 1] == b'/';
        let insert_at = if self_closing { close - 1 } else { close };
        let attr = format!(" pp-data=\"{name}\"");
        let mut out = String::with_capacity(raw.len() + attr.len());
        out.push_str(&raw[..insert_at]);
        // Ensure exactly one space before the attribute.
        if !out.ends_with(char::is_whitespace) {
            out.push(' ');
        }
        out.push_str(attr.trim_start());
        out.push_str(&raw[insert_at..]);
        return out;
    }
    raw.to_owned()
}

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
    (start..=bytes.len() - needle.len()).find(|&i| &bytes[i..i + needle.len()] == needle)
}

/// Find the index of `>` (or `/` in `/>`) that closes the opening tag
/// starting at `tag_start` (a `<`). Respects attribute value quoting so
/// a `>` inside `title="a > b"` isn't mistaken for the end.
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

/// Result of checking whether a raw `.poco` template has exactly one
/// element root (RFC 045). The two failure cases are genuinely
/// different bugs with different fixes, so the macro layer renders a
/// different panic message for each.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RootCheck {
    /// Exactly one element root.
    Ok = 0,
    /// Zero element roots — empty, comment-only, or stray text.
    Missing = 1,
    /// First root parses fine, but sibling content follows it.
    Multiple = 2,
}

// Const-fn variants of the byte helpers above. The non-const versions
// use iterator methods which aren't usable in a `const fn` context yet.
const fn const_find_byte(bytes: &[u8], start: usize, needle: u8) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

const fn const_find_seq(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || start + needle.len() > bytes.len() {
        return None;
    }
    let end = bytes.len() - needle.len();
    let mut i = start;
    'outer: while i <= end {
        let mut j = 0;
        while j < needle.len() {
            if bytes[i + j] != needle[j] {
                i += 1;
                continue 'outer;
            }
            j += 1;
        }
        return Some(i);
        // unreachable tail — the `continue 'outer` above advances `i`.
    }
    None
}

const fn const_find_tag_end(bytes: &[u8], tag_start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = tag_start + 1;
    let mut quote: u8 = 0;
    while i < len {
        let b = bytes[i];
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'"' || b == b'\'' {
            quote = b;
        } else if b == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

const fn is_ascii_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
}

const fn ascii_eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        let mut ca = a[i];
        let mut cb = b[i];
        if ca.is_ascii_uppercase() {
            ca += 32;
        }
        if cb.is_ascii_uppercase() {
            cb += 32;
        }
        if ca != cb {
            return false;
        }
        i += 1;
    }
    true
}

/// HTML void elements — implicitly self-closing even when authored
/// without `/>` (e.g. `<br>`, `<img src="...">`). Depth tracking in
/// [`check_single_root`] treats these as balanced without a matching
/// `</tag>`.
const VOID_ELEMENTS: &[&[u8]] = &[
    b"area", b"base", b"br", b"col", b"embed", b"hr", b"img", b"input", b"link", b"meta",
    b"source", b"track", b"wbr",
];

const fn is_void_tag(name: &[u8]) -> bool {
    let mut i = 0;
    while i < VOID_ELEMENTS.len() {
        if ascii_eq_ignore_case(VOID_ELEMENTS[i], name) {
            return true;
        }
        i += 1;
    }
    false
}

/// Return the tag name starting at `tag_start + 1` (or `tag_start + 2`
/// for closing tags). Stops at whitespace, `/`, `>`, or end-of-input.
const fn read_tag_name(bytes: &[u8], start: usize) -> &[u8] {
    let mut end = start;
    while end < bytes.len() {
        let b = bytes[end];
        if is_ascii_ws(b) || b == b'/' || b == b'>' {
            break;
        }
        end += 1;
    }
    // `bytes.get(start..end)` isn't const; slice directly.
    let (_, rest) = bytes.split_at(start);
    let (name, _) = rest.split_at(end - start);
    name
}

/// Compile-time validator for the RFC-045 single-root rule. Returns
/// [`RootCheck::Ok`] iff `raw` — after skipping leading whitespace,
/// comments, doctype, and XML PIs — contains exactly one element
/// whose opening / body / closing tags span the remainder, tolerating
/// only trailing whitespace and comments.
///
/// Designed to run in `const` context so the `#[component]` macro
/// can surface failures as `rustc` compile errors.
pub const fn check_single_root(raw: &str) -> RootCheck {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    // ── Phase 1: skip leading noise, find root's opening `<`. ──
    loop {
        while i < len && is_ascii_ws(bytes[i]) {
            i += 1;
        }
        if i >= len {
            return RootCheck::Missing;
        }
        if bytes[i] != b'<' {
            // Stray text before the first element.
            return RootCheck::Missing;
        }
        // Comment: `<!-- ... -->`
        if i + 4 <= len
            && bytes[i + 1] == b'!'
            && bytes[i + 2] == b'-'
            && bytes[i + 3] == b'-'
        {
            match const_find_seq(bytes, i + 4, b"-->") {
                Some(p) => {
                    i = p + 3;
                    continue;
                }
                None => return RootCheck::Missing,
            }
        }
        // Doctype / other `<!...>`
        if i + 2 <= len && bytes[i + 1] == b'!' {
            match const_find_byte(bytes, i, b'>') {
                Some(p) => {
                    i = p + 1;
                    continue;
                }
                None => return RootCheck::Missing,
            }
        }
        // PI: `<? ... ?>`
        if i + 2 <= len && bytes[i + 1] == b'?' {
            match const_find_seq(bytes, i + 2, b"?>") {
                Some(p) => {
                    i = p + 2;
                    continue;
                }
                None => return RootCheck::Missing,
            }
        }
        // Real opening tag (but reject a stray `</...>` before any open).
        if i + 1 < len && bytes[i + 1] == b'/' {
            return RootCheck::Missing;
        }
        break;
    }

    // ── Phase 2: walk the root element until depth returns to zero. ──
    let mut depth: i32 = 0;
    while i < len {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Comment inside / around the root.
        if i + 4 <= len
            && bytes[i + 1] == b'!'
            && bytes[i + 2] == b'-'
            && bytes[i + 3] == b'-'
        {
            match const_find_seq(bytes, i + 4, b"-->") {
                Some(p) => {
                    i = p + 3;
                    continue;
                }
                None => return RootCheck::Missing,
            }
        }
        // Doctype / PI mid-root — tolerate by skipping.
        if i + 2 <= len && bytes[i + 1] == b'!' {
            match const_find_byte(bytes, i, b'>') {
                Some(p) => {
                    i = p + 1;
                    continue;
                }
                None => return RootCheck::Missing,
            }
        }
        if i + 2 <= len && bytes[i + 1] == b'?' {
            match const_find_seq(bytes, i + 2, b"?>") {
                Some(p) => {
                    i = p + 2;
                    continue;
                }
                None => return RootCheck::Missing,
            }
        }
        // Closing tag `</...>`.
        if i + 1 < len && bytes[i + 1] == b'/' {
            match const_find_tag_end(bytes, i) {
                Some(end) => {
                    depth -= 1;
                    i = end + 1;
                    if depth == 0 {
                        break;
                    }
                    if depth < 0 {
                        return RootCheck::Missing;
                    }
                    continue;
                }
                None => return RootCheck::Missing,
            }
        }
        // Opening / self-closing tag.
        let name = read_tag_name(bytes, i + 1);
        if name.is_empty() {
            return RootCheck::Missing;
        }
        match const_find_tag_end(bytes, i) {
            Some(end) => {
                let self_closing = end > 0 && bytes[end - 1] == b'/';
                if !self_closing && !is_void_tag(name) {
                    depth += 1;
                }
                i = end + 1;
                // A self-closing / void tag as the root completes
                // depth immediately — no explicit close follows.
                if depth == 0 {
                    break;
                }
            }
            None => return RootCheck::Missing,
        }
    }

    if depth != 0 {
        return RootCheck::Missing;
    }

    // ── Phase 3: trailing noise must be whitespace or comments only. ──
    while i < len {
        while i < len && is_ascii_ws(bytes[i]) {
            i += 1;
        }
        if i >= len {
            return RootCheck::Ok;
        }
        if bytes[i] != b'<' {
            return RootCheck::Multiple;
        }
        if i + 4 <= len
            && bytes[i + 1] == b'!'
            && bytes[i + 2] == b'-'
            && bytes[i + 3] == b'-'
        {
            match const_find_seq(bytes, i + 4, b"-->") {
                Some(p) => {
                    i = p + 3;
                    continue;
                }
                None => return RootCheck::Multiple,
            }
        }
        // Anything else (element, doctype, PI) = second root.
        return RootCheck::Multiple;
    }
    RootCheck::Ok
}

#[cfg(test)]
mod tests {
    use super::{check_single_root, compile_template, inject_pp_data, RootCheck};

    #[test]
    fn basic_root_gets_attr() {
        let out = inject_pp_data("<div>hi</div>", "counter");
        assert_eq!(out, r#"<div pp-data="counter">hi</div>"#);
    }

    #[test]
    fn preserves_existing_attrs() {
        let out = inject_pp_data("<div class=\"x\" pp-init=\"init\">hi</div>", "counter");
        assert_eq!(
            out,
            r#"<div class="x" pp-init="init" pp-data="counter">hi</div>"#
        );
    }

    #[test]
    fn handles_self_closing_root() {
        let out = inject_pp_data("<input type=\"text\" />", "foo");
        assert_eq!(out, r#"<input type="text" pp-data="foo"/>"#);
    }

    #[test]
    fn skips_leading_comments() {
        let out = inject_pp_data("<!-- hello --><div>x</div>", "x");
        assert_eq!(out, r#"<!-- hello --><div pp-data="x">x</div>"#);
    }

    #[test]
    fn tolerates_gt_in_attr_value() {
        let out = inject_pp_data("<div title=\"a > b\">x</div>", "n");
        assert_eq!(out, r#"<div title="a > b" pp-data="n">x</div>"#);
    }

    // ── compile_template + role rewriting ─────────────────────────

    #[test]
    fn compile_template_no_role_matches_inject_pp_data() {
        let out = compile_template("<div>hi</div>", "c", None);
        assert_eq!(out, r#"<div pp-data="c">hi</div>"#);
    }

    #[test]
    fn role_visual_rewrites_root_to_span() {
        let out = compile_template(
            "<root class=\"pine-avatar-root\"><slot></slot></root>",
            "pine-avatar-root",
            Some(("span", "visual")),
        );
        assert_eq!(
            out,
            r#"<span data-pine-role="visual" class="pine-avatar-root" pp-data="pine-avatar-root"><slot></slot></span>"#
        );
    }

    #[test]
    fn role_interactive_injects_type_button() {
        let out = compile_template(
            "<root class=\"pine-switch\"><slot/></root>",
            "pine-switch",
            Some(("button", "interactive")),
        );
        assert!(out.starts_with(
            r#"<button data-pine-role="interactive" type="button" class="pine-switch""#
        ));
        assert!(out.ends_with("</button>"));
    }

    #[test]
    fn role_interactive_respects_existing_type() {
        let out = compile_template(
            "<root type=\"submit\" class=\"x\"><slot/></root>",
            "c",
            Some(("button", "interactive")),
        );
        // Only one `type=` should appear — the original submit wins.
        assert_eq!(out.matches("type=").count(), 1);
        assert!(out.contains(r#"type="submit""#));
    }

    #[test]
    fn role_panel_rewrites_to_div() {
        let out = compile_template("<root><slot/></root>", "p", Some(("div", "panel")));
        assert_eq!(
            out,
            r#"<div data-pine-role="panel" pp-data="p"><slot/></div>"#
        );
    }

    #[test]
    fn role_with_self_closing_root() {
        // Self-closing placeholder: `<root/>` → `<img .../>`.
        let out = compile_template(
            "<root :src=\"src\"/>",
            "pine-avatar-image",
            Some(("img", "media")),
        );
        assert!(out.contains(r#"data-pine-role="media""#));
        assert!(out.contains(r#"pp-data="pine-avatar-image""#));
        assert!(out.ends_with("/>"));
    }

    // ── RFC 045 — single-root validator ───────────────────────────

    #[test]
    fn single_root_basic() {
        assert_eq!(check_single_root("<div>hi</div>"), RootCheck::Ok);
    }

    #[test]
    fn single_root_with_role_placeholder() {
        assert_eq!(
            check_single_root("<root class=\"x\"><slot></slot></root>"),
            RootCheck::Ok
        );
    }

    #[test]
    fn single_root_self_closing() {
        assert_eq!(
            check_single_root("<input type=\"text\" />"),
            RootCheck::Ok
        );
    }

    #[test]
    fn single_root_void_element() {
        // `<br>` has no `/>` and no close tag — must not blow the
        // depth counter.
        assert_eq!(check_single_root("<img src=\"x.png\">"), RootCheck::Ok);
    }

    #[test]
    fn void_element_inside_root_does_not_inflate_depth() {
        assert_eq!(
            check_single_root("<div><br>hello<img src=\"x\"></div>"),
            RootCheck::Ok
        );
    }

    #[test]
    fn single_root_leading_comment_and_doctype() {
        assert_eq!(
            check_single_root(
                "<!-- hello --> \n <!DOCTYPE html>\n<div>x</div>"
            ),
            RootCheck::Ok
        );
    }

    #[test]
    fn single_root_leading_pi() {
        assert_eq!(
            check_single_root("<?xml version=\"1.0\"?><svg></svg>"),
            RootCheck::Ok
        );
    }

    #[test]
    fn single_root_trailing_whitespace_and_comment() {
        assert_eq!(
            check_single_root("<div>x</div>\n  <!-- trailing -->  \n"),
            RootCheck::Ok
        );
    }

    #[test]
    fn single_root_nested() {
        assert_eq!(
            check_single_root(
                "<div><span><b>deep</b></span><br><p>ok</p></div>"
            ),
            RootCheck::Ok
        );
    }

    #[test]
    fn single_root_gt_inside_quoted_attr() {
        assert_eq!(
            check_single_root("<div title=\"a > b\">x</div>"),
            RootCheck::Ok
        );
    }

    #[test]
    fn single_root_unquoted_attr_value() {
        // HTML allows `<div class=foo>`; the validator must not crash
        // or mis-count on these.
        assert_eq!(
            check_single_root("<div class=foo id=bar>x</div>"),
            RootCheck::Ok
        );
    }

    #[test]
    fn missing_on_empty() {
        assert_eq!(check_single_root(""), RootCheck::Missing);
    }

    #[test]
    fn missing_on_whitespace_only() {
        assert_eq!(check_single_root("   \n\t\n  "), RootCheck::Missing);
    }

    #[test]
    fn missing_on_comment_only() {
        assert_eq!(
            check_single_root("<!-- nothing here -->"),
            RootCheck::Missing
        );
    }

    #[test]
    fn missing_on_stray_leading_text() {
        assert_eq!(
            check_single_root("oops<div>x</div>"),
            RootCheck::Missing
        );
    }

    #[test]
    fn missing_on_unbalanced_root() {
        assert_eq!(check_single_root("<div>x"), RootCheck::Missing);
    }

    #[test]
    fn missing_on_stray_closing_tag() {
        assert_eq!(check_single_root("</div>"), RootCheck::Missing);
    }

    #[test]
    fn multiple_on_two_sibling_roots() {
        assert_eq!(
            check_single_root("<div>a</div><div>b</div>"),
            RootCheck::Multiple
        );
    }

    #[test]
    fn multiple_on_root_then_self_closing_sibling() {
        assert_eq!(
            check_single_root("<div>a</div><br>"),
            RootCheck::Multiple
        );
    }

    #[test]
    fn multiple_on_self_closing_root_then_sibling() {
        assert_eq!(
            check_single_root("<img src=\"x\"/><div>second</div>"),
            RootCheck::Multiple
        );
    }

    #[test]
    fn multiple_on_trailing_text() {
        assert_eq!(
            check_single_root("<div>a</div>stray"),
            RootCheck::Multiple
        );
    }

    #[test]
    fn multiple_on_comment_then_second_root() {
        assert_eq!(
            check_single_root("<div>a</div>\n<!-- ok -->\n<span>b</span>"),
            RootCheck::Multiple
        );
    }

    // Verify the validator works in a `const` context — this is the
    // whole point of `pub const fn`: if the body ever regresses to
    // non-const primitives, this line stops compiling.
    const _: () = {
        let _ = check_single_root("<div>hi</div>");
    };
}
