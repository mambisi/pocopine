//! Quoted Rust literals in inline templates share one decoding path between
//! component expansion and source tools. Source mapping retains original Rust
//! byte positions even when decoding escapes changes a literal's length.

use std::ops::Range;

use proc_macro2::{TokenStream, TokenTree};

#[derive(Debug)]
pub struct NormalizedTemplate {
    pub source: String,
    edits: Vec<(Range<usize>, Range<usize>)>,
}

impl NormalizedTemplate {
    /// Map a byte offset in normalized HTML back into the original body.
    pub fn original_offset(&self, offset: usize) -> usize {
        let mut original_end = 0;
        let mut normalized_end = 0;
        for (original, normalized) in &self.edits {
            if offset < normalized.start {
                break;
            }
            if offset < normalized.end {
                return original.start + (offset - normalized.start).min(original.len());
            }
            original_end = original.end;
            normalized_end = normalized.end;
        }
        original_end + offset - normalized_end
    }
}

/// Decode string literals in text position. Attribute values (preceded by =)
/// and interpolation groups belong to the template/expression parsers and are
/// left untouched. `base` is the body's byte offset in the tokens' source file.
pub fn normalize_inline_text(source: &str, tokens: TokenStream, base: usize) -> NormalizedTemplate {
    let mut replacements = Vec::new();
    let mut previous_eq = false;
    for token in tokens {
        match token {
            TokenTree::Literal(literal) => {
                if !previous_eq
                    && let Ok(text) = syn::parse_str::<syn::LitStr>(&literal.to_string())
                {
                    let range = literal.span().byte_range();
                    if range.start >= base && range.end <= base + source.len() {
                        let range = (range.start - base)..(range.end - base);
                        if source.is_char_boundary(range.start)
                            && source.is_char_boundary(range.end)
                        {
                            let escaped = text
                                .value()
                                .replace('&', "&amp;")
                                .replace('<', "&lt;")
                                .replace('>', "&gt;");
                            replacements.push((range, escaped));
                        }
                    }
                }
                previous_eq = false;
            }
            TokenTree::Punct(punct) => previous_eq = punct.as_char() == '=',
            _ => previous_eq = false,
        }
    }
    let mut out = NormalizedTemplate {
        source: String::with_capacity(source.len()),
        edits: Vec::new(),
    };
    let mut cursor = 0;
    for (range, replacement) in replacements {
        out.source.push_str(&source[cursor..range.start]);
        let start = out.source.len();
        out.source.push_str(&replacement);
        out.edits.push((range.clone(), start..out.source.len()));
        cursor = range.end;
    }
    out.source.push_str(&source[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_only_text_and_maps_following_unicode_spans() {
        let source = r#"<p title="&amp;">"Don't stop — ©" "\u{7b}\u{7b} $t.common.ready }}"</p><input :title="$t.common.label">"#;
        let normalized = normalize_inline_text(source, source.parse().unwrap(), 0);
        assert!(
            normalized
                .source
                .contains("Don't stop — © {{ $t.common.ready }}")
        );
        assert!(normalized.source.contains("title=\"&amp;\""));
        assert_eq!(
            normalized.original_offset(normalized.source.find("<input").unwrap()),
            source.find("<input").unwrap()
        );
        assert_eq!(
            normalized.original_offset(normalized.source.len()),
            source.len()
        );
    }
}
