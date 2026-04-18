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
    bytes[start..].iter().position(|&b| b == needle).map(|p| start + p)
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

#[cfg(test)]
mod tests {
    use super::inject_pp_data;

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
}
