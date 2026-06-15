//! Flows and the flow context (RFC-093 Phase 2.5, §D4, §D6, §D7).
//!
//! A [`Flow`] is the app-callable unit: typed input/output, a declared context
//! manifest, and a body that runs generation, retrieval, custom steps, and
//! (later) agents/parallel branches under one trace tree. [`AiFlowContext`] is
//! handed to the body; it exposes `ai()`, `step(...)`, and `retrieve::<R>()`,
//! recording framework-mediated access against the manifest and emitting
//! advisory `ai_context_gap` diagnostics for undeclared access (§D6).

use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, ContextGapDiagnostic, ContextManifest, FlowDescriptor,
    ResourceKind, RetrievalSet, RunId, StepId, StepKind, StepStatus, StreamMode, TraceId, events,
};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::agent::{AgentRun, AiAgent};
use super::context::AiContext;
use super::generate::Ai;
use super::parallel::ParallelBuilder;
use super::provider::BoxFuture;
use super::reduce::ReduceBuilder;
use super::retrieval::AiRetriever;
use super::run::RunState;
use super::schema::schema_ref_for;
use super::thread::ThreadBuilder;

/// An object-safe registered flow: validates JSON input, runs the typed body,
/// and serializes the typed output.
pub trait FlowHandler: Send + Sync + 'static {
    /// The flow id.
    fn id(&self) -> &str;
    /// The flow descriptor (schemas + manifest + stream mode).
    fn descriptor(&self) -> FlowDescriptor;
    /// Run the flow over JSON input within `ctx`.
    fn run_json<'a>(
        &'a self,
        input: serde_json::Value,
        ctx: AiFlowContext,
    ) -> BoxFuture<'a, AgenkitResult<serde_json::Value>>;
}

/// A typed flow: `id` + a body `Fn(Input, AiFlowContext) -> Future<Output>`.
pub struct Flow<I, O, F> {
    descriptor: FlowDescriptor,
    handler: F,
    _marker: PhantomData<fn(I) -> O>,
}

impl<I, O, F, Fut> Flow<I, O, F>
where
    I: DeserializeOwned + JsonSchema + Send + 'static,
    O: Serialize + JsonSchema + Send + 'static,
    F: Fn(I, AiFlowContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = AgenkitResult<O>> + Send + 'static,
{
    /// Register a flow body under `id`. The input/output JSON schemas are
    /// derived from `I`/`O` via `schemars`, so the public server contract and
    /// context manifest carry the real shapes.
    pub fn new(id: impl Into<String>, handler: F) -> Self {
        let descriptor = FlowDescriptor::new(id)
            .input(schema_ref_for::<I>())
            .output(schema_ref_for::<O>());
        Self {
            descriptor,
            handler,
            _marker: PhantomData,
        }
    }

    /// Mark the flow public (exposed as a server function).
    pub fn public(mut self) -> Self {
        self.descriptor = self.descriptor.public();
        self
    }

    /// Set the public stream mode.
    pub fn stream_mode(mut self, mode: StreamMode) -> Self {
        self.descriptor = self.descriptor.stream_mode(mode);
        self
    }

    /// Declare a tool dependency.
    pub fn uses_tool(mut self, id: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.uses_tool(id);
        self
    }

    /// Declare a retriever dependency.
    pub fn uses_retriever(mut self, id: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.uses_retriever(id);
        self
    }

    /// Declare an agent dependency.
    pub fn uses_agent(mut self, id: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.uses_agent(id);
        self
    }

    /// Declare an app-state dependency.
    pub fn uses_state(mut self, key: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.uses_state(key);
        self
    }
}

impl<I, O, F, Fut> FlowHandler for Flow<I, O, F>
where
    I: DeserializeOwned + Send + 'static,
    O: Serialize + Send + 'static,
    F: Fn(I, AiFlowContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = AgenkitResult<O>> + Send + 'static,
{
    fn id(&self) -> &str {
        &self.descriptor.id
    }

    fn descriptor(&self) -> FlowDescriptor {
        self.descriptor.clone()
    }

    fn run_json<'a>(
        &'a self,
        input: serde_json::Value,
        ctx: AiFlowContext,
    ) -> BoxFuture<'a, AgenkitResult<serde_json::Value>> {
        Box::pin(async move {
            let input: I = serde_json::from_value(input).map_err(|err| {
                AgenkitError::validation(format!("flow `{}` input: {err}", self.descriptor.id))
            })?;
            let output = (self.handler)(input, ctx).await?;
            serde_json::to_value(output).map_err(|err| {
                AgenkitError::validation(format!("flow `{}` output: {err}", self.descriptor.id))
            })
        })
    }
}

/// A registry of flows keyed by id.
#[derive(Default, Clone)]
pub struct FlowRegistry {
    flows: HashMap<String, Arc<dyn FlowHandler>>,
}

impl FlowRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a flow.
    pub fn register<H: FlowHandler>(&mut self, flow: H) {
        self.flows.insert(flow.id().to_string(), Arc::new(flow));
    }

    /// Look up a flow by id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn FlowHandler>> {
        self.flows.get(id).cloned()
    }

    /// Whether a flow id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.flows.contains_key(id)
    }
}

/// The context handed to a flow body. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct AiFlowContext {
    run: Arc<RunState>,
    flow_id: String,
    manifest: ContextManifest,
    parent_step: Option<StepId>,
}

impl AiFlowContext {
    pub(crate) fn new(run: Arc<RunState>, flow_id: String, manifest: ContextManifest) -> Self {
        Self {
            run,
            flow_id,
            manifest,
            parent_step: None,
        }
    }

    /// The run id for this invocation.
    pub fn run_id(&self) -> &RunId {
        &self.run.run_id
    }

    /// The trace id for this invocation.
    pub fn trace_id(&self) -> &TraceId {
        &self.run.trace_id
    }

    pub(crate) fn run_state(&self) -> &Arc<RunState> {
        &self.run
    }

    /// A traced generation request bound to this run.
    pub fn ai(&self) -> Ai {
        Ai::new(self.run.inner.clone()).with_tracer(self.run.clone(), self.parent_step.clone())
    }

    /// Begin a typed agent run under this flow's trace tree.
    pub fn agent<A: AiAgent>(&self) -> AgentRun<A> {
        AgentRun::new(self.run.clone(), self.parent_step.clone())
    }

    /// Open or create thread state for agent `A`.
    pub fn thread<A: AiAgent>(&self) -> ThreadBuilder {
        ThreadBuilder::new(self.run.clone(), A::ID)
    }

    /// Reduce parallel candidates into one typed result (§D7).
    pub fn reduce<C>(&self, name: impl Into<String>, candidates: Vec<C>) -> ReduceBuilder<'_, C> {
        ReduceBuilder::new(self, name, candidates)
    }

    /// Begin a bounded parallel fan-out group (§D7).
    pub fn parallel<T: Send + 'static>(&self, group: impl Into<String>) -> ParallelBuilder<'_, T> {
        ParallelBuilder::new(self, group)
    }

    /// Fetch a framework-mediated app resource by key (§D6). Prefer declaring
    /// it via `Flow::uses_state(...)` so the access is traceable.
    pub fn state<T: std::any::Any + Send + Sync>(
        &self,
        key: &str,
    ) -> pocopine_agenkit_core::AgenkitResult<Arc<T>> {
        self.record_access(ResourceKind::State, key);
        self.exec_context().state(key)
    }

    /// The caller principal this flow runs under (anonymous unless invoked with
    /// one via `flow(id).principal(..)` or the `PrincipalLayer` scope) (§D5/§D10).
    pub fn principal(&self) -> &pocopine_auth::Principal {
        &self.run.principal
    }

    /// An execution context over the runtime's app state, bound to the caller
    /// principal so tools/retrieval run under the request identity.
    fn exec_context(&self) -> AiContext {
        AiContext::with_principal(self.run.inner.state.clone(), self.run.principal.clone())
    }

    /// Record framework-mediated access; emit an advisory gap diagnostic when
    /// the resource was not declared in the flow's manifest (§D6).
    fn record_access(&self, kind: ResourceKind, id: &str) {
        if !self.manifest.contains(kind, id) {
            let diagnostic = ContextGapDiagnostic::new(self.flow_id.clone(), kind, id);
            self.run.emit(
                self.run
                    .event(
                        events::AI_CONTEXT_GAP,
                        StepKind::Custom,
                        StepStatus::Completed,
                    )
                    .with_field("flow_id", self.flow_id.clone())
                    .with_field("resource_id", diagnostic.resource_id)
                    .with_field("message", diagnostic.message),
            );
        }
    }

    /// Wrap custom app work in a traced step under this flow's trace tree.
    pub async fn step<T, Fut>(&self, name: &str, work: Fut) -> AgenkitResult<T>
    where
        Fut: Future<Output = AgenkitResult<T>>,
    {
        let step_id = self.run.next_step_id();
        let mut started = self
            .run
            .event(
                events::AI_STEP_STARTED,
                StepKind::Custom,
                StepStatus::Started,
            )
            .with_step(step_id.clone())
            .with_field("name", name);
        if let Some(parent) = &self.parent_step {
            started = started.with_parent(parent.clone());
        }
        self.run.emit(started);

        let result = work.await;
        match &result {
            Ok(_) => self.run.emit(
                self.run
                    .event(
                        events::AI_STEP_COMPLETED,
                        StepKind::Custom,
                        StepStatus::Completed,
                    )
                    .with_step(step_id)
                    .with_field("name", name),
            ),
            Err(error) => self.run.emit(
                self.run
                    .event(events::AI_STEP_FAILED, StepKind::Custom, StepStatus::Failed)
                    .with_step(step_id)
                    .with_field("name", name)
                    .with_error(error.clone()),
            ),
        }
        result
    }

    /// Begin a deterministic retrieval against retriever `R`.
    pub fn retrieve<R: AiRetriever>(&self) -> RetrieveBuilder<'_, R>
    where
        R::Query: Serialize,
    {
        RetrieveBuilder {
            ctx: self,
            query: None,
            top_k: None,
            _marker: PhantomData,
        }
    }
}

/// Builder for a deterministic retrieval (`ctx.retrieve::<R>()`).
pub struct RetrieveBuilder<'a, R: AiRetriever> {
    ctx: &'a AiFlowContext,
    query: Option<R::Query>,
    top_k: Option<u32>,
    _marker: PhantomData<R>,
}

impl<'a, R: AiRetriever> RetrieveBuilder<'a, R>
where
    R::Query: Serialize,
{
    /// Set the typed query.
    pub fn query(mut self, query: R::Query) -> Self {
        self.query = Some(query);
        self
    }

    /// Limit the number of hits (best-effort: merged into the query object).
    pub fn top_k(mut self, k: u32) -> Self {
        self.top_k = Some(k);
        self
    }

    /// Execute the retrieval, emitting `ai_retrieval_*` trace events.
    pub async fn run(self) -> AgenkitResult<RetrievalSet> {
        let retriever = self.ctx.run.inner.retrievers.get(R::ID).ok_or_else(|| {
            AgenkitError::not_found(format!("retriever `{}` is not registered", R::ID))
        })?;
        self.ctx.record_access(ResourceKind::Retriever, R::ID);

        let query = self.query.ok_or_else(|| {
            AgenkitError::config(format!("retriever `{}` requires a query", R::ID))
        })?;
        let mut value = serde_json::to_value(query).map_err(|err| {
            AgenkitError::validation(format!("retriever `{}` query: {err}", R::ID))
        })?;
        if let (Some(k), Some(object)) = (self.top_k, value.as_object_mut()) {
            object
                .entry("top_k")
                .or_insert_with(|| serde_json::json!(k));
        }

        let step_id = self.ctx.run.next_step_id();
        self.ctx.run.emit(
            self.ctx
                .run
                .event(
                    events::AI_RETRIEVAL_STARTED,
                    StepKind::Retrieval,
                    StepStatus::Started,
                )
                .with_step(step_id.clone())
                .with_field("retriever_id", R::ID),
        );

        let result = retriever
            .retrieve_json(value, self.ctx.exec_context())
            .await;
        match &result {
            Ok(set) => self.ctx.run.emit(
                self.ctx
                    .run
                    .event(
                        events::AI_RETRIEVAL_COMPLETED,
                        StepKind::Retrieval,
                        StepStatus::Completed,
                    )
                    .with_step(step_id)
                    .with_field("retriever_id", R::ID)
                    .with_field("hit_count", set.len() as u64),
            ),
            Err(error) => self.ctx.run.emit(
                self.ctx
                    .run
                    .event(
                        events::AI_RETRIEVAL_FAILED,
                        StepKind::Retrieval,
                        StepStatus::Failed,
                    )
                    .with_step(step_id)
                    .with_field("retriever_id", R::ID)
                    .with_error(error.clone()),
            ),
        }
        result
    }
}
