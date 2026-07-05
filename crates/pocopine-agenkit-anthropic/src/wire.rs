//! Anthropic Messages API wire types and the mapping to/from Agenkit's neutral
//! request/response.
//!
//! The Messages API differs from OpenAI Chat Completions in three ways this
//! module bridges: the system prompt is a **top-level field** (not a message
//! role), tool use/results are **content blocks** inside user/assistant
//! messages (there is no `tool` role), and there is **no `response_format`** —
//! structured output is obtained by forcing a single tool whose `input_schema`
//! is the desired shape.

use pocopine_agenkit::server::{FinishReason, GenerateRequest, GenerateResponse};
use pocopine_agenkit_core::{
    AgenkitResult, Content, ContentPart, Message, ModelRef, Role, ThinkingLevel, ToolCall,
    ToolDescriptor, ToolNameMap, Usage,
};
use serde::{Deserialize, Serialize};

/// Name of the synthetic tool used to coax schema-shaped structured output.
/// Conforms to Anthropic's `^[a-zA-Z0-9_-]{1,64}$` tool-name rule.
pub(crate) const STRUCTURED_TOOL_NAME: &str = "structured_output";

/// Whether a model is reasoning-capable per the W1 catalog. Only such models
/// get a `thinking` request block; everything else ignores [`ThinkingLevel`].
/// An unknown alias (not in the catalog) is treated as non-reasoning.
fn model_supports_reasoning(model: &ModelRef) -> bool {
    pocopine_agenkit::server::catalog::lookup(model)
        .map(|m| m.reasoning)
        .unwrap_or(false)
}

/// Whether the catalog **positively** denies tool calling for `model`. An
/// unknown alias passes (the provider decides). Used to skip the forced
/// structured-output tool — the trick rides tool calling, so a toolless model
/// gets the system-prompt fallback instead of a request the endpoint rejects.
fn model_denies_tools(model: &ModelRef) -> bool {
    pocopine_agenkit::server::catalog::lookup(model).is_some_and(|m| !m.tools)
}

/// Map a [`ThinkingLevel`] to an Anthropic extended-thinking `budget_tokens`.
/// `Off` requests no thinking. The minimum Anthropic accepts is 1024.
fn reasoning_budget(level: ThinkingLevel) -> Option<u32> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal => Some(1_024),
        ThinkingLevel::Low => Some(4_096),
        ThinkingLevel::Medium => Some(8_192),
        ThinkingLevel::High => Some(16_384),
    }
}

// Tool-name sanitization and the collision-safe reverse map live in
// `pocopine_agenkit_core::tool_name` (shared with the OpenAI provider).

// ---- request ----------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct MessagesRequest {
    pub(crate) model: String,
    /// Anthropic **requires** `max_tokens`; the provider supplies a default
    /// when the neutral request leaves it unset.
    pub(crate) max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) system: Option<String>,
    pub(crate) messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<WireTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<ToolChoice>,
    /// Extended-thinking budget (roadmap W4); omitted unless requested for a
    /// reasoning-capable model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream: Option<bool>,
    /// Provider-specific extra body fields
    /// (`GenerateRequest::provider_options`), flattened verbatim into the top
    /// level of the request. Serialized after the typed fields, so a
    /// duplicated key reaches the provider twice (most JSON parsers keep the
    /// later one).
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

/// Anthropic's extended-thinking request block: `{"type":"enabled","budget_tokens":N}`.
#[derive(Serialize)]
pub(crate) struct ThinkingConfig {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) budget_tokens: u32,
}

impl MessagesRequest {
    /// Map a neutral request. `stream` toggles SSE; `default_max_tokens` fills
    /// the required cap when the request omits one.
    ///
    /// Errors (config) when the request carries media this wire cannot deliver
    /// — see [`GenerateRequest::ensure_media_support`] — instead of silently
    /// dropping an attachment.
    pub(crate) fn from_agenkit(
        request: &GenerateRequest,
        stream: bool,
        default_max_tokens: u32,
    ) -> AgenkitResult<Self> {
        request.ensure_media_support()?;
        request.ensure_tool_support()?;
        // The Messages API has no system role: hoist `request.system` plus any
        // System-role messages into the top-level `system` field.
        let mut system = request.system.clone();
        for message in &request.messages {
            if message.role == Role::System {
                append_system(&mut system, message.content.as_text());
            }
        }

        // Collision-safe tool-name map for this request's tools, used for both
        // the tool defs and any replayed assistant tool calls in history.
        let names = ToolNameMap::from_descriptors(&request.tools);
        let messages = fold_messages(&request.messages, &names);

        let mut tools: Vec<WireTool> = request
            .tools
            .iter()
            .map(|tool| WireTool::from_descriptor(tool, &names))
            .collect();
        let has_user_tools = !tools.is_empty();

        // Extended thinking (W4): only for reasoning-capable models; `Off` and
        // unknown/non-reasoning models send no `thinking` block.
        let thinking_budget = model_supports_reasoning(&request.model)
            .then(|| reasoning_budget(request.thinking))
            .flatten();

        // Structured output: Anthropic has no `response_format`.
        let mut tool_choice = None;
        if let Some(schema) = &request.json_schema {
            // A forced `tool_choice` is incompatible with extended thinking
            // (Anthropic rejects forcing a specific tool while thinking is on), so
            // when thinking is requested fall back to the system-prompt approach
            // even with no user tools. Likewise for a model the catalog
            // positively marks toolless — the trick rides tool calling.
            if !has_user_tools && thinking_budget.is_none() && !model_denies_tools(&request.model) {
                // With no user tools, force a single tool whose `input_schema` is
                // the requested shape — the model must "call" it, yielding
                // schema-constrained JSON.
                tools.push(WireTool {
                    name: STRUCTURED_TOOL_NAME.to_string(),
                    description: "Return the final answer as a structured JSON object.".to_string(),
                    input_schema: normalize_schema(schema),
                });
                tool_choice = Some(ToolChoice {
                    kind: "tool",
                    name: STRUCTURED_TOOL_NAME.to_string(),
                });
            } else {
                // With user tools present (or thinking on) we cannot force the
                // structured tool — Anthropic forces at most one tool per turn,
                // which would suppress the user tools (`tool_choice: tool`
                // prefills a tool_use and emits no other tools). Instead instruct
                // the model to emit the schema-shaped JSON as its final answer
                // once it is done calling tools; the runtime parses that JSON.
                // This is the documented "auto + prompt" approach and is the path
                // the agent loop takes.
                append_system(&mut system, structured_instruction(schema));
            }
        }

        // Anthropic counts thinking + visible output both against `max_tokens`.
        // The caller's `max_tokens` is their OUTPUT budget, so add the thinking
        // budget ON TOP of it — rather than overriding the caller's explicit cap
        // (which silently billed far more output than requested).
        let mut max_tokens = request.max_tokens.unwrap_or(default_max_tokens);
        let thinking = thinking_budget.map(|budget| {
            max_tokens = max_tokens.saturating_add(budget);
            ThinkingConfig {
                kind: "enabled",
                budget_tokens: budget,
            }
        });

        Ok(Self {
            model: request.model.model().to_string(),
            max_tokens,
            system,
            messages,
            tools,
            tool_choice,
            thinking,
            stream: stream.then_some(true),
            extra: request.provider_options.clone(),
        })
    }
}

fn append_system(system: &mut Option<String>, text: String) {
    if text.is_empty() {
        return;
    }
    *system = Some(match system.take() {
        Some(existing) => format!("{existing}\n{text}"),
        None => text,
    });
}

/// Map neutral messages to Anthropic messages, folding consecutive same-role
/// messages into one (the Messages API expects content blocks grouped per
/// turn — e.g. several tool results belong in one user message).
fn fold_messages(messages: &[Message], names: &ToolNameMap) -> Vec<WireMessage> {
    let mut folded: Vec<WireMessage> = Vec::new();
    for message in messages {
        let (role, blocks) = match message.role {
            Role::System => continue, // hoisted to top-level `system`
            Role::User => ("user", user_blocks(&message.content)),
            Role::Assistant => assistant_blocks(message, names),
            Role::Tool => ("user", tool_result_blocks(message)),
        };
        if blocks.is_empty() {
            continue;
        }
        match folded.last_mut() {
            Some(last) if last.role == role => last.content.extend(blocks),
            _ => folded.push(WireMessage {
                role,
                content: blocks,
            }),
        }
    }
    folded
}

fn text_block(content: &Content) -> Option<ContentBlock> {
    let text = content.as_text();
    (!text.is_empty()).then_some(ContentBlock::Text { text })
}

/// User content: the single joined text block when there is no media (the
/// common path, unchanged), else per-part text/image blocks in author order.
/// `GenerateRequest::ensure_media_support` (checked at request mapping)
/// guarantees every media part is an image with a source.
fn user_blocks(content: &Content) -> Vec<ContentBlock> {
    let has_media = content
        .parts
        .iter()
        .any(|part| matches!(part, ContentPart::Media(_)));
    if !has_media {
        return text_block(content).into_iter().collect();
    }
    let mut blocks = Vec::new();
    for part in &content.parts {
        match part {
            ContentPart::Text { text } if !text.is_empty() => {
                blocks.push(ContentBlock::Text { text: text.clone() })
            }
            ContentPart::Media(media) => {
                let source = match (&media.url, &media.data_base64) {
                    (Some(url), _) => ImageSource::Url { url: url.clone() },
                    (None, Some(data)) => ImageSource::Base64 {
                        media_type: media.media_type.clone(),
                        data: data.clone(),
                    },
                    // Rejected upstream by `ensure_media_support`.
                    (None, None) => continue,
                };
                blocks.push(ContentBlock::Image { source });
            }
            // Parity with `Content::as_text()`: Json/Thinking parts never cross
            // the wire as user-visible content.
            _ => {}
        }
    }
    blocks
}

fn assistant_blocks(message: &Message, names: &ToolNameMap) -> (&'static str, Vec<ContentBlock>) {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    // Replay any reasoning blocks first (W4) — Anthropic requires the original
    // `thinking` block, with its signature verbatim, to precede the final answer
    // on a turn that follows extended thinking, or the request 400s. A thinking
    // part with no signature can't be replayed, so it is dropped.
    for part in &message.content.parts {
        if let Some((thinking, Some(signature))) = part.as_thinking() {
            blocks.push(ContentBlock::Thinking {
                thinking: thinking.to_string(),
                signature: signature.to_string(),
            });
        }
    }
    blocks.extend(text_block(&message.content));
    for call in &message.tool_calls {
        blocks.push(ContentBlock::ToolUse {
            id: call.id.clone(),
            name: names.wire(&call.tool_id),
            input: call.args.clone(),
        });
    }
    ("assistant", blocks)
}

fn tool_result_blocks(message: &Message) -> Vec<ContentBlock> {
    match message.tool_call_id.clone() {
        Some(tool_use_id) => vec![ContentBlock::ToolResult {
            tool_use_id,
            content: message.content.as_text(),
        }],
        // A tool message without a `tool_call_id` can't form a valid
        // `tool_result` (which must reference a `tool_use_id`, or the Messages
        // API 400s). Rather than silently dropping the turn, surface its content
        // as plain user text so the model still sees the output. The agent loop
        // always sets the id; a missing one only occurs in a hand-built history.
        None => text_block(&message.content).into_iter().collect(),
    }
}

/// The system-prompt instruction used when structured output cannot be forced
/// via a tool (user tools are present). Mirrors the runtime's JSON-mode fallback
/// so the model emits a single schema-shaped JSON object as its final answer.
fn structured_instruction(schema: &serde_json::Value) -> String {
    format!(
        "After any tool use is complete, respond with a single JSON object that conforms to \
         this JSON Schema, and nothing else:\n{}",
        normalize_schema(schema)
    )
}

/// Anthropic's `input_schema` is plain JSON Schema; drop the `$schema` meta key
/// schemars emits (harmless but unnecessary on the wire).
fn normalize_schema(schema: &serde_json::Value) -> serde_json::Value {
    let mut schema = schema.clone();
    if let Some(map) = schema.as_object_mut() {
        map.remove("$schema");
    }
    schema
}

#[derive(Serialize)]
pub(crate) struct WireMessage {
    pub(crate) role: &'static str,
    pub(crate) content: Vec<ContentBlock>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ContentBlock {
    Text {
        text: String,
    },
    /// An image attachment in a user message, referenced by URL or inlined as
    /// base64.
    Image {
        source: ImageSource,
    },
    /// A replayed reasoning block (W4): `thinking` text + the opaque `signature`
    /// the model returned, sent back verbatim so Anthropic can verify it.
    Thinking {
        thinking: String,
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// An image block's source: `{"type":"url","url":..}` or
/// `{"type":"base64","media_type":..,"data":..}`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Serialize)]
pub(crate) struct ToolChoice {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) name: String,
}

#[derive(Serialize)]
pub(crate) struct WireTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: serde_json::Value,
}

impl WireTool {
    fn from_descriptor(descriptor: &ToolDescriptor, names: &ToolNameMap) -> Self {
        let input_schema = descriptor
            .input
            .json_schema
            .clone()
            .map(|schema| normalize_schema(&schema))
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        Self {
            name: names.wire(&descriptor.id),
            description: descriptor.description.clone(),
            input_schema,
        }
    }
}

// ---- non-streaming response -------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct MessagesResponse {
    #[serde(default)]
    pub(crate) content: Vec<ResponseBlock>,
    #[serde(default)]
    pub(crate) stop_reason: Option<String>,
    #[serde(default)]
    pub(crate) usage: Option<WireUsage>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseBlock {
    Text {
        text: String,
    },
    /// A reasoning block (W4). The `signature` is opaque and replayed verbatim on
    /// the next turn; an empty/absent one means the block can't be replayed.
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    /// Ignore block types we don't model (e.g. `redacted_thinking`).
    #[serde(other)]
    Other,
}

impl MessagesResponse {
    pub(crate) fn into_agenkit(self, names: &ToolNameMap) -> GenerateResponse {
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut structured: Option<serde_json::Value> = None;
        // Reasoning blocks (W4): kept server-side for replay/observability, and
        // skipped by `Content::as_text()` so they never reach user-visible text.
        let mut thinking_parts: Vec<ContentPart> = Vec::new();

        for block in self.content {
            match block {
                ResponseBlock::Text { text: fragment } => text.push_str(&fragment),
                ResponseBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    let signature = (!signature.is_empty()).then_some(signature);
                    thinking_parts.push(ContentPart::thinking(thinking, signature));
                }
                ResponseBlock::ToolUse { id, name, input } => {
                    // A `tool_use` input is normally an object (`{}` for a no-arg
                    // tool), but normalize a missing/`null` input to `{}` — a
                    // struct-typed tool input deserializes from `{}`, never from
                    // `null` (which would fail with a cryptic "expected object").
                    let input = if input.is_null() {
                        serde_json::json!({})
                    } else {
                        input
                    };
                    if name == STRUCTURED_TOOL_NAME {
                        // The forced structured tool: its input IS the answer.
                        structured = Some(input);
                    } else {
                        tool_calls.push(ToolCall::new(id, names.resolve(&name), input));
                    }
                }
                ResponseBlock::Other => {}
            }
        }

        let mut response = match structured {
            Some(value) => GenerateResponse::structured(value),
            None => GenerateResponse::text(text),
        };
        // Reasoning precedes the answer: prepend thinking parts to the content so
        // a stored assistant turn replays them in order (server-side only).
        if !thinking_parts.is_empty() {
            thinking_parts.extend(std::mem::take(&mut response.content.parts));
            response.content = Content::from_parts(thinking_parts);
        }
        response.tool_calls = tool_calls;
        response.usage = self.usage.map(WireUsage::into_usage);
        response.finish_reason = finish_reason(self.stop_reason.as_deref());
        response
    }
}

#[derive(Deserialize)]
pub(crate) struct WireUsage {
    #[serde(default)]
    pub(crate) input_tokens: u64,
    #[serde(default)]
    pub(crate) output_tokens: u64,
    // Anthropic reports cache tokens separately; `input_tokens` is already the
    // uncached count, so no subtraction is needed.
    #[serde(default)]
    pub(crate) cache_read_input_tokens: u64,
    #[serde(default)]
    pub(crate) cache_creation_input_tokens: u64,
}

impl WireUsage {
    pub(crate) fn into_usage(self) -> Usage {
        Usage::new(self.input_tokens, self.output_tokens).with_cache(
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        )
    }
}

pub(crate) fn finish_reason(raw: Option<&str>) -> FinishReason {
    match raw {
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    }
}

// ---- streaming events -------------------------------------------------------

/// One decoded Anthropic SSE event. The `data:` payload is self-describing via
/// its `type`, so the `event:` line is ignored.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum StreamEvent {
    MessageStart {
        message: StreamMessageStart,
    },
    ContentBlockStart {
        index: u32,
        content_block: StreamBlockStart,
    },
    ContentBlockDelta {
        index: u32,
        delta: StreamDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        #[serde(default)]
        usage: Option<WireUsage>,
    },
    MessageStop,
    /// `ping` and any future event types.
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
pub(crate) struct StreamMessageStart {
    #[serde(default)]
    pub(crate) usage: Option<WireUsage>,
}

#[derive(Deserialize)]
pub(crate) struct StreamBlockStart {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum StreamDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    /// Incremental reasoning text (W4).
    ThinkingDelta {
        thinking: String,
    },
    /// The reasoning block's opaque signature, delivered once near its end (W4).
    SignatureDelta {
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_agenkit::server::models;
    use pocopine_agenkit_core::Content;

    fn to_value(request: &MessagesRequest) -> serde_json::Value {
        serde_json::to_value(request).unwrap()
    }

    // Tool-name sanitization + the collision-safe reverse map are tested in
    // `pocopine_agenkit_core::tool_name`.

    #[test]
    fn hoists_system_field_and_system_messages() {
        let request = GenerateRequest {
            model: models::anthropic::CLAUDE_OPUS_4_8,
            system: Some("be brief".to_string()),
            messages: vec![
                Message::new(Role::System, "also be kind"),
                Message::new(Role::User, "hi"),
            ],
            ..GenerateRequest::default()
        };
        let wire = MessagesRequest::from_agenkit(&request, false, 4096).unwrap();
        assert_eq!(wire.system.as_deref(), Some("be brief\nalso be kind"));
        // The system message is hoisted out, leaving only the user turn.
        let value = to_value(&wire);
        assert_eq!(value["messages"].as_array().unwrap().len(), 1);
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["max_tokens"], 4096);
    }

    #[test]
    fn toolless_model_gets_the_prompt_fallback_not_a_forced_tool() {
        // The forced structured-output tool rides tool calling; a model the
        // catalog positively marks toolless (gpt-5-chat-latest resolves via the
        // model-portion fallback) gets the system-prompt fallback instead.
        let request = GenerateRequest {
            model: ModelRef::new("openai/gpt-5-chat-latest"),
            messages: vec![Message::new(Role::User, "summarize")],
            json_schema: Some(serde_json::json!({"type": "object", "properties": {}})),
            ..GenerateRequest::default()
        };
        let wire = MessagesRequest::from_agenkit(&request, false, 4096).unwrap();
        let value = to_value(&wire);
        assert!(value.get("tool_choice").is_none() || value["tool_choice"].is_null());
        assert!(
            value.get("tools").is_none() || value["tools"].as_array().is_none_or(Vec::is_empty)
        );
        assert!(
            wire.system
                .as_deref()
                .is_some_and(|s| s.contains("JSON Schema")),
            "schema instruction should ride the system prompt"
        );
    }

    #[test]
    fn user_images_become_image_blocks() {
        use pocopine_agenkit_core::MediaPart;
        let request = GenerateRequest {
            model: models::anthropic::CLAUDE_OPUS_4_8,
            messages: vec![Message::new(
                Role::User,
                Content::from_parts(vec![
                    ContentPart::text("describe this"),
                    ContentPart::Media(MediaPart {
                        media_type: "image/png".to_string(),
                        url: None,
                        data_base64: Some("QUJD".to_string()),
                        name: None,
                    }),
                    ContentPart::Media(MediaPart {
                        media_type: "image/png".to_string(),
                        url: Some("https://example.com/cat.png".to_string()),
                        data_base64: None,
                        name: None,
                    }),
                ]),
            )],
            ..GenerateRequest::default()
        };
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096).unwrap());
        let content = &value["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "describe this");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "QUJD");
        assert_eq!(content[2]["source"]["type"], "url");
        assert_eq!(content[2]["source"]["url"], "https://example.com/cat.png");
    }

    #[test]
    fn provider_options_flatten_into_the_request_body() {
        let mut request = GenerateRequest {
            model: models::anthropic::CLAUDE_OPUS_4_8,
            messages: vec![Message::new(Role::User, "hi")],
            ..GenerateRequest::default()
        };
        request.provider_options.insert(
            "metadata".to_string(),
            serde_json::json!({"user_id": "u-1"}),
        );
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096).unwrap());
        assert_eq!(value["metadata"]["user_id"], "u-1");
        // Typed fields are unaffected.
        assert_eq!(value["max_tokens"], 4096);
    }

    #[test]
    fn forces_a_tool_for_structured_output_without_user_tools() {
        let request = GenerateRequest {
            model: models::anthropic::CLAUDE_OPUS_4_8,
            messages: vec![Message::new(Role::User, "summarize")],
            json_schema: Some(serde_json::json!({"type": "object", "properties": {}})),
            ..GenerateRequest::default()
        };
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096).unwrap());
        assert_eq!(value["tool_choice"]["type"], "tool");
        assert_eq!(value["tool_choice"]["name"], STRUCTURED_TOOL_NAME);
        assert_eq!(value["tools"][0]["name"], STRUCTURED_TOOL_NAME);
    }

    #[test]
    fn does_not_force_a_tool_when_user_tools_are_present() {
        let request = GenerateRequest {
            model: models::anthropic::CLAUDE_OPUS_4_8,
            messages: vec![Message::new(Role::User, "summarize")],
            tools: vec![ToolDescriptor::new("search", "Search")],
            json_schema: Some(serde_json::json!({"type": "object"})),
            ..GenerateRequest::default()
        };
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096).unwrap());
        assert!(value.get("tool_choice").is_none() || value["tool_choice"].is_null());
        assert_eq!(value["tools"][0]["name"], "search");
    }

    #[test]
    fn colliding_tool_ids_get_distinct_wire_names() {
        // Two ids that sanitize to the same wire name must stay distinct, or a
        // model tool call would dispatch to the wrong tool (#5).
        let request = GenerateRequest {
            model: models::anthropic::CLAUDE_OPUS_4_8,
            messages: vec![Message::new(Role::User, "hi")],
            tools: vec![
                ToolDescriptor::new("weather.lookup", "a"),
                ToolDescriptor::new("weather/lookup", "b"),
            ],
            ..GenerateRequest::default()
        };
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096).unwrap());
        let names: Vec<&str> = value["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1], "colliding ids must get distinct names");
    }

    #[test]
    fn tool_message_without_id_becomes_user_text_not_dropped() {
        // A tool turn with no `tool_call_id` can't form a valid `tool_result`;
        // its content must still reach the model as user text, never be dropped
        // (which would leave a preceding tool_use unpaired → 400) (#6).
        let request = GenerateRequest {
            model: models::anthropic::CLAUDE_OPUS_4_8,
            messages: vec![Message {
                role: Role::Tool,
                content: Content::text("tool output"),
                tool_calls: vec![],
                tool_call_id: None,
            }],
            ..GenerateRequest::default()
        };
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096).unwrap());
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "the turn must not be dropped");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[0]["content"][0]["text"], "tool output");
    }

    #[test]
    fn folds_assistant_tool_use_and_tool_result_turns() {
        let request = GenerateRequest {
            model: models::anthropic::CLAUDE_OPUS_4_8,
            messages: vec![
                Message::new(Role::User, "weather?"),
                Message {
                    role: Role::Assistant,
                    content: Content::text(""),
                    tool_calls: vec![ToolCall::new(
                        "toolu_1",
                        "get_weather",
                        serde_json::json!({"city": "Paris"}),
                    )],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Tool,
                    content: Content::text("sunny"),
                    tool_calls: vec![],
                    tool_call_id: Some("toolu_1".to_string()),
                },
            ],
            ..GenerateRequest::default()
        };
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096).unwrap());
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn response_maps_text_tool_and_structured() {
        let names = ToolNameMap::from_descriptors(&[ToolDescriptor::new("weather.lookup", "w")]);
        // A real tool call: sanitized name maps back to the original id.
        let resp: MessagesResponse = serde_json::from_value(serde_json::json!({
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "weather_lookup", "input": {"city": "Paris"}}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        }))
        .unwrap();
        let mapped = resp.into_agenkit(&names);
        assert_eq!(mapped.tool_calls.len(), 1);
        assert_eq!(mapped.tool_calls[0].tool_id, "weather.lookup");

        // The forced structured tool surfaces as a structured value, not a call.
        let resp: MessagesResponse = serde_json::from_value(serde_json::json!({
            "content": [{"type": "tool_use", "id": "toolu_2", "name": STRUCTURED_TOOL_NAME, "input": {"title": "X"}}],
            "stop_reason": "tool_use"
        }))
        .unwrap();
        let mapped = resp.into_agenkit(&ToolNameMap::default());
        assert!(mapped.tool_calls.is_empty());
        assert_eq!(
            mapped.structured_value(),
            Some(&serde_json::json!({"title": "X"}))
        );
    }

    #[test]
    fn wire_usage_parses_cache_tokens() {
        // Anthropic reports cache tokens separately; input_tokens is already uncached.
        let u: super::WireUsage = serde_json::from_str(
            r#"{"input_tokens":40,"output_tokens":12,"cache_read_input_tokens":100,"cache_creation_input_tokens":7}"#,
        )
        .unwrap();
        let usage = u.into_usage();
        assert_eq!(usage.input_tokens, 40);
        assert_eq!(usage.output_tokens, 12);
        assert_eq!(usage.cache_read_tokens, 100);
        assert_eq!(usage.cache_creation_tokens, 7);
        // Missing cache fields default to 0.
        let plain: super::WireUsage =
            serde_json::from_str(r#"{"input_tokens":5,"output_tokens":6}"#).unwrap();
        let usage = plain.into_usage();
        assert_eq!(
            (usage.cache_read_tokens, usage.cache_creation_tokens),
            (0, 0)
        );
    }
}
