//! OpenAI Chat Completions wire types and the mapping to/from Agenkit's
//! neutral request/response.

use pocopine_agenkit::server::{FinishReason, GenerateRequest, GenerateResponse};
use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Content, ContentPart, Message, ModelRef, Role, ThinkingLevel,
    ToolCall, ToolNameMap, Usage,
};
use serde::{Deserialize, Serialize};

use crate::{MaxTokensParam, ThinkingParam};

/// Whether a model is reasoning-capable per the W1 catalog. Only such models get
/// a `reasoning_effort`; everything else ignores [`ThinkingLevel`]. An unknown
/// alias (e.g. an OpenRouter model the catalog doesn't index) is treated as
/// non-reasoning.
fn model_supports_reasoning(model: &ModelRef) -> bool {
    pocopine_agenkit::server::catalog::lookup(model)
        .map(|m| m.reasoning)
        .unwrap_or(false)
}

/// Map a [`ThinkingLevel`] to OpenAI's `reasoning_effort`. `Off` requests none.
fn reasoning_effort(level: ThinkingLevel) -> Option<&'static str> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal => Some("minimal"),
        ThinkingLevel::Low => Some("low"),
        ThinkingLevel::Medium => Some("medium"),
        ThinkingLevel::High => Some("high"),
    }
}

// Tool-name sanitization and the collision-safe reverse map live in
// `pocopine_agenkit_core::tool_name` (shared with the Anthropic provider).

// ---- request ----------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct ChatRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<WireTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<ResponseFormat>,
    /// Legacy output-token cap, accepted by most compatible gateways.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_tokens: Option<u32>,
    /// Output-token cap required by OpenAI's o-series / gpt-5-class models
    /// (which reject `max_tokens`). Exactly one of the two is ever set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream_options: Option<StreamOptions>,
    /// Reasoning budget for o-series / reasoning models (roadmap W4); omitted
    /// unless requested for a reasoning-capable model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<&'static str>,
    /// DashScope's thinking toggle (Qwen3 family), sent instead of
    /// `reasoning_effort` under [`ThinkingParam::EnableThinking`]: `true` for
    /// any requested level on a reasoning-capable model. `Off` omits the field
    /// (the endpoint's default stands; an explicit `false` rides
    /// `provider_options`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enable_thinking: Option<bool>,
    /// Provider-specific extra body fields
    /// (`GenerateRequest::provider_options`), flattened verbatim into the top
    /// level of the request — e.g. DashScope's `enable_search`. Serialized
    /// after the typed fields, so a duplicated key reaches the provider twice
    /// (most JSON parsers keep the later one).
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
pub(crate) struct StreamOptions {
    pub(crate) include_usage: bool,
}

impl ChatRequest {
    /// Map a neutral request. `stream` toggles SSE; `strict_schema` selects
    /// native `json_schema` structured output (vs `json_object`);
    /// `max_tokens_param` selects which output-token field to send;
    /// `thinking_param` selects which reasoning field a thinking level maps to.
    ///
    /// Errors (config) when the request carries media this wire cannot deliver
    /// — see [`GenerateRequest::ensure_media_support`] — instead of silently
    /// dropping an attachment.
    pub(crate) fn from_agenkit(
        request: &GenerateRequest,
        stream: bool,
        strict_schema: bool,
        max_tokens_param: MaxTokensParam,
        thinking_param: ThinkingParam,
    ) -> AgenkitResult<Self> {
        request.ensure_media_support()?;
        let has_tools = !request.tools.is_empty();
        // Collision-safe tool-name map for this request's tools, used for both
        // the tool defs and any replayed assistant tool calls in history.
        let names = ToolNameMap::from_descriptors(&request.tools);
        // Whether the request will get native strict `json_schema` enforcement
        // (vs the `json_object` fallback, which guarantees valid JSON but NOT the
        // schema shape) — mirrors the decision in `response_format`.
        let use_strict = request
            .json_schema
            .as_ref()
            .is_some_and(|schema| strict_schema && !has_tools && is_strict_compatible(schema));

        let mut messages = Vec::new();
        let mut system = request.system.clone();
        // `json_object` mode requires the word "json" in the conversation, so fold
        // a structured instruction into the system turn. Under native strict mode
        // the shape is enforced by the API; otherwise (the `json_object` fallback,
        // which the agent loop always hits since it sends tools) the model gets
        // validity but no shape — so convey the full schema, or it guesses field
        // names and the runtime's `from_value::<T>` fails.
        if let Some(schema) = &request.json_schema {
            let note = if use_strict {
                "Respond with a single valid JSON object.".to_string()
            } else {
                format!(
                    "Respond with a single JSON object that conforms to this JSON Schema, \
                     and nothing else:\n{schema}"
                )
            };
            system = Some(match system {
                Some(existing) => format!("{existing}\n{note}"),
                None => note,
            });
        }
        if let Some(system) = system {
            messages.push(WireMessage::text("system", system));
        }
        messages.extend(
            request
                .messages
                .iter()
                .map(|message| WireMessage::from_message(message, &names)),
        );
        // o-series / gpt-5-class models reject `max_tokens`; compatible gateways
        // reject `max_completion_tokens`. Send exactly the one the endpoint wants.
        let (max_tokens, max_completion_tokens) = match max_tokens_param {
            MaxTokensParam::MaxTokens => (request.max_tokens, None),
            MaxTokensParam::MaxCompletionTokens => (None, request.max_tokens),
        };
        // Reasoning (W4): only for reasoning-capable models — `Off` and
        // unknown/non-reasoning models send nothing. The endpoint dialect picks
        // the field: OpenAI-style `reasoning_effort`, or DashScope's boolean
        // `enable_thinking` (which has no level granularity).
        let wants_reasoning =
            model_supports_reasoning(&request.model) && request.thinking != ThinkingLevel::Off;
        let (reasoning_effort, enable_thinking) = match thinking_param {
            ThinkingParam::ReasoningEffort => {
                (wants_reasoning.then(|| reasoning_effort(request.thinking)).flatten(), None)
            }
            ThinkingParam::EnableThinking => (None, wants_reasoning.then_some(true)),
        };
        Ok(Self {
            model: request.model.model().to_string(),
            messages,
            tools: request
                .tools
                .iter()
                .map(|tool| WireTool::from_descriptor(tool, &names))
                .collect(),
            response_format: response_format(
                request.json_schema.as_ref(),
                strict_schema,
                has_tools,
            ),
            max_tokens,
            max_completion_tokens,
            stream: stream.then_some(true),
            stream_options: stream.then_some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort,
            enable_thinking,
            extra: request.provider_options.clone(),
        })
    }
}

fn response_format(
    schema: Option<&serde_json::Value>,
    strict_schema: bool,
    has_tools: bool,
) -> Option<ResponseFormat> {
    let schema = schema?;
    // Native strict `json_schema` only when asked, the schema is one OpenAI's
    // strict subset can express, AND no tools are offered — strict structured
    // output forces the model to emit the schema, which suppresses tool calls
    // (the agent loop sends both). In every other case fall back to JSON-object
    // mode (the model still returns a JSON object; the runtime validates it).
    if strict_schema && !has_tools && is_strict_compatible(schema) {
        let mut strict = schema.clone();
        make_strict(&mut strict);
        Some(ResponseFormat::JsonSchema {
            json_schema: JsonSchemaSpec {
                name: "agenkit_output".to_string(),
                strict: true,
                schema: strict,
            },
        })
    } else {
        Some(ResponseFormat::JsonObject)
    }
}

/// Whether a derived schema is one OpenAI strict `json_schema` mode can express:
/// a concrete root object (not a bare `$ref`/marker) with no
/// strict-incompatible construct anywhere. Anything else degrades to
/// `json_object`.
fn is_strict_compatible(schema: &serde_json::Value) -> bool {
    schema.get("properties").is_some() && !contains_strict_incompatible(schema)
}

/// True if any node carries a construct OpenAI strict mode rejects with a hard
/// 400: a schema-valued `additionalProperties` (an open map like
/// `HashMap<String, T>`), an `allOf`/`oneOf` combinator (strict supports only
/// `anyOf`), or a draft-07 `definitions` block (strict expects `$defs`).
/// schemars 0.8 emits these routinely — a doc-commented nested field wraps its
/// `$ref` in `allOf`, a data-carrying enum becomes `oneOf` — so they must
/// degrade to `json_object` instead of failing the request at the provider.
fn contains_strict_incompatible(schema: &serde_json::Value) -> bool {
    match schema {
        serde_json::Value::Object(map) => {
            matches!(map.get("additionalProperties"), Some(v) if v.is_object())
                || map.contains_key("allOf")
                || map.contains_key("oneOf")
                || map.contains_key("definitions")
                || map.values().any(contains_strict_incompatible)
        }
        serde_json::Value::Array(items) => items.iter().any(contains_strict_incompatible),
        _ => false,
    }
}

/// Best-effort transform of a derived JSON Schema into OpenAI's strict subset:
/// every object gets `additionalProperties: false` and *all* properties listed
/// in `required` (strict mode requires this) — but a property that `schemars`
/// left optional (an `Option<T>` field, absent from the original `required`) is
/// made nullable so it can still be omitted in spirit. `$schema`/`format` (which
/// strict mode rejects) are dropped. Applied recursively through `properties`,
/// `$defs`, `items`, and `anyOf`/`allOf`/`oneOf`.
pub(crate) fn make_strict(schema: &mut serde_json::Value) {
    let serde_json::Value::Object(map) = schema else {
        return;
    };
    map.remove("$schema");
    map.remove("format");

    if map.get("properties").map(|p| p.is_object()) == Some(true) {
        // Capture which keys were optional before we overwrite `required`.
        let originally_required: std::collections::HashSet<String> = map
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let Some(serde_json::Value::Object(properties)) = map.get_mut("properties") else {
            unreachable!("checked is_object above");
        };
        let keys: Vec<serde_json::Value> = properties
            .keys()
            .cloned()
            .map(serde_json::Value::String)
            .collect();
        for (key, value) in properties.iter_mut() {
            make_strict(value);
            if !originally_required.contains(key) {
                make_nullable(value);
            }
        }
        map.insert(
            "additionalProperties".to_string(),
            serde_json::Value::Bool(false),
        );
        map.insert("required".to_string(), serde_json::Value::Array(keys));
    }
    for defs_key in ["$defs", "definitions"] {
        if let Some(serde_json::Value::Object(defs)) = map.get_mut(defs_key) {
            for value in defs.values_mut() {
                make_strict(value);
            }
        }
    }
    if let Some(items) = map.get_mut("items") {
        make_strict(items);
    }
    for combinator in ["anyOf", "allOf", "oneOf"] {
        if let Some(serde_json::Value::Array(variants)) = map.get_mut(combinator) {
            for value in variants.iter_mut() {
                make_strict(value);
            }
        }
    }
}

/// Allow `null` for an (originally-optional) property's type, so a strict schema
/// that lists it as `required` still lets the model omit it by emitting `null`.
fn make_nullable(schema: &mut serde_json::Value) {
    let serde_json::Value::Object(map) = schema else {
        return;
    };
    match map.get_mut("type") {
        Some(serde_json::Value::String(ty)) => {
            if ty != "null" {
                let ty = ty.clone();
                map.insert(
                    "type".to_string(),
                    serde_json::Value::Array(vec![
                        serde_json::Value::String(ty),
                        serde_json::Value::String("null".to_string()),
                    ]),
                );
            }
        }
        Some(serde_json::Value::Array(types)) => {
            if !types.iter().any(|v| v == "null") {
                types.push(serde_json::Value::String("null".to_string()));
            }
        }
        // No scalar `type` (e.g. a `$ref` or `anyOf` optional): leave as-is —
        // adding null would require rewriting the combinator; the field is still
        // `required`, which is the strict-mode default.
        _ => {}
    }
}

#[derive(Serialize)]
pub(crate) struct WireMessage {
    pub(crate) role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<WireContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
}

/// A message body: the plain string shape for text-only content (what every
/// gateway accepts), or the content-parts array when media rides along.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum WireContent {
    Text(String),
    Parts(Vec<WireContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WireContentPart {
    Text { text: String },
    ImageUrl { image_url: WireImageUrl },
}

#[derive(Serialize)]
pub(crate) struct WireImageUrl {
    pub(crate) url: String,
}

/// Map neutral content to the wire body. Text-only content keeps the plain
/// string shape (unchanged, maximum gateway compatibility); content carrying
/// media becomes a parts array in author order, an image referenced by `url`
/// or inlined as a `data:` URI. `GenerateRequest::ensure_media_support`
/// (checked at request mapping) guarantees every media part is an image with a
/// source, and that media only occurs in user messages.
fn wire_content(content: &Content) -> WireContent {
    let has_media = content
        .parts
        .iter()
        .any(|part| matches!(part, ContentPart::Media(_)));
    if !has_media {
        return WireContent::Text(content.as_text());
    }
    let mut parts = Vec::new();
    for part in &content.parts {
        match part {
            ContentPart::Text { text } => parts.push(WireContentPart::Text { text: text.clone() }),
            ContentPart::Media(media) => {
                let url = media.url.clone().unwrap_or_else(|| {
                    format!(
                        "data:{};base64,{}",
                        media.media_type,
                        media.data_base64.as_deref().unwrap_or_default()
                    )
                });
                parts.push(WireContentPart::ImageUrl {
                    image_url: WireImageUrl { url },
                });
            }
            // Parity with `Content::as_text()`: Json/Thinking parts never cross
            // the wire as user-visible content.
            ContentPart::Json { .. } | ContentPart::Thinking { .. } => {}
        }
    }
    WireContent::Parts(parts)
}

impl WireMessage {
    pub(crate) fn text(role: &'static str, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(WireContent::Text(content.into())),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub(crate) fn from_message(message: &Message, names: &ToolNameMap) -> Self {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let tool_calls = message
            .tool_calls
            .iter()
            .map(|call| WireToolCall::from_tool_call(call, names))
            .collect();
        let content = if message.content.is_empty() && !message.tool_calls.is_empty() {
            None
        } else {
            Some(wire_content(&message.content))
        };
        Self {
            role,
            content,
            tool_calls,
            tool_call_id: message.tool_call_id.clone(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct WireToolCall {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) function: WireFunctionCall,
}

impl WireToolCall {
    fn from_tool_call(call: &ToolCall, names: &ToolNameMap) -> Self {
        Self {
            id: call.id.clone(),
            kind: "function",
            function: WireFunctionCall {
                // Match the wire name the tool was offered under, so a replayed
                // assistant turn stays consistent with the tool defs.
                name: names.wire(&call.tool_id),
                arguments: serde_json::to_string(&call.args).unwrap_or_else(|_| "{}".to_string()),
            },
        }
    }
}

#[derive(Serialize)]
pub(crate) struct WireFunctionCall {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Serialize)]
pub(crate) struct WireTool {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) function: WireFunctionDef,
}

impl WireTool {
    fn from_descriptor(
        descriptor: &pocopine_agenkit_core::ToolDescriptor,
        names: &ToolNameMap,
    ) -> Self {
        let parameters = descriptor
            .input
            .json_schema
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        Self {
            kind: "function",
            function: WireFunctionDef {
                name: names.wire(&descriptor.id),
                description: descriptor.description.clone(),
                parameters,
            },
        }
    }
}

#[derive(Serialize)]
pub(crate) struct WireFunctionDef {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: serde_json::Value,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseFormat {
    JsonObject,
    JsonSchema { json_schema: JsonSchemaSpec },
}

#[derive(Serialize)]
pub(crate) struct JsonSchemaSpec {
    pub(crate) name: String,
    pub(crate) strict: bool,
    pub(crate) schema: serde_json::Value,
}

// ---- non-streaming response -------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct ChatResponse {
    #[serde(default)]
    pub(crate) choices: Vec<Choice>,
    #[serde(default)]
    pub(crate) usage: Option<WireUsage>,
}

impl ChatResponse {
    pub(crate) fn into_agenkit(self, names: &ToolNameMap) -> AgenkitResult<GenerateResponse> {
        let choice = self
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| AgenkitError::provider("OpenAI response contained no choices"))?;

        // Reasoning ("thinking") content (W4): OpenAI-compatible reasoning models
        // return it in `reasoning_content`. It rides the response server-side
        // (replay/observability) but `Content::as_text()` skips it, so it never
        // reaches user-visible text. OpenAI exposes no replay signature → `None`.
        let text = choice.message.content.unwrap_or_default();
        let content = match choice.message.reasoning_content.filter(|r| !r.is_empty()) {
            Some(reasoning) => Content::from_parts(vec![
                ContentPart::thinking(reasoning, None),
                ContentPart::text(text),
            ]),
            None => Content::text(text),
        };
        let tool_calls = choice
            .message
            .tool_calls
            .into_iter()
            .map(|call| call.into_call(names))
            .collect();
        let finish_reason = finish_reason(choice.finish_reason.as_deref());
        let usage = self.usage.map(WireUsage::into_usage);

        Ok(GenerateResponse {
            content,
            tool_calls,
            usage,
            finish_reason,
        })
    }
}

#[derive(Deserialize)]
pub(crate) struct Choice {
    pub(crate) message: ChoiceMessage,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ChoiceMessage {
    #[serde(default)]
    pub(crate) content: Option<String>,
    /// Reasoning text from a reasoning model (W4). Non-standard but emitted by
    /// DeepSeek-R1, many OpenRouter reasoning models, and OpenAI-compatible
    /// gateways. Absent on non-reasoning responses.
    #[serde(default)]
    pub(crate) reasoning_content: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<ResponseToolCall>,
}

#[derive(Deserialize)]
pub(crate) struct ResponseToolCall {
    pub(crate) id: String,
    pub(crate) function: ResponseFunction,
}

impl ResponseToolCall {
    fn into_call(self, names: &ToolNameMap) -> ToolCall {
        // Empty (zero-argument tool) or unparseable args default to `{}`, not
        // `null` — a struct-typed tool input deserializes from `{}` but not from
        // `null`, so `null` would turn a valid no-arg call into a cryptic
        // "expected object" validation error downstream. Mirrors the streaming
        // path's `emit_tool_calls`.
        let args = if self.function.arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&self.function.arguments).unwrap_or_else(|_| serde_json::json!({}))
        };
        ToolCall::new(self.id, names.resolve(&self.function.name), args)
    }
}

#[derive(Deserialize)]
pub(crate) struct ResponseFunction {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Deserialize)]
pub(crate) struct WireUsage {
    #[serde(default)]
    pub(crate) prompt_tokens: u64,
    #[serde(default)]
    pub(crate) completion_tokens: u64,
    // OpenAI's `prompt_tokens` INCLUDES the cached subset; subtract it so
    // `Usage.input_tokens` is the uncached count.
    #[serde(default)]
    pub(crate) prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
pub(crate) struct PromptTokensDetails {
    #[serde(default)]
    pub(crate) cached_tokens: u64,
}

impl WireUsage {
    pub(crate) fn into_usage(self) -> Usage {
        let cached = self
            .prompt_tokens_details
            .map(|d| d.cached_tokens)
            .unwrap_or(0);
        // `prompt_tokens` includes `cached`; report the uncached remainder as
        // input. OpenAI doesn't bill cache writes, so cache_creation = 0.
        Usage::new(
            self.prompt_tokens.saturating_sub(cached),
            self.completion_tokens,
        )
        .with_cache(cached, 0)
    }
}

pub(crate) fn finish_reason(raw: Option<&str>) -> FinishReason {
    match raw {
        Some("length") => FinishReason::Length,
        Some("tool_calls") => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    }
}

// ---- streaming response -----------------------------------------------------

/// One SSE `data:` chunk from a streaming completion.
#[derive(Deserialize)]
pub(crate) struct StreamEvent {
    #[serde(default)]
    pub(crate) choices: Vec<StreamChoice>,
    #[serde(default)]
    pub(crate) usage: Option<WireUsage>,
}

#[derive(Deserialize)]
pub(crate) struct StreamChoice {
    #[serde(default)]
    pub(crate) delta: StreamDelta,
}

#[derive(Deserialize, Default)]
pub(crate) struct StreamDelta {
    #[serde(default)]
    pub(crate) content: Option<String>,
    /// Incremental reasoning text from a reasoning model (W4).
    #[serde(default)]
    pub(crate) reasoning_content: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<StreamToolCallDelta>,
}

/// A partial tool call: `id`/`name` arrive once, `arguments` in fragments,
/// correlated by `index`.
#[derive(Deserialize)]
pub(crate) struct StreamToolCallDelta {
    #[serde(default)]
    pub(crate) index: u32,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) function: Option<StreamFunctionDelta>,
}

#[derive(Deserialize)]
pub(crate) struct StreamFunctionDelta {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_strict_locks_objects_down() {
        let mut schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "count": {"type": "integer", "format": "uint32"}
            }
        });
        make_strict(&mut schema);
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
        assert!(required.contains(&serde_json::json!("title")));
        assert!(required.contains(&serde_json::json!("count")));
        assert!(schema.get("$schema").is_none());
        assert!(schema["properties"]["count"].get("format").is_none());
    }

    // Tool-name sanitization + the collision-safe reverse map are tested in
    // `pocopine_agenkit_core::tool_name`.

    #[test]
    fn provider_options_flatten_into_the_request_body() {
        let mut request = GenerateRequest {
            model: ModelRef::new("qwen/qwen-plus"),
            messages: vec![Message::new(Role::User, "hi")],
            ..GenerateRequest::default()
        };
        request
            .provider_options
            .insert("enable_search".to_string(), serde_json::json!(true));
        request.provider_options.insert(
            "search_options".to_string(),
            serde_json::json!({"forced_search": true}),
        );
        let wire =
            ChatRequest::from_agenkit(&request, false, false, MaxTokensParam::MaxTokens, ThinkingParam::ReasoningEffort).unwrap();
        let body = serde_json::to_value(&wire).unwrap();
        assert_eq!(body["enable_search"], serde_json::json!(true));
        assert_eq!(body["search_options"]["forced_search"], serde_json::json!(true));
        // Typed fields are unaffected.
        assert_eq!(body["model"], serde_json::json!("qwen-plus"));
    }

    #[test]
    fn thinking_param_selects_the_reasoning_field() {
        // qwen-plus is reasoning-capable per the catalog.
        let request = GenerateRequest {
            model: ModelRef::new("qwen/qwen-plus"),
            messages: vec![Message::new(Role::User, "hi")],
            thinking: ThinkingLevel::Medium,
            ..GenerateRequest::default()
        };
        // OpenAI dialect: `reasoning_effort`, no `enable_thinking`.
        let wire = ChatRequest::from_agenkit(
            &request,
            false,
            false,
            MaxTokensParam::MaxTokens,
            ThinkingParam::ReasoningEffort,
        )
        .unwrap();
        let body = serde_json::to_value(&wire).unwrap();
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("enable_thinking").is_none());
        // DashScope dialect: `enable_thinking: true`, no `reasoning_effort`.
        let wire = ChatRequest::from_agenkit(
            &request,
            false,
            false,
            MaxTokensParam::MaxTokens,
            ThinkingParam::EnableThinking,
        )
        .unwrap();
        let body = serde_json::to_value(&wire).unwrap();
        assert_eq!(body["enable_thinking"], serde_json::json!(true));
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn thinking_off_and_non_reasoning_models_send_no_reasoning_field() {
        let params = [ThinkingParam::ReasoningEffort, ThinkingParam::EnableThinking];
        // `Off` on a reasoning-capable model: nothing, under either dialect.
        let off = GenerateRequest {
            model: ModelRef::new("qwen/qwen-plus"),
            messages: vec![Message::new(Role::User, "hi")],
            ..GenerateRequest::default()
        };
        // A level on a model the catalog marks non-reasoning: nothing either.
        let non_reasoning = GenerateRequest {
            model: ModelRef::new("openai/gpt-3.5-turbo"),
            messages: vec![Message::new(Role::User, "hi")],
            thinking: ThinkingLevel::High,
            ..GenerateRequest::default()
        };
        for request in [&off, &non_reasoning] {
            for param in params {
                let wire = ChatRequest::from_agenkit(
                    request,
                    false,
                    false,
                    MaxTokensParam::MaxTokens,
                    param,
                )
                .unwrap();
                let body = serde_json::to_value(&wire).unwrap();
                assert!(body.get("reasoning_effort").is_none());
                assert!(body.get("enable_thinking").is_none());
            }
        }
    }

    #[test]
    fn user_images_become_content_parts() {
        use pocopine_agenkit_core::MediaPart;
        let request = GenerateRequest {
            model: ModelRef::new("openai/gpt-4o"),
            messages: vec![Message::new(
                Role::User,
                Content::from_parts(vec![
                    ContentPart::text("what is in this image?"),
                    ContentPart::Media(MediaPart {
                        media_type: "image/png".to_string(),
                        url: Some("https://example.com/cat.png".to_string()),
                        data_base64: None,
                        name: None,
                    }),
                    ContentPart::Media(MediaPart {
                        media_type: "image/jpeg".to_string(),
                        url: None,
                        data_base64: Some("QUJD".to_string()),
                        name: None,
                    }),
                ]),
            )],
            ..GenerateRequest::default()
        };
        let wire =
            ChatRequest::from_agenkit(&request, false, false, MaxTokensParam::MaxTokens, ThinkingParam::ReasoningEffort).unwrap();
        let body = serde_json::to_value(&wire).unwrap();
        let content = &body["messages"][0]["content"];
        assert!(content.is_array(), "media content uses the parts shape");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "what is in this image?");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "https://example.com/cat.png");
        // Inline base64 is carried as a data: URI.
        assert_eq!(content[2]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
    }

    #[test]
    fn text_only_content_keeps_the_plain_string_shape() {
        // Gateways universally accept the plain string body; text-only messages
        // must not silently migrate to the parts shape.
        let request = GenerateRequest {
            model: ModelRef::new("openai/gpt-4o"),
            messages: vec![Message::new(Role::User, "hi")],
            ..GenerateRequest::default()
        };
        let wire =
            ChatRequest::from_agenkit(&request, false, false, MaxTokensParam::MaxTokens, ThinkingParam::ReasoningEffort).unwrap();
        let body = serde_json::to_value(&wire).unwrap();
        assert_eq!(body["messages"][0]["content"], serde_json::json!("hi"));
    }

    #[test]
    fn media_the_wire_cannot_carry_errors_instead_of_dropping() {
        use pocopine_agenkit_core::MediaPart;
        // Audio has no Chat Completions mapping here yet — the request must
        // fail loudly, not silently strip the attachment.
        let request = GenerateRequest {
            model: ModelRef::new("openai/gpt-4o"),
            messages: vec![Message::new(
                Role::User,
                Content::from_parts(vec![ContentPart::Media(MediaPart {
                    media_type: "audio/mpeg".to_string(),
                    url: Some("https://example.com/a.mp3".to_string()),
                    data_base64: None,
                    name: None,
                })]),
            )],
            ..GenerateRequest::default()
        };
        let result = ChatRequest::from_agenkit(&request, false, false, MaxTokensParam::MaxTokens, ThinkingParam::ReasoningEffort);
        assert!(result.is_err());
    }

    #[test]
    fn tool_descriptor_schema_becomes_function_parameters() {
        use pocopine_agenkit_core::{SchemaRef, ToolDescriptor};
        // The runtime derives this from the tool's `Input` type; it must land in
        // the function's `parameters` so the model sees the argument shape.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"]
        });
        let descriptor = ToolDescriptor::new("search_docs", "Search project docs")
            .with_input(SchemaRef::named("SearchInput").with_json_schema(schema.clone()));

        let names = ToolNameMap::from_descriptors(std::slice::from_ref(&descriptor));
        let wire = WireTool::from_descriptor(&descriptor, &names);
        assert_eq!(wire.function.name, "search_docs");
        assert_eq!(wire.function.parameters, schema);
    }

    #[test]
    fn tool_without_a_schema_falls_back_to_an_object() {
        use pocopine_agenkit_core::ToolDescriptor;
        let descriptor = ToolDescriptor::new("noop", "Does nothing");
        let names = ToolNameMap::from_descriptors(std::slice::from_ref(&descriptor));
        let wire = WireTool::from_descriptor(&descriptor, &names);
        assert_eq!(
            wire.function.parameters,
            serde_json::json!({"type": "object"})
        );
    }

    #[test]
    fn real_schema_selects_json_schema_mode_only_when_strict() {
        let schema = serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}});
        assert!(matches!(
            response_format(Some(&schema), true, false),
            Some(ResponseFormat::JsonSchema { .. })
        ));
        assert!(matches!(
            response_format(Some(&schema), false, false),
            Some(ResponseFormat::JsonObject)
        ));
        // A marker schema (no properties) always uses json_object.
        let marker = serde_json::json!({"type": "object"});
        assert!(matches!(
            response_format(Some(&marker), true, false),
            Some(ResponseFormat::JsonObject)
        ));
    }

    #[test]
    fn tools_force_json_object_over_strict_schema() {
        // Strict json_schema would suppress tool calling, so a request that
        // carries tools degrades to json_object even with a real schema.
        let schema = serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}});
        assert!(matches!(
            response_format(Some(&schema), true, true),
            Some(ResponseFormat::JsonObject)
        ));
    }

    #[test]
    fn open_map_schema_falls_back_to_json_object() {
        // A HashMap<String, u32> field -> open `additionalProperties`, which
        // strict mode can't express; degrade instead of sending an invalid schema.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "counts": {"type": "object", "additionalProperties": {"type": "integer"}}
            }
        });
        assert!(matches!(
            response_format(Some(&schema), true, false),
            Some(ResponseFormat::JsonObject)
        ));
    }

    #[test]
    fn all_of_wrapped_ref_falls_back_to_json_object() {
        // schemars 0.8 wraps a doc-commented nested struct field in
        // `allOf: [{$ref}]`; strict mode rejects `allOf` with a 400, so the
        // request must degrade instead.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "nested": {
                    "allOf": [{"$ref": "#/definitions/Nested"}],
                    "description": "doc comment"
                }
            },
            "definitions": {
                "Nested": {"type": "object", "properties": {"x": {"type": "string"}}}
            }
        });
        assert!(matches!(
            response_format(Some(&schema), true, false),
            Some(ResponseFormat::JsonObject)
        ));
    }

    #[test]
    fn one_of_enum_falls_back_to_json_object() {
        // A data-carrying enum derives to `oneOf`, which strict mode rejects
        // (only `anyOf` is supported); degrade instead of a provider 400.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {"oneOf": [
                    {"type": "object", "properties": {"a": {"type": "string"}}},
                    {"type": "object", "properties": {"b": {"type": "integer"}}}
                ]}
            }
        });
        assert!(matches!(
            response_format(Some(&schema), true, false),
            Some(ResponseFormat::JsonObject)
        ));
    }

    #[test]
    fn ref_root_schema_falls_back_to_json_object() {
        // A newtype/enum root serializes as a bare `$ref`; make_strict can't lock
        // it, so it must degrade rather than send an under-constrained root.
        let schema = serde_json::json!({"$ref": "#/$defs/T", "$defs": {"T": {"type": "string"}}});
        assert!(matches!(
            response_format(Some(&schema), true, false),
            Some(ResponseFormat::JsonObject)
        ));
    }

    #[test]
    fn optional_fields_become_nullable_and_required() {
        // schemars leaves an Option<T> field out of `required`; strict mode wants
        // every field required, so it must be made nullable to stay omittable.
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "note": {"type": "string"}
            },
            "required": ["title"]
        });
        make_strict(&mut schema);
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 2, "all fields required under strict mode");
        // The required field keeps its scalar type; the optional one is nullable.
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert_eq!(
            schema["properties"]["note"]["type"],
            serde_json::json!(["string", "null"])
        );
    }

    #[test]
    fn wire_usage_subtracts_cached_from_prompt_tokens() {
        // OpenAI's prompt_tokens INCLUDES the cached subset; into_usage subtracts it.
        let u: super::WireUsage = serde_json::from_str(
            r#"{"prompt_tokens":150,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":120}}"#,
        )
        .unwrap();
        let usage = u.into_usage();
        assert_eq!(usage.input_tokens, 30); // 150 - 120
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.cache_read_tokens, 120);
        assert_eq!(usage.cache_creation_tokens, 0); // OpenAI doesn't bill cache writes
        // No details → no cached subset.
        let plain: super::WireUsage =
            serde_json::from_str(r#"{"prompt_tokens":10,"completion_tokens":2}"#).unwrap();
        let usage = plain.into_usage();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.cache_read_tokens, 0);
    }
}
