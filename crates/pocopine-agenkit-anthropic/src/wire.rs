//! Anthropic Messages API wire types and the mapping to/from Agenkit's neutral
//! request/response.
//!
//! The Messages API differs from OpenAI Chat Completions in three ways this
//! module bridges: the system prompt is a **top-level field** (not a message
//! role), tool use/results are **content blocks** inside user/assistant
//! messages (there is no `tool` role), and there is **no `response_format`** —
//! structured output is obtained by forcing a single tool whose `input_schema`
//! is the desired shape.

use std::collections::HashMap;

use pocopine_agenkit::server::{FinishReason, GenerateRequest, GenerateResponse};
use pocopine_agenkit_core::{Content, Message, Role, ToolCall, ToolDescriptor, Usage};
use serde::{Deserialize, Serialize};

/// Name of the synthetic tool used to coax schema-shaped structured output.
/// Conforms to Anthropic's `^[a-zA-Z0-9_-]{1,64}$` tool-name rule.
pub(crate) const STRUCTURED_TOOL_NAME: &str = "structured_output";

// ---- tool-name sanitization -------------------------------------------------

/// Anthropic requires tool names to match `^[a-zA-Z0-9_-]{1,64}$` (same rule as
/// OpenAI). Map every other byte to `_` and cap at 64; an empty/all-invalid
/// name becomes `_`. The provider sanitizes on the way out and reverses the
/// mapping on the way back (see [`tool_name_map`]).
pub(crate) fn sanitize_tool_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    out.truncate(64);
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Reverse map (sanitized name → original tool id) for a request's tools, so a
/// tool call the model makes under the sanitized name dispatches to the real
/// tool. Identity for already-valid ids.
pub(crate) fn tool_name_map(tools: &[ToolDescriptor]) -> HashMap<String, String> {
    tools
        .iter()
        .map(|tool| (sanitize_tool_name(&tool.id), tool.id.clone()))
        .collect()
}

/// Resolve a wire tool-call name back to the original tool id; passes names
/// through unchanged when there is no mapping.
pub(crate) fn resolve_tool_name(name: String, name_map: &HashMap<String, String>) -> String {
    name_map.get(&name).cloned().unwrap_or(name)
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream: Option<bool>,
}

impl MessagesRequest {
    /// Map a neutral request. `stream` toggles SSE; `default_max_tokens` fills
    /// the required cap when the request omits one.
    pub(crate) fn from_agenkit(
        request: &GenerateRequest,
        stream: bool,
        default_max_tokens: u32,
    ) -> Self {
        // The Messages API has no system role: hoist `request.system` plus any
        // System-role messages into the top-level `system` field.
        let mut system = request.system.clone();
        for message in &request.messages {
            if message.role == Role::System {
                append_system(&mut system, message.content.as_text());
            }
        }

        let messages = fold_messages(&request.messages);

        let mut tools: Vec<WireTool> = request
            .tools
            .iter()
            .map(WireTool::from_descriptor)
            .collect();
        let has_user_tools = !tools.is_empty();

        // Structured output: Anthropic has no `response_format`.
        let mut tool_choice = None;
        if let Some(schema) = &request.json_schema {
            if !has_user_tools {
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
                // With user tools present we cannot force the structured tool —
                // Anthropic forces at most one tool per turn, which would suppress
                // the user tools (`tool_choice: tool` prefills a tool_use and emits
                // no other tools). Instead instruct the model to emit the
                // schema-shaped JSON as its final answer once it is done calling
                // tools; the runtime parses that JSON. This is the documented
                // "auto + prompt" approach and is the path the agent loop takes.
                append_system(&mut system, structured_instruction(schema));
            }
        }

        Self {
            model: request.model.model().to_string(),
            max_tokens: request.max_tokens.unwrap_or(default_max_tokens),
            system,
            messages,
            tools,
            tool_choice,
            stream: stream.then_some(true),
        }
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
fn fold_messages(messages: &[Message]) -> Vec<WireMessage> {
    let mut folded: Vec<WireMessage> = Vec::new();
    for message in messages {
        let (role, blocks) = match message.role {
            Role::System => continue, // hoisted to top-level `system`
            Role::User => ("user", text_block(&message.content).into_iter().collect()),
            Role::Assistant => assistant_blocks(message),
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

fn assistant_blocks(message: &Message) -> (&'static str, Vec<ContentBlock>) {
    let mut blocks: Vec<ContentBlock> = text_block(&message.content).into_iter().collect();
    for call in &message.tool_calls {
        blocks.push(ContentBlock::ToolUse {
            id: call.id.clone(),
            name: sanitize_tool_name(&call.tool_id),
            input: call.args.clone(),
        });
    }
    ("assistant", blocks)
}

fn tool_result_blocks(message: &Message) -> Vec<ContentBlock> {
    let Some(tool_use_id) = message.tool_call_id.clone() else {
        return Vec::new();
    };
    vec![ContentBlock::ToolResult {
        tool_use_id,
        content: message.content.as_text(),
    }]
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
    fn from_descriptor(descriptor: &ToolDescriptor) -> Self {
        let input_schema = descriptor
            .input
            .json_schema
            .clone()
            .map(|schema| normalize_schema(&schema))
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        Self {
            name: sanitize_tool_name(&descriptor.id),
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
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    /// Ignore block types we don't model (e.g. `thinking`).
    #[serde(other)]
    Other,
}

impl MessagesResponse {
    pub(crate) fn into_agenkit(self, name_map: &HashMap<String, String>) -> GenerateResponse {
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut structured: Option<serde_json::Value> = None;

        for block in self.content {
            match block {
                ResponseBlock::Text { text: fragment } => text.push_str(&fragment),
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
                        tool_calls.push(ToolCall::new(
                            id,
                            resolve_tool_name(name, name_map),
                            input,
                        ));
                    }
                }
                ResponseBlock::Other => {}
            }
        }

        let mut response = match structured {
            Some(value) => GenerateResponse::structured(value),
            None => GenerateResponse::text(text),
        };
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
}

impl WireUsage {
    pub(crate) fn into_usage(self) -> Usage {
        Usage::new(self.input_tokens, self.output_tokens)
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
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_agenkit_core::{Content, ModelRef};

    fn to_value(request: &MessagesRequest) -> serde_json::Value {
        serde_json::to_value(request).unwrap()
    }

    #[test]
    fn sanitize_tool_name_conforms_to_pattern() {
        assert_eq!(sanitize_tool_name("get_weather"), "get_weather");
        assert_eq!(sanitize_tool_name("weather.lookup"), "weather_lookup");
        assert_eq!(sanitize_tool_name("café"), "caf_");
        assert_eq!(sanitize_tool_name(&"x".repeat(100)).len(), 64);
        assert_eq!(sanitize_tool_name(""), "_");
    }

    #[test]
    fn hoists_system_field_and_system_messages() {
        let request = GenerateRequest {
            model: ModelRef::new("anthropic/claude-opus-4-8"),
            system: Some("be brief".to_string()),
            messages: vec![
                Message::new(Role::System, "also be kind"),
                Message::new(Role::User, "hi"),
            ],
            ..GenerateRequest::default()
        };
        let wire = MessagesRequest::from_agenkit(&request, false, 4096);
        assert_eq!(wire.system.as_deref(), Some("be brief\nalso be kind"));
        // The system message is hoisted out, leaving only the user turn.
        let value = to_value(&wire);
        assert_eq!(value["messages"].as_array().unwrap().len(), 1);
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["max_tokens"], 4096);
    }

    #[test]
    fn forces_a_tool_for_structured_output_without_user_tools() {
        let request = GenerateRequest {
            model: ModelRef::new("anthropic/claude-opus-4-8"),
            messages: vec![Message::new(Role::User, "summarize")],
            json_schema: Some(serde_json::json!({"type": "object", "properties": {}})),
            ..GenerateRequest::default()
        };
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096));
        assert_eq!(value["tool_choice"]["type"], "tool");
        assert_eq!(value["tool_choice"]["name"], STRUCTURED_TOOL_NAME);
        assert_eq!(value["tools"][0]["name"], STRUCTURED_TOOL_NAME);
    }

    #[test]
    fn does_not_force_a_tool_when_user_tools_are_present() {
        let request = GenerateRequest {
            model: ModelRef::new("anthropic/claude-opus-4-8"),
            messages: vec![Message::new(Role::User, "summarize")],
            tools: vec![ToolDescriptor::new("search", "Search")],
            json_schema: Some(serde_json::json!({"type": "object"})),
            ..GenerateRequest::default()
        };
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096));
        assert!(value.get("tool_choice").is_none() || value["tool_choice"].is_null());
        assert_eq!(value["tools"][0]["name"], "search");
    }

    #[test]
    fn folds_assistant_tool_use_and_tool_result_turns() {
        let request = GenerateRequest {
            model: ModelRef::new("anthropic/claude-opus-4-8"),
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
        let value = to_value(&MessagesRequest::from_agenkit(&request, false, 4096));
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
        let name_map = tool_name_map(&[ToolDescriptor::new("weather.lookup", "w")]);
        // A real tool call: sanitized name maps back to the original id.
        let resp: MessagesResponse = serde_json::from_value(serde_json::json!({
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "weather_lookup", "input": {"city": "Paris"}}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        }))
        .unwrap();
        let mapped = resp.into_agenkit(&name_map);
        assert_eq!(mapped.tool_calls.len(), 1);
        assert_eq!(mapped.tool_calls[0].tool_id, "weather.lookup");

        // The forced structured tool surfaces as a structured value, not a call.
        let resp: MessagesResponse = serde_json::from_value(serde_json::json!({
            "content": [{"type": "tool_use", "id": "toolu_2", "name": STRUCTURED_TOOL_NAME, "input": {"title": "X"}}],
            "stop_reason": "tool_use"
        }))
        .unwrap();
        let mapped = resp.into_agenkit(&HashMap::new());
        assert!(mapped.tool_calls.is_empty());
        assert_eq!(
            mapped.structured_value(),
            Some(&serde_json::json!({"title": "X"}))
        );
    }
}
