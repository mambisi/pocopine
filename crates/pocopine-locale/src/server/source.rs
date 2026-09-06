use std::collections::BTreeMap;

use super::{Diagnostic, Span};

#[derive(Clone, Debug)]
pub struct SourceMessage {
    pub text: String,
    pub span: Span,
}

pub type SourceMessages = BTreeMap<String, SourceMessage>;

/// Read the RFC's flat JSON string map, retaining value spans and detecting
/// duplicate decoded keys (including aliases written with JSON escapes).
/// String decoding is delegated to serde_json, not reimplemented here.
pub fn parse_messages(source: &str, file: u32) -> Result<SourceMessages, Diagnostic> {
    if source.len() > crate::catalog::MAX_CATALOG_BYTES {
        return Err(Diagnostic::error("locale source exceeds 16 MiB").at(Span {
            file,
            start: 0,
            end: 0,
        }));
    }
    let mut reader = Reader {
        source,
        offset: 0,
        file,
    };
    reader.space();
    reader.expect(b'{')?;
    reader.space();
    let mut messages = BTreeMap::new();
    if reader.byte() != Some(b'}') {
        loop {
            let (key, key_span) = reader.string()?;
            reader.space();
            reader.expect(b':')?;
            reader.space();
            let (text, span) = reader.string()?;
            if messages
                .insert(key.clone(), SourceMessage { text, span })
                .is_some()
            {
                return Err(
                    Diagnostic::error(format!("duplicate message key {key:?}")).at(key_span)
                );
            }
            if messages.len() > crate::catalog::MAX_MESSAGES {
                return Err(reader.error("locale source exceeds 100000 messages"));
            }
            reader.space();
            if reader.byte() != Some(b',') {
                break;
            }
            reader.offset += 1;
            reader.space();
        }
    }
    reader.expect(b'}')?;
    reader.space();
    if reader.offset != source.len() {
        return Err(reader.error("unexpected content after locale object"));
    }
    Ok(messages)
}

struct Reader<'a> {
    source: &'a str,
    offset: usize,
    file: u32,
}

impl Reader<'_> {
    fn byte(&self) -> Option<u8> {
        self.source.as_bytes().get(self.offset).copied()
    }
    fn space(&mut self) {
        while self
            .byte()
            .is_some_and(|b| matches!(b, b' ' | b'\r' | b'\n' | b'\t'))
        {
            self.offset += 1;
        }
    }
    fn error(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::error(message).at(Span {
            file: self.file,
            start: self.offset as u32,
            end: self.offset.saturating_add(1).min(self.source.len()) as u32,
        })
    }
    fn expect(&mut self, byte: u8) -> Result<(), Diagnostic> {
        if self.byte() != Some(byte) {
            return Err(self.error(format!("expected {:?}", char::from(byte))));
        }
        self.offset += 1;
        Ok(())
    }
    fn string(&mut self) -> Result<(String, Span), Diagnostic> {
        let start = self.offset;
        let mut decoder =
            serde_json::Deserializer::from_str(&self.source[start..]).into_iter::<String>();
        let value = decoder
            .next()
            .ok_or_else(|| self.error("expected a JSON string"))?
            .map_err(|error| self.error(format!("expected a JSON string: {error}")))?;
        self.offset += decoder.byte_offset();
        Ok((
            value,
            Span {
                file: self.file,
                start: start as u32,
                end: self.offset as u32,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_escaped_text_and_rejects_duplicate_decoded_keys() {
        let source = r#"{ "auth.sign_in": "Sign \"in\"", "common.loading": "Loading\u2026" }"#;
        let parsed = parse_messages(source, 7).unwrap();
        assert_eq!(parsed["common.loading"].text, "Loading…");
        let span = parsed["auth.sign_in"].span;
        assert_eq!(span.file, 7);
        assert_eq!(
            &source[span.start as usize..span.end as usize],
            r#""Sign \"in\"""#
        );
        let error = parse_messages(r#"{"common.x":"a","common.\u0078":"b"}"#, 0).unwrap_err();
        assert!(error.message.contains("duplicate"));
    }

    #[test]
    fn requires_a_flat_string_map_and_complete_json() {
        for source in [
            "[]",
            "null",
            r#"{"x":2}"#,
            r#"{"x":{"nested":"value"}}"#,
            r#"{"x":"a",}"#,
            "{} trailing",
            r#"{"x":"a" "y":"b"}"#,
        ] {
            assert!(parse_messages(source, 0).is_err(), "{source}");
        }
    }
}
