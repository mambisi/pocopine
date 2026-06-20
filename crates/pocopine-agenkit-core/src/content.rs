//! Content and messaging payloads (RFC-093 Phase 1.2).
//!
//! A [`Message`] pairs a [`Role`] with [`Content`], a sequence of
//! [`ContentPart`]s. Parts are multimodal: text, a media attachment, or an
//! opaque JSON value (structured tool output). These are the client-safe
//! shapes carried between flows, providers, and threads.

use crate::Role;

/// One piece of a multimodal message.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text.
    Text {
        /// The text body.
        text: String,
    },
    /// A media attachment (image, audio, document, ...).
    Media(MediaPart),
    /// An opaque JSON value, e.g. a structured tool result.
    Json {
        /// The JSON payload.
        value: serde_json::Value,
    },
    /// Model reasoning ("thinking") content. **Server-side only** — it rides the
    /// message model for replay + observability but is redaction-gated at the
    /// wire (§D10) and never crosses to the client. `signature` is a
    /// provider-opaque token replayed verbatim on the next turn (extended-
    /// thinking providers require it); the framework never interprets it.
    Thinking {
        /// The reasoning text.
        text: String,
        /// Opaque provider signature, replayed verbatim next turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

impl ContentPart {
    /// A text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// A reasoning ("thinking") part. `signature` is the provider-opaque token
    /// replayed verbatim next turn (`None` where the provider doesn't sign).
    pub fn thinking(text: impl Into<String>, signature: Option<String>) -> Self {
        Self::Thinking {
            text: text.into(),
            signature,
        }
    }

    /// The text of this part, if it is a [`ContentPart::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }

    /// The reasoning text and signature, if this is a [`ContentPart::Thinking`].
    pub fn as_thinking(&self) -> Option<(&str, Option<&str>)> {
        match self {
            Self::Thinking { text, signature } => Some((text, signature.as_deref())),
            _ => None,
        }
    }
}

/// A media attachment. References by `url` or carries inline `data_base64`;
/// the framework never stores raw provider payloads here by default (§D10).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MediaPart {
    /// IANA media type, e.g. `image/png`.
    pub media_type: String,
    /// External URL for the media, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Inline base64 data, if any (encoded via `pocopine-codec` upstream).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    /// Optional display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An ordered list of [`ContentPart`]s.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Content {
    /// The parts, in order.
    pub parts: Vec<ContentPart>,
}

impl Content {
    /// Content with a single text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            parts: vec![ContentPart::text(text)],
        }
    }

    /// Content from an explicit list of parts.
    pub fn from_parts(parts: Vec<ContentPart>) -> Self {
        Self { parts }
    }

    /// True when there are no parts.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Append a text part.
    pub fn push_text(&mut self, text: impl Into<String>) {
        self.parts.push(ContentPart::text(text));
    }

    /// Concatenate all text parts (media/json parts are skipped).
    pub fn as_text(&self) -> String {
        self.parts
            .iter()
            .filter_map(ContentPart::as_text)
            .collect::<Vec<_>>()
            .join("")
    }
}

impl From<&str> for Content {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl From<String> for Content {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

/// A single message in a conversation/generation request.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    /// Who authored the message.
    pub role: Role,
    /// The message body.
    pub content: Content,
    /// Tool calls this (assistant) message requested. Carries the protocol
    /// linkage providers like OpenAI require between a tool request and its
    /// result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<crate::ToolCall>,
    /// The id of the [`crate::ToolCall`] a `tool`-role message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// A message with an explicit role and content.
    pub fn new(role: Role, content: impl Into<Content>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// An `assistant` message that requested the given tool calls, preserving
    /// any text the model emitted alongside them (some models narrate before a
    /// call; dropping it loses that context on the next turn). Pass an empty
    /// content for a pure tool-call turn.
    pub fn assistant_tool_calls(
        content: impl Into<Content>,
        tool_calls: Vec<crate::ToolCall>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }

    /// A `tool` result message answering the call with `tool_call_id`.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<Content>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// A `system` message.
    pub fn system(content: impl Into<Content>) -> Self {
        Self::new(Role::System, content)
    }

    /// A `user` message.
    pub fn user(content: impl Into<Content>) -> Self {
        Self::new(Role::User, content)
    }

    /// An `assistant` message.
    pub fn assistant(content: impl Into<Content>) -> Self {
        Self::new(Role::Assistant, content)
    }

    /// The concatenated text of the message body.
    pub fn text(&self) -> String {
        self.content.as_text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_content_round_trips() {
        let content = Content::text("hello");
        let json = serde_json::to_string(&content).unwrap();
        assert_eq!(json, r#"[{"type":"text","text":"hello"}]"#);
        let back: Content = serde_json::from_str(&json).unwrap();
        assert_eq!(content, back);
        assert_eq!(back.as_text(), "hello");
    }

    #[test]
    fn media_part_omits_none_fields() {
        let part = ContentPart::Media(MediaPart {
            media_type: "image/png".to_string(),
            url: Some("https://x/y.png".to_string()),
            data_base64: None,
            name: None,
        });
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains(r#""type":"media""#));
        assert!(json.contains(r#""media_type":"image/png""#));
        assert!(!json.contains("data_base64"));
        assert_eq!(serde_json::from_str::<ContentPart>(&json).unwrap(), part);
    }

    #[test]
    fn message_constructors_set_role_and_text() {
        let msg = Message::user("how do uploads work?");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.text(), "how do uploads work?");
        let back: Message = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn as_text_joins_text_parts_and_skips_others() {
        let content = Content::from_parts(vec![
            ContentPart::text("a"),
            ContentPart::Json {
                value: serde_json::json!({"k": 1}),
            },
            ContentPart::text("b"),
        ]);
        assert_eq!(content.as_text(), "ab");
    }

    #[test]
    fn thinking_part_round_trips_with_signature() {
        let part = ContentPart::thinking("let me reason", Some("sig-abc".to_string()));
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains(r#""type":"thinking""#));
        assert!(json.contains(r#""text":"let me reason""#));
        assert!(json.contains(r#""signature":"sig-abc""#));
        assert_eq!(serde_json::from_str::<ContentPart>(&json).unwrap(), part);
        assert_eq!(part.as_thinking(), Some(("let me reason", Some("sig-abc"))));
    }

    #[test]
    fn thinking_part_omits_absent_signature() {
        let part = ContentPart::thinking("reasoning", None);
        let json = serde_json::to_string(&part).unwrap();
        assert!(!json.contains("signature"));
        // A wire payload without a signature deserializes to `None`.
        let back: ContentPart = serde_json::from_str(r#"{"type":"thinking","text":"r"}"#).unwrap();
        assert_eq!(back, ContentPart::thinking("r", None));
        assert_eq!(part.as_thinking(), Some(("reasoning", None)));
    }

    #[test]
    fn as_text_skips_thinking_parts() {
        // Thinking content never leaks into user-visible text (§D10).
        let content = Content::from_parts(vec![
            ContentPart::text("answer "),
            ContentPart::thinking("secret reasoning", Some("sig".to_string())),
            ContentPart::text("here"),
        ]);
        assert_eq!(content.as_text(), "answer here");
    }
}
