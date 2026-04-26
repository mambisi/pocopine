//! Inline `{{expr}}` text interpolation — RFC-025 as revised by RFC-040.
//!
//! After the walker finishes binding directives on an element, it
//! invokes [`scan_children`] to split each direct Text child into
//! static + dynamic segments. Each dynamic segment gets its own
//! text node bound by a reactive effect, pinned to the enclosing
//! element so release follows the existing unmount path.
//!
//! Double-brace form means single `{` / `}` in ordinary text is
//! always literal — code samples paste in untouched. Literal
//! `{{` / `}}` in text is rare but supported via backslash escape
//! (`\{{` → `{{`).
//!
//! Interpolation and `pp-text` are intentionally orthogonal:
//! `pp-text` owns the whole element, `{{expr}}` interpolates into
//! surrounding literal text. The scanner skips elements that carry
//! `pp-text` so the directive's `textContent` write isn't clobbered
//! by interpolated siblings.

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{console, Element, Node, Text};

use crate::expr::{self, Spanned};
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::track_effect_on;

enum Segment {
    Static(String),
    Dynamic(String),
}

/// Visit `parent`'s direct text children. For any that contain at
/// least one `{{…}}` pair, split into static + dynamic text nodes
/// and install an effect per dynamic segment.
pub fn scan_children(parent: &Element, proxy: &JsValue) {
    // `pp-text` takes over the element's content — don't split
    // children the directive is about to overwrite.
    if parent.has_attribute("pp-text") {
        return;
    }
    // RFC-058 Phase 2 — `data-pp-text-managed` is the macro's
    // marker for "this element used to carry `pp-text` but the
    // attribute was stripped at compile time and the static
    // template plan owns the textContent now". Same skip
    // semantics as the `pp-text` check above so braces inside
    // a planned text value don't get hijacked by interpolation.
    if parent.has_attribute("data-pp-text-managed") {
        return;
    }

    // Snapshot the child node list first — splitting inserts new
    // siblings, which would invalidate a live NodeList.
    let nodes = parent.child_nodes();
    let mut texts: Vec<Text> = Vec::new();
    for i in 0..nodes.length() {
        if let Some(n) = nodes.item(i) {
            if n.node_type() == Node::TEXT_NODE {
                if let Ok(t) = n.dyn_into::<Text>() {
                    texts.push(t);
                }
            }
        }
    }

    for text in texts {
        let Some(data) = text.node_value() else {
            continue;
        };
        // Fast path: no `{{` anywhere means nothing to interpolate.
        // A single `{` never triggers the scanner — unambiguous
        // per RFC-040.
        if !data.contains("{{") {
            continue;
        }
        let segments = match parse_segments(&data) {
            Ok(s) => s,
            Err(err) => {
                console::error_1(&JsValue::from_str(&format!(
                    "text interpolation: {err} in {data:?}"
                )));
                continue;
            }
        };
        if segments.iter().all(|s| matches!(s, Segment::Static(_))) {
            // No `{{…}}` survived escaping; leave the text node
            // untouched so whitespace/entities stay byte-exact.
            continue;
        }
        install(parent, proxy, &text, segments);
    }
}

fn install(parent: &Element, proxy: &JsValue, original: &Text, segments: Vec<Segment>) {
    let parent_node: &Node = parent.as_ref();
    for seg in segments {
        match seg {
            Segment::Static(s) => {
                let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                    return;
                };
                let node = doc.create_text_node(&s);
                let _ = parent_node.insert_before(node.as_ref(), Some(original.as_ref()));
            }
            Segment::Dynamic(src) => {
                let ast: Spanned<expr::Expr> = match expr::parse_cached(&src) {
                    Ok(a) => a,
                    Err(e) => {
                        console::error_1(&JsValue::from_str(&format!(
                            "interpolation `{{{{{src}}}}}`: {} (at {}..{})",
                            e.message, e.span.start, e.span.end
                        )));
                        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                            return;
                        };
                        let fallback = doc.create_text_node(&format!("{{{{{src}}}}}"));
                        let _ =
                            parent_node.insert_before(fallback.as_ref(), Some(original.as_ref()));
                        continue;
                    }
                };
                let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                    return;
                };
                let node = doc.create_text_node("");
                let _ = parent_node.insert_before(node.as_ref(), Some(original.as_ref()));

                let proxy = proxy.clone();
                let node_clone = node.clone();
                let el_for_magic = parent.clone();
                let id = effect(move || {
                    with_current_el(&el_for_magic, || {
                        let v = expr::evaluate(&ast, &proxy);
                        node_clone.set_data(&js_to_string(&v));
                    });
                });
                track_effect_on(parent, id);
            }
        }
    }
    // Remove the original un-split text node.
    let _ = parent_node.remove_child(original.as_ref());
}

/// Tokenise `input` into alternating static + dynamic segments.
/// Scans for `{{…}}` pairs; single `{` / `}` are always literal.
/// Returns an error string on an unclosed `{{`.
fn parse_segments(input: &str) -> Result<Vec<Segment>, String> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut static_buf = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        // Escape: `\{{` → literal `{{`, `\}}` → literal `}}`,
        // `\\` → literal `\`. Any other `\<c>` passes through
        // verbatim as `\<c>` (don't surprise authors who paste
        // regex / shell samples that happen to start with `\`).
        if b == b'\\' && i + 2 < bytes.len() {
            let n1 = bytes[i + 1];
            let n2 = bytes[i + 2];
            if (n1 == b'{' && n2 == b'{') || (n1 == b'}' && n2 == b'}') {
                static_buf.push(n1 as char);
                static_buf.push(n2 as char);
                i += 3;
                continue;
            }
        }
        if b == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            static_buf.push('\\');
            i += 2;
            continue;
        }
        // `{{` opens an expression.
        if b == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if !static_buf.is_empty() {
                out.push(Segment::Static(std::mem::take(&mut static_buf)));
            }
            let start = i + 2;
            // Find the matching `}}`.
            let mut j = start;
            let mut found = false;
            while j + 1 < bytes.len() {
                if bytes[j] == b'}' && bytes[j + 1] == b'}' {
                    found = true;
                    break;
                }
                j += 1;
            }
            if !found {
                return Err("unclosed `{{` in text".into());
            }
            let src = std::str::from_utf8(&bytes[start..j])
                .map_err(|_| "non-UTF-8 text")?
                .trim()
                .to_string();
            if src.is_empty() {
                return Err("empty `{{}}` interpolation".into());
            }
            out.push(Segment::Dynamic(src));
            i = j + 2;
            continue;
        }
        // Single `{` or `}` are literal in text.
        static_buf.push(b as char);
        i += 1;
    }
    if !static_buf.is_empty() {
        out.push(Segment::Static(static_buf));
    }
    Ok(out)
}

fn js_to_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(|n| n.to_string()))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| {
            js_sys::JSON::stringify(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::{parse_segments, Segment};

    fn render(segs: &[Segment]) -> String {
        let mut s = String::new();
        for seg in segs {
            match seg {
                Segment::Static(t) => s.push_str(t),
                Segment::Dynamic(t) => s.push_str(&format!("<<{t}>>")),
            }
        }
        s
    }

    #[test]
    fn single_braces_are_literal() {
        let segs = parse_segments("Rust: fn foo() { x }").unwrap();
        assert_eq!(render(&segs), "Rust: fn foo() { x }");
        assert!(segs.iter().all(|s| matches!(s, Segment::Static(_))));
    }

    #[test]
    fn double_braces_interpolate() {
        let segs = parse_segments("Hi {{name}}, {{count}}!").unwrap();
        assert_eq!(render(&segs), "Hi <<name>>, <<count>>!");
    }

    #[test]
    fn escaped_double_brace_is_literal() {
        let segs = parse_segments(r"\{{literal}}").unwrap();
        assert_eq!(render(&segs), "{{literal}}");
    }

    #[test]
    fn unclosed_errors() {
        assert!(parse_segments("oops {{foo").is_err());
    }

    #[test]
    fn empty_errors() {
        assert!(parse_segments("{{}}").is_err());
    }

    #[test]
    fn mixed_with_code_samples() {
        let src = "let x = { 1 }; template {{expr}}";
        let segs = parse_segments(src).unwrap();
        assert_eq!(render(&segs), "let x = { 1 }; template <<expr>>");
    }
}
