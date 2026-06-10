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
    AgenkitError, AgenkitResult, Content, FlowStreamEvent, Message, ModelRef, Role, StepId,
    StepKind, StepStatus, Usage, events,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use super::agenkit::AgenkitInner;
use super::provider::{FinishReason, GenerateRequest, GenerateResponse, StreamChunk};
use super::run::RunState;
use super::schema::json_schema_for;

/// A pending generation request bound to a runtime.
pub struct Ai {
    inner: Arc<AgenkitInner>,
    model: Option<ModelRef>,
    system: Option<String>,
    messages: Vec<Message>,
    max_tokens: Option<u32>,
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
        })
    }

    async fn run(&self, json_schema: Option<serde_json::Value>) -> AgenkitResult<GenerateResponse> {
        let request = self.build_request(json_schema)?;
        self.inner.check_model_allowed(&request.model)?;
        let provider = self.inner.providers.resolve(&request.model)?;
        let model = request.model.clone();

        let Some(run) = &self.tracer else {
            return provider.generate(request).await;
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
        run.emit(started);

        let result = provider.generate(request).await;
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
        let request = self.build_request(json_schema)?;
        self.inner.check_model_allowed(&request.model)?;
        let provider = self.inner.providers.resolve(&request.model)?;
        let model = request.model.clone();

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
            run.emit(started);
            step_id
        });

        let mut stream = provider.generate_stream(request);
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;
        let mut failure = None;
        let mut last_partial: Option<serde_json::Value> = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
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
                Ok(StreamChunk::Usage(reported)) => usage = Some(reported),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        drop(stream);

        let result = match failure {
            Some(error) => Err(error),
            None => {
                let finish_reason = if tool_calls.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolCalls
                };
                Ok(GenerateResponse {
                    content: Content::text(text),
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
    use super::super::agenkit::Agenkit;
    use super::super::provider::MockProvider;
    use pocopine_agenkit_core::ModelRef;
    use serde::Deserialize;

    fn runtime(provider: MockProvider) -> Agenkit {
        Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default"))
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

    #[test]
    fn structured_output_derives_a_real_schema() {
        // The runtime sends a real JSON Schema (not a marker), so a capable
        // provider can constrain generation.
        let schema = super::json_schema_for::<Summary>();
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert!(schema["properties"].get("words").is_some());
    }
}
