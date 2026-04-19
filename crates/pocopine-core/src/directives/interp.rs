//! Inline `{expr}` text interpolation — RFC-025.
//!
//! After the walker finishes binding directives on an element, it
//! invokes [`scan_children`] to split each direct Text child into
//! static + dynamic segments. Each dynamic segment gets its own
//! text node bound by a reactive effect, pinned to the enclosing
//! element so release follows the existing unmount path.
//!
//! The scanner is a plain UTF-8 pass with escape handling (`\{`,
//! `\}`, `\\`). Malformed segments fall back to raw text and log
//! once; one bad segment doesn't kill a text node's siblings.
//!
//! Interpolation and `pp-text` are intentionally orthogonal:
//! `pp-text` owns the whole element, `{expr}` interpolates into
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
/// least one `{…}` pair, split into static + dynamic text nodes
/// and install an effect per dynamic segment.
pub fn scan_children(parent: &Element, proxy: &JsValue) {
    // `pp-text` takes over the element's content — don't split
    // children the directive is about to overwrite.
    if parent.has_attribute("pp-text") {
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
        let Some(data) = text.node_value() else { continue };
        if !data.contains('{') {
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
            // No `{…}` survived escaping; leave the text node
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
                let ast: Spanned<expr::Expr> = match expr::parse(&src) {
                    Ok(a) => a,
                    Err(e) => {
                        console::error_1(&JsValue::from_str(&format!(
                            "interpolation `{{{src}}}`: {} (at {}..{})",
                            e.message, e.span.start, e.span.end
                        )));
                        // Fall back: render the original `{…}` text
                        // so the author sees where the bad segment
                        // lives.
                        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                            return;
                        };
                        let fallback = doc.create_text_node(&format!("{{{src}}}"));
                        let _ = parent_node
                            .insert_before(fallback.as_ref(), Some(original.as_ref()));
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
/// Returns an error string on an unmatched `{`.
fn parse_segments(input: &str) -> Result<Vec<Segment>, String> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut static_buf = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            match next {
                b'{' | b'}' | b'\\' => {
                    static_buf.push(next as char);
                    i += 2;
                    continue;
                }
                _ => {
                    // Pass through unknown escape verbatim.
                    static_buf.push(b as char);
                    i += 1;
                    continue;
                }
            }
        }
        if b == b'{' {
            if !static_buf.is_empty() {
                out.push(Segment::Static(std::mem::take(&mut static_buf)));
            }
            // Find matching `}`. No nesting.
            let start = i + 1;
            let mut j = start;
            let mut found = false;
            while j < bytes.len() {
                let bj = bytes[j];
                if bj == b'\\' && j + 1 < bytes.len() {
                    j += 2;
                    continue;
                }
                if bj == b'}' {
                    found = true;
                    break;
                }
                if bj == b'{' {
                    return Err("nested `{` inside interpolation".into());
                }
                j += 1;
            }
            if !found {
                return Err("unclosed `{` in text".into());
            }
            let src = std::str::from_utf8(&bytes[start..j])
                .map_err(|_| "non-UTF-8 text")?
                .to_string();
            out.push(Segment::Dynamic(src));
            i = j + 1;
            continue;
        }
        if b == b'}' {
            return Err("stray `}` without matching `{`".into());
        }
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
