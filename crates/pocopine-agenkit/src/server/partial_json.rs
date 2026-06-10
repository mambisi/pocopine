//! Best-effort partial-JSON parsing for streaming structured output (§D8).
//!
//! As a model streams the JSON of a structured result, this parses the prefix
//! accumulated so far into the most complete value it can represent — closing
//! incomplete value strings, dropping incomplete keys / dangling `key:` pairs /
//! partial keywords / trailing commas, and closing open containers. It is the
//! mechanism behind `ObjectDelta` (the Genkit-style partial object). Leaf
//! decoding defers to `serde_json`, so escapes and UTF-8 are handled correctly.

use serde_json::{Map, Value};

/// Parse a possibly-truncated JSON prefix into the most complete value it can
/// represent, or `None` if there is no parseable value yet.
pub(crate) fn parse_partial_json(input: &str) -> Option<Value> {
    let mut parser = Partial {
        bytes: input.as_bytes(),
        pos: 0,
    };
    parser.skip_ws();
    parser.value()
}

struct Partial<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Partial<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.skip_ws();
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string(true).map(Value::String),
            b'-' | b'0'..=b'9' => self.number(),
            b't' | b'f' => self.keyword("true", "false"),
            b'n' => self.keyword("null", "null"),
            _ => None,
        }
    }

    fn object(&mut self) -> Option<Value> {
        self.pos += 1; // consume '{'
        let mut map = Map::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'"') => {
                    // A key must be fully terminated; a truncated key ends the object.
                    let Some(key) = self.string(false) else {
                        break;
                    };
                    self.skip_ws();
                    if self.peek() != Some(b':') {
                        break; // dangling key, no colon yet
                    }
                    self.pos += 1;
                    // An incomplete value drops the pair (keeps prior pairs).
                    let Some(value) = self.value() else {
                        break;
                    };
                    map.insert(key, value);
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            break;
                        }
                        _ => break,
                    }
                }
                _ => break,
            }
        }
        Some(Value::Object(map))
    }

    fn array(&mut self) -> Option<Value> {
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                Some(b',') => {
                    self.pos += 1;
                }
                _ => {
                    let Some(value) = self.value() else {
                        break;
                    };
                    items.push(value);
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        _ => break,
                    }
                }
            }
        }
        Some(Value::Array(items))
    }

    /// Read a string starting at the current `"`. Leaf-decode via `serde_json`.
    /// A truncated string yields its best-effort prefix when `allow_partial`,
    /// else `None` (used to drop incomplete keys).
    fn string(&mut self, allow_partial: bool) -> Option<String> {
        let start = self.pos; // at opening quote
        let mut scan = self.pos + 1;
        let mut escaped = false;
        let mut closed = false;
        while let Some(c) = self.bytes.get(scan).copied() {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                closed = true;
                break;
            }
            scan += 1;
        }

        if closed {
            // bytes[start..=scan] is a complete JSON string literal.
            let literal = std::str::from_utf8(&self.bytes[start..=scan]).ok()?;
            let value: String = serde_json::from_str(literal).ok()?;
            self.pos = scan + 1;
            return Some(value);
        }

        // Truncated: no closing quote. Return the longest decodable prefix.
        self.pos = self.bytes.len();
        if !allow_partial {
            return None;
        }
        let raw = std::str::from_utf8(&self.bytes[start + 1..]).ok()?;
        Some(close_partial_string(raw))
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.pos;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.pos += 1;
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        // A partial/invalid number (e.g. "-", "1.") is dropped.
        serde_json::from_str::<Value>(raw).ok()
    }

    fn keyword(&mut self, a: &str, b: &str) -> Option<Value> {
        let rest = &self.bytes[self.pos..];
        for keyword in [a, b] {
            if rest.starts_with(keyword.as_bytes()) {
                self.pos += keyword.len();
                return serde_json::from_str(keyword).ok();
            }
        }
        None // partial keyword prefix
    }
}

/// Close a truncated JSON string body into its longest decodable prefix.
///
/// `raw` is the (still-escaped) content of an unterminated string. Streamed
/// JSON is valid except at the truncation point, so the only undecodable part
/// is a trailing fragment — a dangling `\`, an incomplete `\uXXXX`, or a lone
/// surrogate half (`\uD83D` with its pair not yet streamed). Re-quote and parse
/// the body, dropping one trailing char at a time until it decodes. This is
/// robust where ad-hoc escape-trimming was not: it never mistakes an escaped
/// backslash (`\\u…`) for a unicode escape, and never collapses a split
/// surrogate to an empty string.
fn close_partial_string(raw: &str) -> String {
    let mut end = raw.len();
    while end > 0 {
        if raw.is_char_boundary(end)
            && let Ok(value) = serde_json::from_str::<String>(&format!("\"{}\"", &raw[..end]))
        {
            return value;
        }
        end -= 1;
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(input: &str) -> Value {
        parse_partial_json(input).unwrap()
    }

    #[test]
    fn closes_incomplete_value_string() {
        assert_eq!(parse(r#"{"title":"Up"#), json!({"title": "Up"}));
        assert_eq!(
            parse(r#"{"title":"uploads use "#),
            json!({"title": "uploads use "})
        );
    }

    #[test]
    fn keeps_prior_pairs_and_drops_incomplete_tail() {
        assert_eq!(
            parse(r#"{"title":"Uploads","words":1"#),
            json!({"title": "Uploads", "words": 1})
        );
        assert_eq!(parse(r#"{"title":"Uploads","#), json!({"title": "Uploads"}));
        // dangling key / colon -> drop the incomplete pair
        assert_eq!(
            parse(r#"{"title":"Uploads","words":"#),
            json!({"title": "Uploads"})
        );
        assert_eq!(
            parse(r#"{"title":"Uploads","wo"#),
            json!({"title": "Uploads"})
        );
    }

    #[test]
    fn drops_incomplete_keyword_value() {
        assert_eq!(parse(r#"{"ok":tr"#), json!({}));
        assert_eq!(parse(r#"{"ok":true"#), json!({"ok": true}));
        assert_eq!(parse(r#"{"v":null"#), json!({"v": null}));
    }

    #[test]
    fn nested_and_arrays() {
        assert_eq!(parse(r#"{"a":[1,2"#), json!({"a": [1, 2]}));
        assert_eq!(parse("[1,2,"), json!([1, 2]));
        assert_eq!(parse(r#"{"a":{"b":"c"#), json!({"a": {"b": "c"}}));
    }

    #[test]
    fn empty_and_opening_brace() {
        assert_eq!(parse("{"), json!({}));
        assert_eq!(parse("  {  "), json!({}));
        assert!(parse_partial_json("").is_none());
        assert!(parse_partial_json("   ").is_none());
    }

    #[test]
    fn complete_json_passes_through() {
        assert_eq!(
            parse(r#"{"title":"Uploads","words":12}"#),
            json!({"title": "Uploads", "words": 12})
        );
    }

    #[test]
    fn unicode_and_escapes_in_strings() {
        assert_eq!(parse(r#"{"s":"a\"b"#), json!({"s": "a\"b"}));
        assert_eq!(parse(r#"{"s":"café"#), json!({"s": "café"}));
    }

    #[test]
    fn truncated_surrogate_keeps_the_prefix() {
        // A split surrogate pair (high half streamed, low half not yet) must not
        // collapse the whole value to "" — keep the decodable prefix.
        assert_eq!(parse(r#"{"title":"Hi \uD83D"#), json!({"title": "Hi "}));
        // A truncated `\uXX` escape likewise drops only the incomplete escape.
        assert_eq!(parse(r#"{"s":"abc\u00"#), json!({"s": "abc"}));
    }

    #[test]
    fn escaped_backslash_before_u_is_not_a_unicode_escape() {
        // `\\u` is an escaped backslash + literal `u`, NOT a (truncated) unicode
        // escape; it must decode rather than corrupt the value to "".
        assert_eq!(parse(r#"{"path":"a\\u"#), json!({"path": "a\\u"}));
        assert_eq!(parse(r#"{"path":"C:\\users"#), json!({"path": "C:\\users"}));
    }
}
