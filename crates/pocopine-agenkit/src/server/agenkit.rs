//! The `Agenkit` facade and its builder (RFC-093 Phase 2.2, §D3).
//!
//! `Agenkit` is the one canonical entry point. Apps configure it once and then
//! call [`Agenkit::ai`] (and, in later checkpoints, registered flows/agents)
//! from server functions, jobs, and evals.

use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, FlowStreamEvent, ModelRef, RunId, StepKind, StepStatus, TraceId,
    events,
};
use pocopine_auth::Principal;
use serde::{Serialize, de::DeserializeOwned};
use std::future::Future;
use tokio::sync::mpsc::UnboundedSender;

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

fn current_principal() -> Principal {
    CURRENT_PRINCIPAL
        .try_with(Clone::clone)
        .unwrap_or_else(|_| Principal::anonymous())
}

use super::context::{AiContext, AppState};
use super::embed::{AiEmbedder, EmbedderRegistry};
use super::flow::{AiFlowContext, FlowHandler, FlowRegistry};
use super::generate::Ai;
use super::provider::{Provider, ProviderRegistry};
use super::retrieval::{AiRetriever, RetrieverRegistry};
use super::run::RunState;
use super::thread::{AgentThreadStore, InMemoryThreadStore};
use super::tool::{AiTool, ToolRegistry};

/// Shared, immutable runtime state behind an [`Agenkit`] handle.
pub(crate) struct AgenkitInner {
    pub(crate) providers: ProviderRegistry,
    pub(crate) default_model: Option<ModelRef>,
    pub(crate) tools: ToolRegistry,
    pub(crate) retrievers: RetrieverRegistry,
    pub(crate) embedders: EmbedderRegistry,
    pub(crate) flows: FlowRegistry,
    pub(crate) state: Arc<AppState>,
    pub(crate) thread_store: Arc<dyn AgentThreadStore>,
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

    /// Begin a flow invocation: `agenkit.flow(id).input(x).run().await`.
    ///
    /// The chain replaces the old `run_flow*` methods. `.input(..)` is optional
    /// (defaults to `null`, which deserializes to a `()`-input flow);
    /// `.principal(..)` is optional (defaults to the ambient principal scoped by
    /// [`crate::server::PrincipalLayer`]); the terminal is `.run::<O>()` (typed
    /// output) or `.stream(sink)` (public [`FlowStreamEvent`]s).
    pub fn flow(&self, id: impl Into<String>) -> FlowCall<'_> {
        FlowCall {
            agenkit: self,
            id: id.into(),
            input: Ok(serde_json::Value::Null),
            principal: None,
        }
    }
}

/// A pending flow invocation built by [`Agenkit::flow`].
pub struct FlowCall<'a> {
    agenkit: &'a Agenkit,
    id: String,
    /// Serialized eagerly in [`FlowCall::input`]; a serialize failure is stashed
    /// and surfaced at the terminal.
    input: AgenkitResult<serde_json::Value>,
    principal: Option<Principal>,
}

impl FlowCall<'_> {
    /// Set the typed flow input (serialized now). Omit for a `()`-input flow.
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

    /// Run the flow streaming public [`FlowStreamEvent`]s into `sink` (client-
    /// safe by construction, §D8). Returns the final raw output (which also
    /// rides the sink as `OutputDelta`/`OutputCompleted`).
    pub async fn stream(
        self,
        sink: UnboundedSender<FlowStreamEvent>,
    ) -> AgenkitResult<serde_json::Value> {
        let input = self.input?;
        self.agenkit
            .run_flow_inner(&self.id, input, self.principal, Some(sink))
            .await
    }
}

/// Builder for [`Agenkit`].
#[derive(Default)]
pub struct AgenkitBuilder {
    providers: ProviderRegistry,
    default_model: Option<ModelRef>,
    tools: ToolRegistry,
    retrievers: RetrieverRegistry,
    embedders: EmbedderRegistry,
    flows: FlowRegistry,
    state: AppState,
    thread_store: Option<Arc<dyn AgentThreadStore>>,
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
                default_model: self.default_model,
                tools: self.tools,
                retrievers: self.retrievers,
                embedders: self.embedders,
                flows: self.flows,
                state: Arc::new(self.state),
                thread_store: self
                    .thread_store
                    .unwrap_or_else(|| Arc::new(InMemoryThreadStore::new())),
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
