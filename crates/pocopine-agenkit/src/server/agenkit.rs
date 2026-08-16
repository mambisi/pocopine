//! The `Agenkit` facade and its builder (RFC-093 Phase 2.2, §D3).
//!
//! `Agenkit` is the one canonical entry point. Apps configure it once and then
//! call [`Agenkit::ai`] (and, in later checkpoints, registered flows/agents)
//! from server functions, jobs, and evals.

use std::any::Any;
use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, FlowStreamEvent, ModelRef, RunId, StepKind, StepStatus, TraceId,
    events,
};
use pocopine_auth::Principal;
use pocopine_core::{ServerError, StreamServerResult};
use serde::{Serialize, de::DeserializeOwned};
use std::future::Future;
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::wrappers::UnboundedReceiverStream;

tokio::task_local! {
    /// The caller principal in scope for the current request (set by the
    /// in-facade adapter, §D15 DC-5). `run_flow` picks this up automatically.
    pub(crate) static CURRENT_PRINCIPAL: Principal;
}

/// Run `future` with `principal` in scope so any `run_flow` call within it
/// executes under that identity. This is the seam an axum
/// middleware uses after reading `Principal` from request extensions (§D15 DC-5).
pub async fn with_principal<F: Future>(principal: Principal, future: F) -> F::Output {
    CURRENT_PRINCIPAL.scope(principal, future).await
}

pub(crate) fn current_principal() -> Principal {
    CURRENT_PRINCIPAL
        .try_with(Clone::clone)
        .unwrap_or_else(|_| Principal::anonymous())
}

use super::context::{AiContext, AppState};
use super::embed::{AiEmbedder, EmbedderRegistry};
use super::flow::{AiFlowContext, FlowDef, FlowHandler, FlowRegistry};
use super::generate::Ai;
use super::provider::{Provider, ProviderRegistry};
use super::retrieval::{AiRetriever, RetrieverRegistry};
use super::run::RunState;
use super::thread::{AgentThreadStore, SessionThreadStore};
use super::tool::{AiTool, DynTool, ToolRegistry};

/// Shared, immutable runtime state behind an [`Agenkit`] handle.
pub(crate) struct AgenkitInner {
    pub(crate) providers: ProviderRegistry,
    /// Resolves the provider credential per request (W6), possibly per principal
    /// (BYOK). Defaults to [`EnvCredentials`](super::credentials::EnvCredentials).
    pub(crate) credentials: Arc<dyn super::credentials::ProviderCredentials>,
    pub(crate) default_model: Option<ModelRef>,
    pub(crate) tools: ToolRegistry,
    pub(crate) retrievers: RetrieverRegistry,
    pub(crate) embedders: EmbedderRegistry,
    pub(crate) flows: FlowRegistry,
    pub(crate) state: Arc<AppState>,
    pub(crate) thread_store: Arc<dyn AgentThreadStore>,
    /// The implementor-owned artifact capture wiring (RFC-122), when the host
    /// configured a sink. Absent ⇒ `ctx.artifacts()` errors loudly.
    pub(crate) artifacts: Option<super::artifact::ArtifactRuntime>,
    /// When set, only these model aliases may be resolved (§D10). Enforced
    /// before any provider call.
    pub(crate) model_allowlist: Option<HashSet<String>>,
    pub(crate) run_seq: AtomicU64,
}

impl AgenkitInner {
    /// Reject a model alias that is not allowlisted, before any provider call.
    pub(crate) fn check_model_allowed(&self, model: &ModelRef) -> AgenkitResult<()> {
        if let Some(allow) = &self.model_allowlist
            && !allow.contains(model.as_str())
        {
            return Err(AgenkitError::config(format!(
                "model alias `{}` is not allowlisted",
                model.as_str()
            )));
        }
        Ok(())
    }
}

/// The unified Agenkit runtime handle. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct Agenkit {
    pub(crate) inner: Arc<AgenkitInner>,
}

impl Agenkit {
    /// Start configuring a runtime.
    pub fn builder() -> AgenkitBuilder {
        AgenkitBuilder::default()
    }

    /// Begin a generation request.
    pub fn ai(&self) -> Ai {
        Ai::new(self.inner.clone())
    }

    /// The configured default model alias, if any.
    pub fn default_model(&self) -> Option<&ModelRef> {
        self.inner.default_model.as_ref()
    }

    /// The tool registry.
    pub fn tools(&self) -> &ToolRegistry {
        &self.inner.tools
    }

    /// The retriever registry.
    pub fn retrievers(&self) -> &RetrieverRegistry {
        &self.inner.retrievers
    }

    /// The embedder registry.
    pub fn embedders(&self) -> &EmbedderRegistry {
        &self.inner.embedders
    }

    /// The configured agent-thread store — e.g. to list a principal's threads
    /// for a sidebar (`agenkit.thread_store().list(owner)`).
    pub fn thread_store(&self) -> &Arc<dyn AgentThreadStore> {
        &self.inner.thread_store
    }

    /// A fresh execution context over the runtime's app state.
    pub fn context(&self) -> AiContext {
        AiContext::new(self.inner.state.clone())
    }

    /// Whether a flow id is registered.
    pub fn has_flow(&self, id: &str) -> bool {
        self.inner.flows.contains(id)
    }

    async fn run_flow_inner(
        &self,
        id: &str,
        input: serde_json::Value,
        principal: Option<Principal>,
        sink: Option<UnboundedSender<FlowStreamEvent>>,
    ) -> AgenkitResult<serde_json::Value> {
        let handler: Arc<dyn FlowHandler> = self
            .inner
            .flows
            .get(id)
            .ok_or_else(|| AgenkitError::not_found(format!("flow `{id}` is not registered")))?;

        // Identity: an explicit principal wins; otherwise pick up the
        // task-local set by the adapter; otherwise anonymous (§D15 DC-5).
        let principal = principal.unwrap_or_else(current_principal);
        let seq = self.inner.run_seq.fetch_add(1, Ordering::Relaxed);
        let run_id = RunId::new(format!("run-{seq}"));
        let trace_id = TraceId::new(format!("trace-{seq}"));
        let run = RunState::new(
            self.inner.clone(),
            run_id.clone(),
            trace_id.clone(),
            principal,
            sink,
        );
        let descriptor = handler.descriptor();
        let ctx = AiFlowContext::new(run.clone(), id.to_string(), descriptor.manifest.clone());

        run.emit(
            run.event(
                events::AI_FLOW_STARTED,
                StepKind::Custom,
                StepStatus::Started,
            )
            .with_field("flow_id", id),
        );
        run.stream(FlowStreamEvent::FlowStarted {
            run_id: run_id.as_str().to_string(),
            trace_id: trace_id.as_str().to_string(),
        });

        let result = handler.run_json(input, ctx).await;
        match &result {
            Ok(value) => {
                // Surface the final typed result as user-visible output BEFORE
                // the lifecycle `completed`, so clients keyed on `FlowCompleted`
                // as end-of-stream still receive the result (no-op when not
                // streaming, since the sink is absent). Skip it when the flow
                // already streamed its output via `stream_text`, to avoid
                // duplicating the result.
                if !run.output_was_streamed() {
                    run.stream(FlowStreamEvent::OutputDelta {
                        text: value.to_string(),
                    });
                }
                run.stream(FlowStreamEvent::OutputCompleted);
                run.emit(
                    run.event(
                        events::AI_FLOW_COMPLETED,
                        StepKind::Custom,
                        StepStatus::Completed,
                    )
                    .with_field("flow_id", id),
                );
                run.stream(FlowStreamEvent::FlowCompleted {
                    run_id: run_id.as_str().to_string(),
                });
            }
            Err(error) => {
                run.emit(
                    run.event(events::AI_FLOW_FAILED, StepKind::Custom, StepStatus::Failed)
                        .with_field("flow_id", id)
                        .with_error(error.clone()),
                );
                run.stream(FlowStreamEvent::FlowFailed {
                    run_id: run_id.as_str().to_string(),
                    error_kind: error.kind().to_string(),
                    trace_id: trace_id.as_str().to_string(),
                });
            }
        }
        result
    }

    /// Begin a flow invocation. Two call styles, one method:
    ///
    /// - **Typed** (preferred) — pass the `#[ai_flow]`-generated marker:
    ///   `agenkit.flow(Summarize).input(x).run().await`. The id can't be
    ///   mistyped, `.input(..)` is checked against the flow's `Input`, and
    ///   `.run()` infers its `Output` (no turbofish).
    /// - **Dynamic** — pass the id string when the flow isn't statically known
    ///   (a dev runner, a telemetry console):
    ///   `agenkit.flow("summarize").input(value).run::<O>().await`.
    ///
    /// `.input(..)` is optional (defaults to `null`, which deserializes to a
    /// `()`-input flow); `.principal(..)` is optional (defaults to the ambient
    /// principal scoped by [`crate::server::PrincipalLayer`]); the terminal is
    /// `.run()` (typed output) or `.stream_into(sink)` (public [`FlowStreamEvent`]s).
    pub fn flow<K: FlowKey>(&self, key: K) -> K::Call {
        key.into_call(self.clone())
    }
}

/// The key accepted by [`Agenkit::flow`]: either a `#[ai_flow]` marker (typed —
/// yields a [`TypedFlowCall`]) or an id string (dynamic — yields a [`FlowCall`]).
/// The typed impls are generated by `#[ai_flow]`; the string impls below are the
/// escape hatch. App code rarely implements this by hand.
pub trait FlowKey {
    /// The pending-call builder this key produces.
    type Call;
    /// Begin the call against `agenkit` (a cheap `Arc` clone).
    fn into_call(self, agenkit: Agenkit) -> Self::Call;
}

impl FlowKey for &str {
    type Call = FlowCall;
    fn into_call(self, agenkit: Agenkit) -> FlowCall {
        FlowCall::new(agenkit, self.to_string())
    }
}

impl FlowKey for String {
    type Call = FlowCall;
    fn into_call(self, agenkit: Agenkit) -> FlowCall {
        FlowCall::new(agenkit, self)
    }
}

/// A dynamic (id-string) pending flow invocation, from `agenkit.flow("id")`. The
/// output type is chosen at the terminal (`run::<O>()`); prefer the typed
/// [`TypedFlowCall`] (`agenkit.flow(Marker)`) when the flow is statically known.
pub struct FlowCall {
    agenkit: Agenkit,
    id: String,
    /// Serialized eagerly in [`FlowCall::input`]; a serialize failure is stashed
    /// and surfaced at the terminal.
    input: AgenkitResult<serde_json::Value>,
    principal: Option<Principal>,
    request_reasoning: bool,
}

impl FlowCall {
    fn new(agenkit: Agenkit, id: String) -> Self {
        Self {
            agenkit,
            id,
            input: Ok(serde_json::Value::Null),
            principal: None,
            request_reasoning: false,
        }
    }

    /// Set the flow input (serialized now). Omit for a `()`-input flow.
    pub fn input(mut self, input: impl Serialize) -> Self {
        self.input = serde_json::to_value(input)
            .map_err(|err| AgenkitError::validation(format!("flow `{}` input: {err}", self.id)));
        self
    }

    /// Run explicitly under `principal`, bypassing the ambient task-local — for
    /// tests and non-request contexts.
    pub fn principal(mut self, principal: Principal) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Request the model's reasoning ("thinking") text on the [`stream`] wire.
    /// This is the **caller** half of the reasoning gate: text crosses only when
    /// this is `true` **and** the flow author permitted it via
    /// [`Flow::expose_reasoning`](crate::server::Flow::expose_reasoning) (the
    /// ceiling). Off → only a redacted character count crosses (§D10). A
    /// `#[server]` fn wires this from the client's request (a query flag/header),
    /// so the client sees reasoning when it asks and the author allows.
    ///
    /// [`stream`]: FlowCall::stream
    pub fn request_reasoning(mut self, requested: bool) -> Self {
        self.request_reasoning = requested;
        self
    }

    /// Run the flow and deserialize its typed output. `O` is inferred from the
    /// binding (`Value` works for untyped output). Emits `ai_flow_started` /
    /// `ai_flow_completed` / `ai_flow_failed` under a fresh trace tree.
    pub async fn run<O: DeserializeOwned>(self) -> AgenkitResult<O> {
        let input = self.input?;
        let output = self
            .agenkit
            .run_flow_inner(&self.id, input, self.principal, None)
            .await?;
        serde_json::from_value(output)
            .map_err(|err| AgenkitError::validation(format!("flow `{}` output: {err}", self.id)))
    }

    /// Stream the flow's [`FlowStreamEvent`]s into a caller-owned `sink`,
    /// returning the final raw output (which also rides the sink as
    /// `OutputDelta`/`OutputCompleted`).
    ///
    /// **Full fidelity, server-side.** Unlike [`stream`](FlowCall::stream) (the
    /// wire boundary), this applies **no** redaction or [`StreamMode`] cap — the
    /// sink receives every event verbatim, including `ThinkingDelta` carrying the
    /// model's raw reasoning text. It is for trusted in-process consumers (a dev
    /// building/observing an agent). Do **not** forward these events to an
    /// untrusted client as-is; use [`stream`](FlowCall::stream), which gates
    /// reasoning (author ceiling × caller request) and enforces visibility (§D10).
    ///
    /// [`StreamMode`]: pocopine_agenkit_core::StreamMode
    pub async fn stream_into(
        self,
        sink: UnboundedSender<FlowStreamEvent>,
    ) -> AgenkitResult<serde_json::Value> {
        let input = self.input?;
        self.agenkit
            .run_flow_inner(&self.id, input, self.principal, Some(sink))
            .await
    }

    /// Expose this flow as a streaming `#[server]` fn (RFC-107): returns a
    /// [`StreamServerResult`] of redacted public [`FlowStreamEvent`]s. Only a
    /// `public` flow is reachable (a private one is indistinguishable from
    /// unknown); the author-declared [`StreamMode`](pocopine_agenkit_core::StreamMode)
    /// caps visibility (§D8). Reasoning text crosses only when the author allowed
    /// it (`expose_reasoning()`) **and** the caller asked via
    /// [`request_reasoning`](FlowCall::request_reasoning).
    ///
    /// ```ignore
    /// #[server(public)]
    /// pub async fn summarize_stream(input: In, want_reasoning: bool)
    ///     -> StreamServerResult<FlowStreamEvent>
    /// {
    ///     active_plugin::<Agenkit>().unwrap()
    ///         .flow("summarize").input(input)
    ///         .request_reasoning(want_reasoning) // from the client's request
    ///         .stream()
    /// }
    /// ```
    pub fn stream(self) -> StreamServerResult<FlowStreamEvent> {
        stream_flow_to_client(
            self.agenkit,
            self.id,
            self.input,
            self.principal,
            self.request_reasoning,
        )
    }
}

/// A typed pending flow invocation, from `agenkit.flow(Marker)` where `Marker`
/// is a [`FlowDef`] (generated by `#[ai_flow]`). The flow id, the input type,
/// and the output type are all compile-time — no stringly-typed call site.
pub struct TypedFlowCall<F: FlowDef> {
    agenkit: Agenkit,
    input: AgenkitResult<serde_json::Value>,
    principal: Option<Principal>,
    request_reasoning: bool,
    _marker: PhantomData<fn() -> F>,
}

impl<F: FlowDef> TypedFlowCall<F> {
    /// Construct from a runtime handle. Prefer `agenkit.flow(Marker)`; this is
    /// the seam the `#[ai_flow]`-generated [`FlowKey`] impl calls.
    #[doc(hidden)]
    pub fn from_handle(agenkit: Agenkit) -> Self {
        Self {
            agenkit,
            input: Ok(serde_json::Value::Null),
            principal: None,
            request_reasoning: false,
            _marker: PhantomData,
        }
    }

    /// Set the flow input — checked against [`FlowDef::Input`] at compile time.
    /// Omit for a `()`-input flow.
    pub fn input(mut self, input: F::Input) -> Self {
        self.input = serde_json::to_value(input)
            .map_err(|err| AgenkitError::validation(format!("flow `{}` input: {err}", F::ID)));
        self
    }

    /// Run explicitly under `principal`, bypassing the ambient task-local — for
    /// tests and non-request contexts.
    pub fn principal(mut self, principal: Principal) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Request the model's reasoning text on the [`stream`](TypedFlowCall::stream)
    /// wire — see [`FlowCall::request_reasoning`]. Honored only when the author
    /// also permitted it via
    /// [`Flow::expose_reasoning`](crate::server::Flow::expose_reasoning).
    pub fn request_reasoning(mut self, requested: bool) -> Self {
        self.request_reasoning = requested;
        self
    }

    /// Run the flow, returning its [`FlowDef::Output`] (inferred — no turbofish).
    /// Emits `ai_flow_started` / `ai_flow_completed` / `ai_flow_failed` under a
    /// fresh trace tree.
    pub async fn run(self) -> AgenkitResult<F::Output> {
        let input = self.input?;
        let output = self
            .agenkit
            .run_flow_inner(F::ID, input, self.principal, None)
            .await?;
        serde_json::from_value(output)
            .map_err(|err| AgenkitError::validation(format!("flow `{}` output: {err}", F::ID)))
    }

    /// Stream the flow's [`FlowStreamEvent`]s into a caller-owned `sink`,
    /// returning the final typed [`FlowDef::Output`]. **Full fidelity,
    /// server-side** — no redaction or [`StreamMode`](pocopine_agenkit_core::StreamMode)
    /// cap is applied (raw `ThinkingDelta` text included); for an untrusted client
    /// use [`stream`](TypedFlowCall::stream). See [`FlowCall::stream_into`].
    pub async fn stream_into(
        self,
        sink: UnboundedSender<FlowStreamEvent>,
    ) -> AgenkitResult<F::Output> {
        let input = self.input?;
        let output = self
            .agenkit
            .run_flow_inner(F::ID, input, self.principal, Some(sink))
            .await?;
        serde_json::from_value(output)
            .map_err(|err| AgenkitError::validation(format!("flow `{}` output: {err}", F::ID)))
    }

    /// Expose this flow as a streaming `#[server]` fn (RFC-107). See
    /// [`FlowCall::stream`].
    pub fn stream(self) -> StreamServerResult<FlowStreamEvent> {
        stream_flow_to_client(
            self.agenkit,
            F::ID.to_string(),
            self.input,
            self.principal,
            self.request_reasoning,
        )
    }
}

/// Shared body of `FlowCall`/`TypedFlowCall`'s `stream`: gate the flow
/// as `public`, spawn it under the caller principal feeding a channel, and
/// return a redacted [`StreamServerResult`] of public [`FlowStreamEvent`]s. The
/// redaction chokepoint ([`super::bridge::stream_filter`]) is applied to every
/// event before it reaches the SSE frame (§D8/§D10).
fn stream_flow_to_client(
    agenkit: Agenkit,
    id: String,
    input: AgenkitResult<serde_json::Value>,
    principal: Option<Principal>,
    request_reasoning: bool,
) -> StreamServerResult<FlowStreamEvent> {
    use futures::StreamExt;

    // Only public flows are reachable; a private one is indistinguishable from
    // an unknown one (§D9/§D10).
    if !agenkit.flow_is_public(&id) {
        return Err(ServerError::bad_request("unknown AI flow"));
    }
    // An input-serialization error is the outer (handshake-level) failure.
    let input = input.map_err(|e| super::bridge::to_server_error(&e))?;
    // The author-declared visibility cap; the filter is the wire chokepoint (§D8).
    let mode = agenkit.flow_stream_mode(&id);
    // Reasoning text crosses only when the author permits it (the ceiling) AND
    // the caller requested it; either off → only a redacted count crosses (§D10),
    // so a public flow's reasoning can't be extracted by a caller it didn't opt in.
    let expose_reasoning = agenkit.flow_exposes_reasoning(&id) && request_reasoning;
    // Capture the ambient principal now — the spawned task loses the task-local.
    let principal = principal.unwrap_or_else(current_principal);

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let _ = agenkit
            .run_flow_inner(&id, input, Some(principal), Some(tx))
            .await;
    });

    let stream = UnboundedReceiverStream::new(rx)
        .filter_map(move |event| async move {
            // Per-flow reasoning gate first (strip text unless exposed), then the
            // visibility-cap chokepoint — both before the event hits the wire.
            let event = super::bridge::redact_reasoning(event, expose_reasoning);
            super::bridge::stream_filter(&event, mode).then_some(Ok::<_, ServerError>(event))
        })
        .boxed();
    Ok(stream)
}

/// Builder for [`Agenkit`].
#[derive(Default)]
pub struct AgenkitBuilder {
    providers: ProviderRegistry,
    credentials: Option<Arc<dyn super::credentials::ProviderCredentials>>,
    default_model: Option<ModelRef>,
    tools: ToolRegistry,
    retrievers: RetrieverRegistry,
    embedders: EmbedderRegistry,
    flows: FlowRegistry,
    state: AppState,
    thread_store: Option<Arc<dyn AgentThreadStore>>,
    artifact_sink: Option<Arc<dyn super::artifact::ArtifactSink>>,
    artifact_append_budget: Option<usize>,
    model_allowlist: HashSet<String>,
}

impl AgenkitBuilder {
    /// Register a provider (consumed and boxed).
    pub fn provider<P: Provider>(mut self, provider: P) -> Self {
        self.providers.register(Arc::new(provider));
        self
    }

    /// Register an already-shared provider.
    pub fn provider_arc(mut self, provider: Arc<dyn Provider>) -> Self {
        self.providers.register(provider);
        self
    }

    /// Set the credential store that resolves provider credentials per request,
    /// optionally per principal (BYOK, W6). Defaults to
    /// [`EnvCredentials`](super::credentials::EnvCredentials) — `{PROVIDER}_API_KEY`
    /// from the environment.
    pub fn credentials(mut self, store: Arc<dyn super::credentials::ProviderCredentials>) -> Self {
        self.credentials = Some(store);
        self
    }

    /// Set the default model alias used when an [`Ai`] request omits one.
    pub fn default_model(mut self, model: impl Into<ModelRef>) -> Self {
        self.default_model = Some(model.into());
        self
    }

    /// Register a typed tool.
    pub fn tool<T: AiTool>(mut self, tool: T) -> Self {
        self.tools.register(tool);
        self
    }

    /// Register an already-erased tool — e.g. a retriever exposed to agents via
    /// [`AiRetriever::as_tool`], so the model can invoke it (§D5).
    pub fn tool_dyn(mut self, tool: Arc<dyn DynTool>) -> Self {
        self.tools.register_dyn(tool);
        self
    }

    /// Register a typed retriever.
    pub fn retriever<R: AiRetriever>(mut self, retriever: R) -> Self {
        self.retrievers.register(retriever);
        self
    }

    /// Register a typed embedder.
    pub fn embedder<E: AiEmbedder>(mut self, embedder: E) -> Self {
        self.embedders.register(embedder);
        self
    }

    /// Provide a framework-mediated app resource under `key` (§D6).
    pub fn state<T: Any + Send + Sync>(mut self, key: impl Into<String>, value: T) -> Self {
        self.state.insert(key, value);
        self
    }

    /// Provide an already-shared app resource under `key`.
    pub fn state_arc<T: Any + Send + Sync>(
        mut self,
        key: impl Into<String>,
        value: Arc<T>,
    ) -> Self {
        self.state.insert_arc(key, value);
        self
    }

    /// Register a flow.
    pub fn flow<H: FlowHandler>(mut self, flow: H) -> Self {
        self.flows.register(flow);
        self
    }

    /// Provide a custom agent-thread store (defaults to an in-memory store).
    pub fn thread_store<S: AgentThreadStore>(mut self, store: S) -> Self {
        self.thread_store = Some(Arc::new(store));
        self
    }

    /// Wire the implementor-owned artifact capture sink (RFC-122 §1). Without
    /// one, `ctx.artifacts()` errors loudly and image-output models are
    /// rejected at request build.
    pub fn artifact_sink<S: super::artifact::ArtifactSink + 'static>(mut self, sink: S) -> Self {
        self.artifact_sink = Some(Arc::new(sink));
        self
    }

    /// Wire an already-shared artifact sink.
    pub fn artifact_sink_arc(mut self, sink: Arc<dyn super::artifact::ArtifactSink>) -> Self {
        self.artifact_sink = Some(sink);
        self
    }

    /// Override the per-stream ephemeral budget for `Append`-mode media
    /// chunks (RFC-122 §5.2; defaults to
    /// [`MAX_APPEND_STREAM_BYTES`](pocopine_agenkit_core::MAX_APPEND_STREAM_BYTES)).
    pub fn artifact_append_budget(mut self, bytes: usize) -> Self {
        self.artifact_append_budget = Some(bytes);
        self
    }

    /// Allowlist a model alias. Once any alias is allowlisted, only allowlisted
    /// aliases may be resolved — rejected before any provider call (§D10).
    pub fn allow_model(mut self, alias: impl Into<String>) -> Self {
        self.model_allowlist.insert(alias.into());
        self
    }

    /// Allowlist several model aliases.
    pub fn allow_models<I, S>(mut self, aliases: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.model_allowlist
            .extend(aliases.into_iter().map(Into::into));
        self
    }

    /// Finalize the runtime. Fails if no provider was registered.
    pub fn build(self) -> AgenkitResult<Agenkit> {
        if self.providers.is_empty() {
            return Err(AgenkitError::config(
                "Agenkit requires at least one provider",
            ));
        }
        Ok(Agenkit {
            inner: Arc::new(AgenkitInner {
                providers: self.providers,
                credentials: self
                    .credentials
                    .unwrap_or_else(|| Arc::new(super::credentials::EnvCredentials)),
                default_model: self.default_model,
                tools: self.tools,
                retrievers: self.retrievers,
                embedders: self.embedders,
                flows: self.flows,
                state: Arc::new(self.state),
                thread_store: self
                    .thread_store
                    .unwrap_or_else(|| Arc::new(SessionThreadStore::in_memory())),
                artifacts: self.artifact_sink.map(|sink| super::artifact::ArtifactRuntime {
                    sink,
                    append_budget: self
                        .artifact_append_budget
                        .unwrap_or(pocopine_agenkit_core::MAX_APPEND_STREAM_BYTES),
                }),
                model_allowlist: (!self.model_allowlist.is_empty()).then_some(self.model_allowlist),
                run_seq: AtomicU64::new(0),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::provider::MockProvider;
    use super::*;

    #[test]
    fn build_requires_a_provider() {
        assert!(Agenkit::builder().build().is_err());
        let ok = Agenkit::builder()
            .provider(MockProvider::new("local"))
            .default_model(ModelRef::new("local/default"))
            .build();
        assert!(ok.is_ok());
        assert_eq!(
            ok.unwrap().default_model().unwrap().as_str(),
            "local/default"
        );
    }
}
