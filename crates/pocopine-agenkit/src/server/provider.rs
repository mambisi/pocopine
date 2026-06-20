//! Provider trait, request/response types, registry, and a deterministic mock
//! (RFC-093 Phase 2.1, §D15 DC-4).
//!
//! A [`Provider`] resolves a model alias to actual generation. The trait is
//! object-safe via boxed-future return types (mirroring
//! `pocopine_sync_query::source::Source`), so the registry stores
//! `Arc<dyn Provider>` directly with no separate `Dyn` shim. Provider
//! credentials live behind the implementation — they never appear in these
//! payloads (§D10).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Content, ContentPart, Message, ModelRef, ThinkingLevel, ToolCall,
    ToolDescriptor, Usage,
};

use super::credentials::ProviderCredential;

/// Per-request context handed to a [`Provider`] alongside the [`GenerateRequest`]
/// (roadmap W6). Carries the credential the runtime resolved for this call — from
/// a [`ProviderCredentials`](super::credentials::ProviderCredentials) store,
/// possibly per-[`Principal`](pocopine_auth::Principal) (BYOK).
///
/// `credential` is `None` when the store had nothing for this provider/principal;
/// the provider then falls back to the credential it was built with. The struct
/// is `#[non_exhaustive]` so future per-call inputs (deadline, request id) can be
/// added without breaking the trait again.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ProviderContext {
    /// A credential resolved for this request, overriding the provider's baked
    /// default. `None` ⇒ use the provider's built-in credential.
    pub credential: Option<ProviderCredential>,
}

impl ProviderContext {
    /// The per-request context for a resolved credential (`None` ⇒ the provider's
    /// built-in credential). The single construction point, so a future per-call
    /// field is added here, not at each resolve site. (Direct callers/tests use
    /// [`ProviderContext::default`].)
    pub(crate) fn for_request(credential: Option<ProviderCredential>) -> Self {
        Self { credential }
    }
}

/// A boxed, `Send` future — the object-safe return shape for async trait
/// methods across the runtime (§D15 DC-4).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed, `Send` stream — the object-safe return shape for streaming
/// generation.
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// What a provider supports beyond basic generation. The runtime uses native
/// support when present and degrades gracefully otherwise (§D13).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Native incremental streaming (otherwise a one-shot fallback is used).
    pub streaming: bool,
    /// Native schema-constrained structured output (otherwise JSON-mode +
    /// prompt instruction is used).
    pub json_schema: bool,
    /// Native tool/function calling.
    pub tools: bool,
}

impl ProviderCapabilities {
    /// A provider that supports everything.
    pub fn all() -> Self {
        Self {
            streaming: true,
            json_schema: true,
            tools: true,
        }
    }
}

/// One incremental piece of a streamed generation.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamChunk {
    /// A user-visible text fragment.
    Text(String),
    /// A fully-assembled tool call (the provider accumulates any partials).
    ToolCall(ToolCall),
    /// A completed reasoning ("thinking") block (roadmap W4). This is the
    /// **internal** provider→runtime channel only: the runtime folds it into the
    /// assembled response's content (server-side, for replay + observability) but
    /// never forwards it to the client `FlowStreamEvent` stream (§D10). The
    /// client-facing `ThinkingDelta` is W5.
    Thinking {
        /// The reasoning text.
        text: String,
        /// The provider's opaque signature, replayed verbatim next turn.
        signature: Option<String>,
    },
    /// Token usage, typically delivered once near the end.
    Usage(Usage),
}

/// Why a generation stopped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The model stopped naturally.
    #[default]
    Stop,
    /// The output was truncated by a token limit.
    Length,
    /// The model requested tool calls.
    ToolCalls,
}

/// A model generation request assembled by the [`crate::Ai`] builder.
#[derive(Clone, Debug, Default)]
pub struct GenerateRequest {
    /// The resolved model alias.
    pub model: ModelRef,
    /// Optional system prompt.
    pub system: Option<String>,
    /// The conversation/messages.
    pub messages: Vec<Message>,
    /// Tools the model may call.
    pub tools: Vec<ToolDescriptor>,
    /// When set, the model must return JSON matching this schema (structured
    /// output). The runtime validates the result before returning it.
    pub json_schema: Option<serde_json::Value>,
    /// Optional output token cap.
    pub max_tokens: Option<u32>,
    /// How much reasoning ("thinking") budget to request (roadmap W4). Defaults
    /// to [`ThinkingLevel::Off`]; providers only honour it for reasoning-capable
    /// models and ignore it otherwise.
    pub thinking: ThinkingLevel,
}

impl GenerateRequest {
    /// The concatenated text of every user message — the effective prompt.
    pub fn prompt_text(&self) -> String {
        self.messages
            .iter()
            .filter(|m| matches!(m.role, pocopine_agenkit_core::Role::User))
            .map(|m| m.content.as_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The text of the most recent message (a user prompt, or a tool result on
    /// a follow-up turn). The mock provider matches rules against this so a tool
    /// rule fires on the prompt turn and a later rule can fire on the tool
    /// result turn, rather than the prompt rule re-firing forever.
    pub fn last_message_text(&self) -> String {
        self.messages
            .last()
            .map(|m| m.content.as_text())
            .unwrap_or_default()
    }
}

/// A model generation response.
#[derive(Clone, Debug)]
pub struct GenerateResponse {
    /// The generated content.
    pub content: Content,
    /// Any tool calls the model requested.
    pub tool_calls: Vec<ToolCall>,
    /// Token usage, when the provider reports it.
    pub usage: Option<Usage>,
    /// Why generation stopped.
    pub finish_reason: FinishReason,
}

impl GenerateResponse {
    /// A plain text response.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: Content::text(text),
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: FinishReason::Stop,
        }
    }

    /// A structured response carrying a JSON value.
    pub fn structured(value: serde_json::Value) -> Self {
        Self {
            content: Content::from_parts(vec![ContentPart::Json { value }]),
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: FinishReason::Stop,
        }
    }

    /// Attach usage.
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// The response text (concatenated text parts).
    pub fn text_output(&self) -> String {
        self.content.as_text()
    }

    /// A textual rendering of the response: text parts if any, else a
    /// structured JSON value rendered as text. Used to accumulate streamed
    /// output and to parse structured results.
    pub fn display_text(&self) -> String {
        let text = self.content.as_text();
        if !text.is_empty() {
            return text;
        }
        self.structured_value()
            .map(|value| value.to_string())
            .unwrap_or_default()
    }

    /// The first JSON content part, if the response is structured.
    pub fn structured_value(&self) -> Option<&serde_json::Value> {
        self.content.parts.iter().find_map(|part| match part {
            ContentPart::Json { value } => Some(value),
            _ => None,
        })
    }
}

/// A model provider. Object-safe; stored as `Arc<dyn Provider>`.
pub trait Provider: Send + Sync + 'static {
    /// The provider alias this implementation serves (e.g. `"local"`).
    fn id(&self) -> &str;

    /// What this provider supports. Defaults to [`ProviderCapabilities::all`]:
    /// a provider is assumed capable unless it declares otherwise, so a custom
    /// `Provider` that only implements `generate` (+ optionally a native
    /// `generate_stream`) keeps working — streaming falls back to one-shot when
    /// the method isn't overridden, and tool/structured requests are attempted
    /// rather than rejected. Providers that genuinely lack a capability override
    /// this to opt out (e.g. `MockProvider` reports `json_schema: false`).
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::all()
    }

    /// Generate a response for `request`, authenticating with `cx.credential`
    /// when present (else the provider's built-in credential).
    fn generate<'a>(
        &'a self,
        request: GenerateRequest,
        cx: &'a ProviderContext,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>>;

    /// Stream a generation as incremental [`StreamChunk`]s.
    ///
    /// The default is a one-shot fallback over [`Provider::generate`], so a
    /// non-streaming provider still satisfies the streaming contract (the whole
    /// result arrives as a single text chunk). Providers that stream natively
    /// override this.
    fn generate_stream<'a>(
        &'a self,
        request: GenerateRequest,
        cx: &'a ProviderContext,
    ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
        default_stream(self.generate(request, cx))
    }
}

/// One-shot stream fallback the runtime uses for a provider that reports no
/// native streaming (`capabilities().streaming == false`): run `generate` and
/// adapt its result to the streaming contract. This is the capability-gated
/// degrade path — distinct from the [`Provider::generate_stream`] trait default,
/// because here the runtime decides based on the declared capability rather than
/// on whether the method was overridden.
pub(crate) fn fallback_stream<'a>(
    provider: &'a dyn Provider,
    request: GenerateRequest,
    cx: &'a ProviderContext,
) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
    default_stream(provider.generate(request, cx))
}

/// Turn a one-shot generation future into a [`StreamChunk`] stream.
fn default_stream<'a>(
    future: BoxFuture<'a, AgenkitResult<GenerateResponse>>,
) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
    Box::pin(futures::stream::once(future).flat_map(|result| {
        let chunks = match result {
            Ok(response) => response_to_chunks(response),
            Err(error) => vec![Err(error)],
        };
        futures::stream::iter(chunks)
    }))
}

/// Decompose a finished response into the chunks a streaming consumer expects.
pub(crate) fn response_to_chunks(response: GenerateResponse) -> Vec<AgenkitResult<StreamChunk>> {
    let mut chunks = Vec::new();
    // Reasoning blocks precede the answer, so emit them first (server-side only).
    for part in &response.content.parts {
        if let Some((text, signature)) = part.as_thinking() {
            chunks.push(Ok(StreamChunk::Thinking {
                text: text.to_string(),
                signature: signature.map(str::to_string),
            }));
        }
    }
    let text = response.display_text();
    if !text.is_empty() {
        chunks.push(Ok(StreamChunk::Text(text)));
    }
    for call in response.tool_calls {
        chunks.push(Ok(StreamChunk::ToolCall(call)));
    }
    if let Some(usage) = response.usage {
        chunks.push(Ok(StreamChunk::Usage(usage)));
    }
    chunks
}

/// A registry of providers keyed by alias.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider under its [`Provider::id`].
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.id().to_string(), provider);
    }

    /// Resolve the provider for a model alias (`"provider/model"`). Falls back
    /// to the single registered provider when the alias is not namespaced.
    pub fn resolve(&self, model: &ModelRef) -> AgenkitResult<Arc<dyn Provider>> {
        if let Some(provider_alias) = model.provider() {
            return self.providers.get(provider_alias).cloned().ok_or_else(|| {
                AgenkitError::config(format!(
                    "no provider registered for alias `{provider_alias}`"
                ))
            });
        }
        if self.providers.len() == 1 {
            return Ok(self.providers.values().next().cloned().expect("len == 1"));
        }
        Err(AgenkitError::config(format!(
            "model alias `{}` has no provider namespace and there is not exactly one provider",
            model.as_str()
        )))
    }

    /// Whether any providers are registered.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

/// A deterministic in-memory provider for tests and examples (§D13 "local
/// provider first"). Matching is by substring of the prompt, in rule order;
/// the first match wins, otherwise a default is returned.
#[derive(Clone, Default)]
pub struct MockProvider {
    id: String,
    rules: Vec<MockRule>,
    default_text: Option<String>,
    default_structured: Option<serde_json::Value>,
    delay_ms: u64,
    usage_override: Option<Usage>,
    /// Reasoning text prepended to every response (for W4/W5 thinking tests).
    thinking: Option<String>,
}

#[derive(Clone)]
struct MockRule {
    needle: String,
    response: MockResponse,
}

#[derive(Clone)]
enum MockResponse {
    Text(String),
    Structured(serde_json::Value),
    Tool {
        tool_id: String,
        args: serde_json::Value,
    },
}

impl MockProvider {
    /// A mock provider serving the given alias.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }

    /// Return `text` when the prompt contains `needle`.
    pub fn on_prompt_text(mut self, needle: impl Into<String>, text: impl Into<String>) -> Self {
        self.rules.push(MockRule {
            needle: needle.into(),
            response: MockResponse::Text(text.into()),
        });
        self
    }

    /// Return structured `value` when the prompt contains `needle`.
    pub fn on_prompt_structured(
        mut self,
        needle: impl Into<String>,
        value: serde_json::Value,
    ) -> Self {
        self.rules.push(MockRule {
            needle: needle.into(),
            response: MockResponse::Structured(value),
        });
        self
    }

    /// Request a tool call when the prompt contains `needle`.
    pub fn on_prompt_tool(
        mut self,
        needle: impl Into<String>,
        tool_id: impl Into<String>,
        args: serde_json::Value,
    ) -> Self {
        self.rules.push(MockRule {
            needle: needle.into(),
            response: MockResponse::Tool {
                tool_id: tool_id.into(),
                args,
            },
        });
        self
    }

    /// The default text when no rule matches.
    pub fn default_text(mut self, text: impl Into<String>) -> Self {
        self.default_text = Some(text.into());
        self
    }

    /// The default structured value when no rule matches a structured request.
    pub fn default_structured(mut self, value: serde_json::Value) -> Self {
        self.default_structured = Some(value);
        self
    }

    /// Delay each generation by `ms` (for cancellation/latency tests).
    pub fn with_delay_ms(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }

    /// Emit `text` as a reasoning ("thinking") block on every response (W4/W5):
    /// it rides the assembled content server-side and streams as a
    /// `StreamChunk::Thinking`, so the wire reasoning gate can be exercised.
    pub fn thinking(mut self, text: impl Into<String>) -> Self {
        self.thinking = Some(text.into());
        self
    }

    /// Override the usage every response reports (for cost/cache metering tests);
    /// by default usage is derived from the prompt length.
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage_override = Some(usage);
        self
    }

    fn respond(&self, request: &GenerateRequest) -> AgenkitResult<GenerateResponse> {
        let mut response = self.respond_inner(request)?;
        // Reasoning precedes the answer (server-side content + a leading
        // `StreamChunk::Thinking` when streamed).
        if let Some(text) = &self.thinking {
            response
                .content
                .parts
                .insert(0, ContentPart::thinking(text.clone(), None));
        }
        Ok(response)
    }

    fn respond_inner(&self, request: &GenerateRequest) -> AgenkitResult<GenerateResponse> {
        let prompt = request.prompt_text();
        let latest = request.last_message_text();
        let usage = self
            .usage_override
            .unwrap_or_else(|| Usage::new((prompt.len() / 4) as u64, 8));

        if let Some(rule) = self.rules.iter().find(|r| latest.contains(&r.needle)) {
            return Ok(match &rule.response {
                MockResponse::Text(text) => GenerateResponse::text(text.clone()).with_usage(usage),
                MockResponse::Structured(value) => {
                    GenerateResponse::structured(value.clone()).with_usage(usage)
                }
                MockResponse::Tool { tool_id, args } => GenerateResponse {
                    content: Content::default(),
                    tool_calls: vec![ToolCall::new(
                        format!("call-{tool_id}"),
                        tool_id.clone(),
                        args.clone(),
                    )],
                    usage: Some(usage),
                    finish_reason: FinishReason::ToolCalls,
                },
            });
        }

        if request.json_schema.is_some() {
            let value = self
                .default_structured
                .clone()
                .ok_or_else(|| AgenkitError::provider("mock: no structured response configured"))?;
            return Ok(GenerateResponse::structured(value).with_usage(usage));
        }

        let text = self
            .default_text
            .clone()
            .unwrap_or_else(|| format!("mock response for: {prompt}"));
        Ok(GenerateResponse::text(text).with_usage(usage))
    }
}

impl Provider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // The mock simulates streaming and tools, but not native JSON-schema
        // structured output (it returns canned values).
        ProviderCapabilities {
            streaming: true,
            json_schema: false,
            tools: true,
        }
    }

    fn generate<'a>(
        &'a self,
        request: GenerateRequest,
        _cx: &'a ProviderContext,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        Box::pin(async move {
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            self.respond(&request)
        })
    }

    fn generate_stream<'a>(
        &'a self,
        request: GenerateRequest,
        _cx: &'a ProviderContext,
    ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
        // Simulate incremental streaming by splitting the displayable response
        // (text, or the serialized JSON of a structured value) into word-sized
        // deltas — so partial-object streaming has prefixes to parse. Tool calls
        // and usage follow whole, so a text+tool_calls response keeps its tool
        // calls (parity with the non-streaming path / `response_to_chunks`).
        let chunks = match self.respond(&request) {
            Ok(response) => {
                let text = response.display_text();
                if text.is_empty() {
                    response_to_chunks(response)
                } else {
                    // Reasoning blocks first (server-side only), then word-sized
                    // text deltas, then tool calls + usage — parity with
                    // `response_to_chunks`.
                    let mut chunks: Vec<AgenkitResult<StreamChunk>> = Vec::new();
                    for part in &response.content.parts {
                        if let Some((thinking, signature)) = part.as_thinking() {
                            chunks.push(Ok(StreamChunk::Thinking {
                                text: thinking.to_string(),
                                signature: signature.map(str::to_string),
                            }));
                        }
                    }
                    chunks.extend(
                        text.split_inclusive(' ')
                            .map(|word| Ok(StreamChunk::Text(word.to_string()))),
                    );
                    for call in response.tool_calls {
                        chunks.push(Ok(StreamChunk::ToolCall(call)));
                    }
                    if let Some(usage) = response.usage {
                        chunks.push(Ok(StreamChunk::Usage(usage)));
                    }
                    chunks
                }
            }
            Err(error) => vec![Err(error)],
        };
        Box::pin(futures::stream::iter(chunks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_agenkit_core::Role;

    fn request(prompt: &str, structured: bool) -> GenerateRequest {
        GenerateRequest {
            model: ModelRef::new("local/default"),
            messages: vec![Message::new(Role::User, prompt)],
            json_schema: structured.then(|| serde_json::json!({"type": "object"})),
            ..GenerateRequest::default()
        }
    }

    #[tokio::test]
    async fn mock_matches_rules_then_defaults() {
        let provider = MockProvider::new("local")
            .on_prompt_text("uploads", "uploads use presigned URLs")
            .default_text("idk");
        let response = provider
            .generate(
                request("how do uploads work?", false),
                &ProviderContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(response.text_output(), "uploads use presigned URLs");

        let response = provider
            .generate(
                request("something else", false),
                &ProviderContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(response.text_output(), "idk");
    }

    #[tokio::test]
    async fn mock_returns_structured_value() {
        let provider =
            MockProvider::new("local").default_structured(serde_json::json!({"answer": "42"}));
        let response = provider
            .generate(request("q", true), &ProviderContext::default())
            .await
            .unwrap();
        assert_eq!(
            response.structured_value().unwrap(),
            &serde_json::json!({"answer": "42"})
        );
    }

    #[tokio::test]
    async fn stream_forwards_tool_calls() {
        use futures::StreamExt;
        // A tool response must reach a streaming consumer as a ToolCall chunk
        // (not be dropped on the streaming path).
        let provider = MockProvider::new("local").on_prompt_tool(
            "search",
            "search_docs",
            serde_json::json!({"q": "uploads"}),
        );
        let cx = ProviderContext::default();
        let mut stream = provider.generate_stream(request("please search docs", false), &cx);
        let mut tool_ids = Vec::new();
        while let Some(chunk) = stream.next().await {
            if let Ok(StreamChunk::ToolCall(call)) = chunk {
                tool_ids.push(call.tool_id);
            }
        }
        assert_eq!(tool_ids, vec!["search_docs".to_string()]);
    }

    #[tokio::test]
    async fn registry_resolves_by_namespace() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(MockProvider::new("local")));
        assert!(registry.resolve(&ModelRef::new("local/default")).is_ok());
        assert!(registry.resolve(&ModelRef::new("openai/gpt")).is_err());
        // Unnamespaced resolves because there is exactly one provider.
        assert!(registry.resolve(&ModelRef::new("default")).is_ok());
    }

    #[test]
    fn default_capabilities_assume_full_support() {
        // A provider that only implements `generate` is assumed capable, so the
        // capability gating doesn't silently regress it (streaming falls back to
        // one-shot; tool/structured requests are attempted, not rejected).
        struct Bare;
        impl Provider for Bare {
            fn id(&self) -> &str {
                "bare"
            }
            fn generate<'a>(
                &'a self,
                _request: GenerateRequest,
                _cx: &'a ProviderContext,
            ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
                Box::pin(async { Ok(GenerateResponse::text("ok")) })
            }
        }
        assert_eq!(Bare.capabilities(), ProviderCapabilities::all());
    }
}
