//! The `Ai` generation builder (RFC-093 Phase 2.2, §D3).
//!
//! `agenkit.ai().system(..).prompt(..).generate_text()` for text, or
//! `.schema::<T>().generate_structured()` for typed structured output. Model
//! output is untrusted: structured output is validated by deserializing into
//! `T`, and a non-JSON / shape-mismatched response surfaces as
//! [`AgenkitError::Validation`] (§D10).

use std::marker::PhantomData;
use std::sync::Arc;

use futures::StreamExt;
use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Content, ContentPart, FlowStreamEvent, Message, ModelRef, Role,
    StepId, StepKind, StepStatus, ThinkingLevel, TraceEvent, Usage, events,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use super::agenkit::AgenkitInner;
use super::provider::{FinishReason, GenerateRequest, GenerateResponse, Provider, StreamChunk};
use super::run::RunState;
use super::schema::json_schema_for;

/// A pending generation request bound to a runtime.
pub struct Ai {
    inner: Arc<AgenkitInner>,
    model: Option<ModelRef>,
    system: Option<String>,
    messages: Vec<Message>,
    max_tokens: Option<u32>,
    thinking: ThinkingLevel,
    provider_options: serde_json::Map<String, serde_json::Value>,
    tracer: Option<Arc<RunState>>,
    parent_step: Option<StepId>,
}

impl Ai {
    pub(crate) fn new(inner: Arc<AgenkitInner>) -> Self {
        Self {
            inner,
            model: None,
            system: None,
            messages: Vec::new(),
            max_tokens: None,
            thinking: ThinkingLevel::Off,
            provider_options: serde_json::Map::new(),
            tracer: None,
            parent_step: None,
        }
    }

    /// Bind this request to a run so model calls emit `ai_model_*` trace events.
    pub(crate) fn with_tracer(mut self, run: Arc<RunState>, parent_step: Option<StepId>) -> Self {
        self.tracer = Some(run);
        self.parent_step = parent_step;
        self
    }

    /// Override the model alias for this request.
    pub fn model(mut self, model: impl Into<ModelRef>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the system prompt.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Append a user prompt message.
    pub fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.messages.push(Message::new(Role::User, prompt.into()));
        self
    }

    /// Append an arbitrary message.
    pub fn message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Cap output tokens.
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Request model reasoning ("thinking") at the given level (roadmap W4).
    ///
    /// Providers map the level to their own knob — Anthropic's `budget_tokens`,
    /// OpenAI's `reasoning_effort` — and only honour it for reasoning-capable
    /// models (per the catalog); others ignore it. The default is
    /// [`ThinkingLevel::Off`]. Any reasoning the model emits rides the response's
    /// content server-side for replay/observability but is never streamed to the
    /// client (§D10).
    pub fn thinking(mut self, level: ThinkingLevel) -> Self {
        self.thinking = level;
        self
    }

    /// Attach a provider-specific request field (the "extra body" escape
    /// hatch), merged verbatim into the top level of the wire request — e.g.
    /// `.provider_option("enable_search", true)` for DashScope's built-in web
    /// search. Prefer a typed knob where one exists (`thinking`, `max_tokens`);
    /// the provider receives unknown keys as-is and rejects what it doesn't
    /// accept.
    pub fn provider_option(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.provider_options.insert(key.into(), value.into());
        self
    }

    /// Switch to typed structured output for `T`.
    ///
    /// `T` must derive `schemars::JsonSchema` so the runtime can constrain the
    /// model (strict schema where the provider supports it). The result is
    /// *type*-validated (deserialized into `T`); schema keyword constraints
    /// like `range`/`length`/`regex` are not re-checked runtime-side.
    pub fn schema<T: DeserializeOwned + JsonSchema>(self) -> AiStructured<T> {
        AiStructured {
            ai: self,
            _marker: PhantomData,
        }
    }

    /// Resolve the effective model: explicit override, else the runtime default.
    fn resolve_model(&self) -> AgenkitResult<ModelRef> {
        self.model
            .clone()
            .or_else(|| self.inner.default_model.clone())
            .ok_or_else(|| {
                AgenkitError::config("no model specified and no default model configured")
            })
    }

    fn build_request(
        &self,
        json_schema: Option<serde_json::Value>,
    ) -> AgenkitResult<GenerateRequest> {
        Ok(GenerateRequest {
            model: self.resolve_model()?,
            system: self.system.clone(),
            messages: self.messages.clone(),
            tools: Vec::new(),
            json_schema,
            max_tokens: self.max_tokens,
            thinking: self.thinking,
            provider_options: self.provider_options.clone(),
        })
    }

    /// The caller principal used for per-request credential resolution: the run's
    /// principal inside a flow, else the ambient task-local (anonymous outside a
    /// request).
    fn principal(&self) -> pocopine_auth::Principal {
        self.tracer
            .as_ref()
            .map(|run| run.principal.clone())
            .unwrap_or_else(super::agenkit::current_principal)
    }

    /// Resolve the provider credential for this request (W6). The store may
    /// return a per-principal override (BYOK); `None` lets the provider fall back
    /// to its built-in credential.
    async fn provider_context(
        &self,
        alias: &str,
    ) -> AgenkitResult<super::provider::ProviderContext> {
        let principal = self.principal();
        let credential = self.inner.credentials.resolve(alias, &principal).await?;
        Ok(super::provider::ProviderContext::for_request(credential))
    }

    async fn run(&self, json_schema: Option<serde_json::Value>) -> AgenkitResult<GenerateResponse> {
        let mut request = self.build_request(json_schema)?;
        self.inner.check_model_allowed(&request.model)?;
        // RFC-122 §4.1: an image-output model needs a wired sink before the
        // first provider call (the loop paths capture; this one-shot returns
        // the response to the caller, but the gate keeps configs honest).
        super::provider::ensure_image_output_capturable(
            &request.model,
            self.inner.artifacts.is_some(),
        )?;
        let provider = self.inner.providers.resolve(&request.model)?;
        apply_structured_fallback(&mut request, provider.as_ref());
        let request = request.normalize_media_for_wire();
        let cx = self.provider_context(provider.id()).await?;
        let model = request.model.clone();
        let model_meta = super::catalog::lookup(&model);

        let Some(run) = &self.tracer else {
            return provider
                .generate(request, &cx)
                .await
                .map_err(reclassify_overflow);
        };

        let step_id = run.next_step_id();
        let mut started = run
            .event(
                events::AI_MODEL_REQUEST,
                StepKind::Generation,
                StepStatus::Started,
            )
            .with_step(step_id.clone())
            .with_model(model.clone());
        if let Some(parent) = &self.parent_step {
            started = started.with_parent(parent.clone());
        }
        if let Some(meta) = model_meta {
            started = annotate_headroom(started, meta, &request.messages, request.max_tokens);
        }
        run.emit(started);

        let result = provider
            .generate(request, &cx)
            .await
            .map_err(reclassify_overflow);
        match &result {
            Ok(response) => {
                let mut completed = run
                    .event(
                        events::AI_MODEL_RESPONSE,
                        StepKind::Generation,
                        StepStatus::Completed,
                    )
                    .with_step(step_id)
                    .with_model(model);
                if let Some(usage) = response.usage {
                    completed = completed.with_usage(usage);
                    if let Some(meta) = model_meta {
                        completed = completed.with_cost(meta.estimate_cost(&usage));
                    }
                }
                run.emit(completed);
            }
            Err(error) => run.emit(
                run.event(
                    events::AI_MODEL_FAILED,
                    StepKind::Generation,
                    StepStatus::Failed,
                )
                .with_step(step_id)
                .with_model(model)
                .with_error(error.clone()),
            ),
        }
        result
    }

    /// Generate plain text.
    pub async fn generate_text(self) -> AgenkitResult<String> {
        Ok(self.run(None).await?.text_output())
    }

    /// Generate text plus its token usage.
    pub async fn generate_text_with_usage(self) -> AgenkitResult<(String, Option<Usage>)> {
        let response = self.run(None).await?;
        Ok((response.text_output(), response.usage))
    }

    /// Generate text, streaming each fragment to the flow's client stream as a
    /// user-visible `OutputDelta` while accumulating the full text.
    ///
    /// Uses the provider's native streaming when available; otherwise the
    /// runtime's one-shot fallback delivers the result as a single delta. The
    /// returned string is always the complete output.
    pub async fn stream_text(self) -> AgenkitResult<String> {
        Ok(self.run_streamed(None, false).await?.display_text())
    }

    /// Run a streamed generation: emit `ai_model_*` traces, consume the
    /// provider stream, forward output to the run's client stream (text deltas,
    /// or partial structured objects when `structured`), and assemble the final
    /// response.
    async fn run_streamed(
        &self,
        json_schema: Option<serde_json::Value>,
        structured: bool,
    ) -> AgenkitResult<GenerateResponse> {
        let mut request = self.build_request(json_schema)?;
        self.inner.check_model_allowed(&request.model)?;
        let provider = self.inner.providers.resolve(&request.model)?;
        apply_structured_fallback(&mut request, provider.as_ref());
        let cx = self.provider_context(provider.id()).await?;
        let model = request.model.clone();
        let model_meta = super::catalog::lookup(&model);

        let model_step = self.tracer.as_ref().map(|run| {
            let step_id = run.next_step_id();
            let mut started = run
                .event(
                    events::AI_MODEL_REQUEST,
                    StepKind::Generation,
                    StepStatus::Started,
                )
                .with_step(step_id.clone())
                .with_model(model.clone())
                .with_field("streaming", true);
            if let Some(parent) = &self.parent_step {
                started = started.with_parent(parent.clone());
            }
            if let Some(meta) = model_meta {
                started = annotate_headroom(started, meta, &request.messages, request.max_tokens);
            }
            run.emit(started);
            step_id
        });

        // Honor the provider's declared streaming capability: stream natively
        // when supported, else fall back to a one-shot generation adapted to the
        // streaming contract (§D13).
        let mut stream = if provider.capabilities().streaming {
            provider.generate_stream(request, &cx)
        } else {
            super::provider::fallback_stream(provider.as_ref(), request, &cx)
        };
        let mut text = String::new();
        let mut thinking_parts: Vec<ContentPart> = Vec::new();
        let mut media_parts: Vec<ContentPart> = Vec::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;
        let mut failure = None;
        let mut last_partial: Option<serde_json::Value> = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
                // Reasoning rides the assembled response server-side (replay +
                // observability). The client event carries the text too, but the
                // wire boundary (`redact_reasoning`) strips it unless the flow
                // opted in — so the redacted default is a count-only signal, and
                // an opted-in flow gets the full reasoning text (§D10).
                Ok(StreamChunk::Thinking { text, signature }) => {
                    if let Some(run) = &self.tracer {
                        run.stream(FlowStreamEvent::ThinkingDelta {
                            chars: text.chars().count() as u32,
                            text: Some(text.clone()),
                        });
                    }
                    thinking_parts.push(ContentPart::thinking(text, signature));
                }
                Ok(StreamChunk::Text(delta)) => {
                    text.push_str(&delta);
                    if let Some(run) = &self.tracer {
                        run.mark_output_streamed();
                        if structured {
                            // Re-parse only when the delta carries a structural
                            // or string-boundary char — i.e. when a field can
                            // start, advance, or complete. Re-parsing the whole
                            // buffer on every token would be O(n^2) over a large
                            // structured response; pure mid-value token growth is
                            // folded into the next structural delta.
                            let advances = delta.bytes().any(|b| {
                                matches!(b, b'{' | b'}' | b'[' | b']' | b':' | b',' | b'"')
                            });
                            if advances {
                                // Emit a Genkit-style partial object whenever the
                                // best-effort parse of the JSON-so-far advances.
                                if let Some(partial) =
                                    super::partial_json::parse_partial_json(&text)
                                    && last_partial.as_ref() != Some(&partial)
                                {
                                    run.stream(FlowStreamEvent::ObjectDelta {
                                        partial: partial.clone(),
                                    });
                                    last_partial = Some(partial);
                                }
                            }
                        } else {
                            run.stream(FlowStreamEvent::OutputDelta {
                                text: delta.clone(),
                            });
                        }
                    }
                }
                Ok(StreamChunk::ToolCall(call)) => tool_calls.push(call),
                // Media rides the assembled response to the flow author
                // verbatim (RFC-122: never silently dropped). The flow wire
                // has no artifact events — capture is the loop paths' job.
                Ok(StreamChunk::Media(media)) => {
                    media_parts.push(ContentPart::Media(media));
                }
                Ok(StreamChunk::Usage(reported)) => {
                    usage = Some(reported);
                    if let Some(run) = &self.tracer {
                        run.stream(FlowStreamEvent::UsageUpdate {
                            input_tokens: reported.input_tokens,
                            output_tokens: reported.output_tokens,
                            cache_read_tokens: reported.cache_read_tokens,
                            cache_creation_tokens: reported.cache_creation_tokens,
                        });
                    }
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        drop(stream);

        let result = match failure {
            Some(error) => Err(reclassify_overflow(error)),
            None => {
                let finish_reason = if tool_calls.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolCalls
                };
                // Reasoning parts (if any) precede the answer text, media
                // follows it — they ride the content server-side for replay;
                // `as_text()` skips them so the user-visible output stays
                // text-only.
                let content = if thinking_parts.is_empty() && media_parts.is_empty() {
                    Content::text(text)
                } else {
                    let mut parts = thinking_parts;
                    parts.push(ContentPart::text(text));
                    parts.extend(media_parts);
                    Content::from_parts(parts)
                };
                Ok(GenerateResponse {
                    content,
                    tool_calls,
                    usage,
                    finish_reason,
                })
            }
        };

        if let (Some(run), Some(step_id)) = (&self.tracer, model_step) {
            match &result {
                Ok(response) => {
                    let mut completed = run
                        .event(
                            events::AI_MODEL_RESPONSE,
                            StepKind::Generation,
                            StepStatus::Completed,
                        )
                        .with_step(step_id)
                        .with_model(model);
                    if let Some(usage) = response.usage {
                        completed = completed.with_usage(usage);
                        if let Some(meta) = model_meta {
                            completed = completed.with_cost(meta.estimate_cost(&usage));
                        }
                    }
                    run.emit(completed);
                }
                Err(error) => run.emit(
                    run.event(
                        events::AI_MODEL_FAILED,
                        StepKind::Generation,
                        StepStatus::Failed,
                    )
                    .with_step(step_id)
                    .with_model(model)
                    .with_error(error.clone()),
                ),
            }
        }
        result
    }
}

/// Capability-gated structured-output degrade path (§D13): when the resolved
/// provider reports no native schema-constrained output
/// (`capabilities().json_schema == false`), append an explicit instruction so
/// the model still returns a JSON object conforming to the requested schema.
/// The schema stays on the request (a provider that *does* support it natively
/// — `json_schema == true` — gets no instruction and uses it directly). No-op
/// for non-structured requests.
fn apply_structured_fallback(request: &mut GenerateRequest, provider: &dyn Provider) {
    // Check the capability before touching the schema: a provider with native
    // schema support (the common case) returns here with no clone.
    if provider.capabilities().json_schema {
        return;
    }
    let Some(schema) = request.json_schema.clone() else {
        return;
    };
    let note = format!(
        "Respond with a single JSON object that conforms to this JSON Schema, and nothing else:\n{schema}"
    );
    request.system = Some(match request.system.take() {
        Some(existing) => format!("{existing}\n\n{note}"),
        None => note,
    });
}

/// Reclassify a provider error as [`AgenkitError::ContextOverflow`] when its
/// message matches a known "input too long" shape (W3), so callers and W7
/// compaction can branch on the kind instead of an opaque provider error.
pub(crate) fn reclassify_overflow(err: AgenkitError) -> AgenkitError {
    if let AgenkitError::Provider { message } = &err
        && super::overflow::is_context_overflow(message)
    {
        return AgenkitError::context_overflow(message.clone());
    }
    err
}

/// Annotate a model-request trace event with a proactive context-overflow
/// prediction (counts only — redaction-safe) when the estimated input plus the
/// reserved output budget would exceed the model's window. Advisory: it never
/// errors or truncates; W7 owns the compaction decision.
fn annotate_headroom(
    event: TraceEvent,
    model: &super::catalog::Model,
    messages: &[Message],
    max_tokens: Option<u32>,
) -> TraceEvent {
    let max_output = max_tokens.unwrap_or(model.max_output);
    let headroom = super::overflow::context_headroom(model, messages, max_output);
    if headroom.over {
        event
            .with_field("context_overflow_predicted", true)
            .with_field("estimated_input_tokens", headroom.estimated_input_tokens)
            .with_field("context_window", headroom.context_window)
    } else {
        event
    }
}

/// An [`Ai`] request bound to a structured output type `T`.
pub struct AiStructured<T> {
    ai: Ai,
    _marker: PhantomData<T>,
}

impl<T: DeserializeOwned + JsonSchema> AiStructured<T> {
    /// Override the model alias.
    pub fn model(mut self, model: impl Into<ModelRef>) -> Self {
        self.ai = self.ai.model(model);
        self
    }

    /// Set the system prompt.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.ai = self.ai.system(system);
        self
    }

    /// Append a user prompt message.
    pub fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.ai = self.ai.prompt(prompt);
        self
    }

    /// Request model reasoning ("thinking") at the given level (roadmap W4).
    pub fn thinking(mut self, level: ThinkingLevel) -> Self {
        self.ai = self.ai.thinking(level);
        self
    }

    /// Attach a provider-specific request field (see [`Ai::provider_option`]).
    pub fn provider_option(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.ai = self.ai.provider_option(key, value);
        self
    }

    /// Generate and validate typed structured output.
    pub async fn generate_structured(self) -> AgenkitResult<T> {
        // The real JSON Schema for `T` — providers that support it constrain
        // generation; others fall back to JSON mode + a prompt instruction.
        let schema = json_schema_for::<T>();
        let response = self.ai.run(Some(schema)).await?;

        let value = response
            .structured_value()
            .cloned()
            .or_else(|| serde_json::from_str(&response.text_output()).ok())
            .ok_or_else(|| {
                AgenkitError::validation("model did not return JSON for a structured request")
            })?;

        serde_json::from_value(value).map_err(|err| {
            AgenkitError::validation(format!("structured output did not match schema: {err}"))
        })
    }

    /// Generate structured output while streaming Genkit-style partial objects
    /// to the flow's client stream (`ObjectDelta`), then validate and return the
    /// complete `T`.
    ///
    /// Each `ObjectDelta` is a best-effort parse of the JSON generated so far,
    /// so a client can render a progressively-completing object. Uses native
    /// streaming when the provider supports it; otherwise the result arrives as
    /// a single partial.
    pub async fn stream_structured(self) -> AgenkitResult<T> {
        let schema = json_schema_for::<T>();
        let response = self.ai.run_streamed(Some(schema), true).await?;

        let value = response
            .structured_value()
            .cloned()
            .or_else(|| serde_json::from_str(&response.display_text()).ok())
            .ok_or_else(|| {
                AgenkitError::validation("model did not return JSON for a structured request")
            })?;

        serde_json::from_value(value).map_err(|err| {
            AgenkitError::validation(format!("structured output did not match schema: {err}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::agenkit::Agenkit;
    use super::super::provider::{
        BoxFuture, BoxStream, GenerateRequest, GenerateResponse, MockProvider, Provider,
        ProviderCapabilities, ProviderContext, StreamChunk,
    };
    use pocopine_agenkit_core::{AgenkitResult, ModelRef};
    use serde::Deserialize;

    fn runtime(provider: MockProvider) -> Agenkit {
        Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default"))
            .build()
            .unwrap()
    }

    /// A provider with configurable capabilities that records the `system` of
    /// the request it receives, so tests can assert how the runtime degrades.
    #[derive(Clone)]
    struct ProbeProvider {
        caps: ProviderCapabilities,
        last_system: Arc<Mutex<Option<String>>>,
        panic_on_stream: bool,
    }

    impl ProbeProvider {
        fn new(caps: ProviderCapabilities) -> Self {
            Self {
                caps,
                last_system: Arc::new(Mutex::new(None)),
                panic_on_stream: false,
            }
        }
    }

    impl Provider for ProbeProvider {
        fn id(&self) -> &str {
            "probe"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            self.caps
        }

        fn generate<'a>(
            &'a self,
            request: GenerateRequest,
            _cx: &'a ProviderContext,
        ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
            *self.last_system.lock().unwrap() = request.system.clone();
            let structured = request.json_schema.is_some();
            Box::pin(async move {
                Ok(if structured {
                    GenerateResponse::structured(serde_json::json!({"title": "X", "words": 1}))
                } else {
                    GenerateResponse::text("plain")
                })
            })
        }

        fn generate_stream<'a>(
            &'a self,
            request: GenerateRequest,
            _cx: &'a ProviderContext,
        ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
            assert!(
                !self.panic_on_stream,
                "runtime must not call generate_stream when streaming is unsupported"
            );
            super::super::provider::fallback_stream(self, request, _cx)
        }
    }

    fn probe_runtime(provider: ProbeProvider) -> Agenkit {
        Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("probe/default"))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn generate_text_uses_default_model() {
        let agenkit = runtime(
            MockProvider::new("local").on_prompt_text("uploads", "uploads use presigned URLs"),
        );
        let answer = agenkit
            .ai()
            .prompt("how do uploads work?")
            .generate_text()
            .await
            .unwrap();
        assert_eq!(answer, "uploads use presigned URLs");
    }

    #[derive(Debug, Deserialize, PartialEq, schemars::JsonSchema)]
    struct Summary {
        title: String,
        words: u32,
    }

    #[tokio::test]
    async fn generate_structured_validates_into_type() {
        let agenkit = runtime(
            MockProvider::new("local")
                .default_structured(serde_json::json!({"title": "Uploads", "words": 12})),
        );
        let summary: Summary = agenkit
            .ai()
            .prompt("summarize the uploads doc")
            .schema::<Summary>()
            .generate_structured()
            .await
            .unwrap();
        assert_eq!(
            summary,
            Summary {
                title: "Uploads".to_string(),
                words: 12
            }
        );
    }

    #[tokio::test]
    async fn structured_shape_mismatch_is_validation_error() {
        let agenkit =
            runtime(MockProvider::new("local").default_structured(serde_json::json!({"title": 7})));
        let result = agenkit
            .ai()
            .prompt("x")
            .schema::<Summary>()
            .generate_structured()
            .await;
        assert_eq!(result.unwrap_err().kind(), "validation");
    }

    #[tokio::test]
    async fn missing_model_is_config_error() {
        let agenkit = Agenkit::builder()
            .provider(MockProvider::new("local"))
            .build()
            .unwrap();
        let result = agenkit.ai().prompt("x").generate_text().await;
        assert_eq!(result.unwrap_err().kind(), "config");
    }

    #[tokio::test]
    async fn no_native_json_schema_injects_a_prompt_instruction() {
        // A provider reporting `json_schema: false` gets an explicit instruction
        // to emit conforming JSON (the capability-gated degrade path).
        let provider = ProbeProvider::new(ProviderCapabilities {
            streaming: true,
            json_schema: false,
            tools: true,
        });
        let seen = provider.last_system.clone();
        let agenkit = probe_runtime(provider);
        let _: Summary = agenkit
            .ai()
            .prompt("summarize")
            .schema::<Summary>()
            .generate_structured()
            .await
            .unwrap();
        let system = seen.lock().unwrap().clone().unwrap_or_default();
        assert!(
            system.contains("JSON Schema"),
            "expected an injected schema instruction, got: {system:?}"
        );
    }

    #[tokio::test]
    async fn native_json_schema_provider_gets_no_injection() {
        // A provider reporting `json_schema: true` constrains generation itself,
        // so the runtime must not inject a fallback instruction.
        let provider = ProbeProvider::new(ProviderCapabilities::all());
        let seen = provider.last_system.clone();
        let agenkit = probe_runtime(provider);
        let _: Summary = agenkit
            .ai()
            .system("be terse")
            .prompt("summarize")
            .schema::<Summary>()
            .generate_structured()
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().clone(), Some("be terse".to_string()));
    }

    #[tokio::test]
    async fn no_native_streaming_uses_one_shot_fallback() {
        // A provider reporting `streaming: false` must be driven via the one-shot
        // fallback — the runtime must not call its native `generate_stream`.
        let mut provider = ProbeProvider::new(ProviderCapabilities {
            streaming: false,
            json_schema: true,
            tools: true,
        });
        provider.panic_on_stream = true; // fails the test if generate_stream is hit
        let agenkit = probe_runtime(provider);
        let text = agenkit.ai().prompt("hi").stream_text().await.unwrap();
        assert_eq!(text, "plain");
    }

    #[test]
    fn structured_output_derives_a_real_schema() {
        // The runtime sends a real JSON Schema (not a marker), so a capable
        // provider can constrain generation.
        let schema = super::json_schema_for::<Summary>();
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert!(schema["properties"].get("words").is_some());
    }

    /// A provider that always rejects with a context-overflow-shaped message.
    struct OverflowProvider;

    impl Provider for OverflowProvider {
        fn id(&self) -> &str {
            "overflow"
        }

        fn generate<'a>(
            &'a self,
            _request: GenerateRequest,
            _cx: &'a ProviderContext,
        ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
            Box::pin(async {
                Err(pocopine_agenkit_core::AgenkitError::provider(
                    "This model's maximum context length is 8192 tokens, however you requested 9000",
                ))
            })
        }

        fn generate_stream<'a>(
            &'a self,
            request: GenerateRequest,
            _cx: &'a ProviderContext,
        ) -> BoxStream<'a, AgenkitResult<StreamChunk>> {
            super::super::provider::fallback_stream(self, request, _cx)
        }
    }

    #[tokio::test]
    async fn provider_overflow_is_reclassified_unary_and_streamed() {
        let agenkit = Agenkit::builder()
            .provider(OverflowProvider)
            .default_model(ModelRef::new("overflow/x"))
            .build()
            .unwrap();
        let unary = agenkit.ai().prompt("x").generate_text().await.unwrap_err();
        assert_eq!(unary.kind(), "context_overflow");
        let streamed = agenkit.ai().prompt("x").stream_text().await.unwrap_err();
        assert_eq!(streamed.kind(), "context_overflow");
    }
}
