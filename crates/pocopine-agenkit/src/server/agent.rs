//! Typed agents (RFC-093 Phase 2.6, §D4, §D7).
//!
//! An [`AiAgent`] is a typed runnable AI unit: a model alias, a system prompt,
//! an optional tool allowlist, and typed input/output. [`AgentRun`] executes a
//! bounded model+tool loop, validating structured output, and is `'static` so
//! it can also run as a parallel branch (Phase 2.6b).

use std::marker::PhantomData;
use std::sync::Arc;

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Message, ModelRef, Role, StepId, StepKind, StepStatus,
    ThreadMessage, ToolDescriptor, events,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::context::AiContext;
use super::provider::GenerateRequest;
use super::run::RunState;
use super::thread::AgentThreadHandle;

/// A typed, author-facing agent.
pub trait AiAgent: Send + Sync + 'static {
    /// Stable agent id.
    const ID: &'static str;

    /// Typed input (serialized into the agent's prompt).
    type Input: Serialize + Send + 'static;
    /// Typed, type-validated output (deserialized into `Self::Output`; schema
    /// keyword constraints like `range`/`length` are not re-checked). Derives
    /// `schemars::JsonSchema` so the agent's generation is schema-constrained
    /// where the provider supports it.
    type Output: Serialize + DeserializeOwned + schemars::JsonSchema + Send + 'static;

    /// Configure the agent's model, system prompt, tools, and limits.
    fn configure(builder: AiAgentBuilder<Self>) -> AiAgentBuilder<Self>
    where
        Self: Sized;
}

/// Configuration for an [`AiAgent`].
pub struct AiAgentBuilder<A: ?Sized> {
    model: Option<ModelRef>,
    system: Option<String>,
    tool_ids: Vec<String>,
    max_tokens: Option<u32>,
    max_steps: u32,
    _marker: PhantomData<fn() -> A>,
}

impl<A: ?Sized> Default for AiAgentBuilder<A> {
    fn default() -> Self {
        Self {
            model: None,
            system: None,
            tool_ids: Vec::new(),
            max_tokens: None,
            max_steps: 4,
            _marker: PhantomData,
        }
    }
}

impl<A: ?Sized> AiAgentBuilder<A> {
    /// Set the agent's model alias (defaults to the runtime default).
    pub fn model(mut self, model: impl Into<ModelRef>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the agent's system prompt.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Allow the agent to call a tool (by registry id).
    pub fn tool(mut self, id: impl Into<String>) -> Self {
        self.tool_ids.push(id.into());
        self
    }

    /// Allow the agent to call several tools (by registry id).
    pub fn tools<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tool_ids.extend(ids.into_iter().map(Into::into));
        self
    }

    /// Cap output tokens per model call.
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Bound the model+tool loop (default 4).
    pub fn max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }
}

/// A configured, runnable agent invocation. `'static`, so it can also be a
/// parallel branch.
pub struct AgentRun<A: AiAgent> {
    run: Arc<RunState>,
    parent_step: Option<StepId>,
    input: Option<A::Input>,
    thread: Option<AgentThreadHandle>,
}

impl<A: AiAgent> AgentRun<A> {
    pub(crate) fn new(run: Arc<RunState>, parent_step: Option<StepId>) -> Self {
        Self {
            run,
            parent_step,
            input: None,
            thread: None,
        }
    }

    /// Set the agent input.
    pub fn input(mut self, input: A::Input) -> Self {
        self.input = Some(input);
        self
    }

    /// Attach an existing thread (the run appends to its history).
    pub fn thread(mut self, thread: AgentThreadHandle) -> Self {
        self.thread = Some(thread);
        self
    }

    /// Execute the agent's model+tool loop and return validated output.
    pub async fn run(self) -> AgenkitResult<A::Output> {
        // Destructure up front so `input` can move out without partially
        // moving `self` (which the borrows below would reject).
        let Self {
            run,
            parent_step,
            input,
            thread,
        } = self;

        let config = A::configure(AiAgentBuilder::default());
        let model = config
            .model
            .clone()
            .or_else(|| run.inner.default_model.clone())
            .ok_or_else(|| AgenkitError::config(format!("agent `{}` has no model", A::ID)))?;
        run.inner.check_model_allowed(&model)?;
        let provider = run.inner.providers.resolve(&model)?;
        let input = input
            .ok_or_else(|| AgenkitError::config(format!("agent `{}` requires input", A::ID)))?;

        let agent_step = run.next_step_id();
        let mut started = run
            .event(
                events::AI_STEP_STARTED,
                StepKind::Agent,
                StepStatus::Started,
            )
            .with_step(agent_step.clone())
            .with_model(model.clone())
            .with_field("agent_id", A::ID);
        if let Some(parent) = &parent_step {
            started = started.with_parent(parent.clone());
        }
        run.emit(started);

        let result = run_loop::<A>(
            &run,
            &config,
            &model,
            &provider,
            &input,
            &agent_step,
            thread.as_ref(),
        )
        .await;
        match &result {
            Ok(_) => run.emit(
                run.event(
                    events::AI_STEP_COMPLETED,
                    StepKind::Agent,
                    StepStatus::Completed,
                )
                .with_step(agent_step.clone())
                .with_field("agent_id", A::ID),
            ),
            Err(error) => run.emit(
                run.event(events::AI_STEP_FAILED, StepKind::Agent, StepStatus::Failed)
                    .with_step(agent_step.clone())
                    .with_field("agent_id", A::ID)
                    .with_error(error.clone()),
            ),
        }

        let output = result?;
        if let Some(thread) = &thread {
            let input_text = serde_json::to_string(&input).unwrap_or_default();
            let output_text = serde_json::to_string(&output).unwrap_or_default();
            thread
                .store
                .append(
                    &thread.id,
                    vec![
                        ThreadMessage::new(Role::User, input_text),
                        ThreadMessage::new(Role::Assistant, output_text),
                    ],
                )
                .await?;
        }
        Ok(output)
    }
}

/// The bounded model+tool loop, factored out so it borrows the run state
/// rather than a partially-moved `AgentRun`.
async fn run_loop<A: AiAgent>(
    run: &Arc<RunState>,
    config: &AiAgentBuilder<A>,
    model: &ModelRef,
    provider: &Arc<dyn super::provider::Provider>,
    input: &A::Input,
    agent_step: &StepId,
    thread: Option<&AgentThreadHandle>,
) -> AgenkitResult<A::Output> {
    let ctx = AiContext::with_principal(run.inner.state.clone(), run.principal.clone());
    let tools: Vec<ToolDescriptor> = config
        .tool_ids
        .iter()
        .filter_map(|id| run.inner.tools.get(id).map(|tool| tool.descriptor()))
        .collect();
    let schema = super::schema::json_schema_for::<A::Output>();

    let mut messages = Vec::new();
    if let Some(thread) = thread {
        for message in thread.history().await? {
            messages.push(Message::new(message.role, message.content));
        }
    }
    messages.push(Message::user(
        serde_json::to_string(input).unwrap_or_default(),
    ));

    for _ in 0..config.max_steps {
        let request = GenerateRequest {
            model: model.clone(),
            system: config.system.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            json_schema: Some(schema.clone()),
            max_tokens: config.max_tokens,
        };

        let model_step = run.next_step_id();
        run.emit(
            run.event(
                events::AI_MODEL_REQUEST,
                StepKind::Generation,
                StepStatus::Started,
            )
            .with_step(model_step.clone())
            .with_parent(agent_step.clone())
            .with_model(model.clone()),
        );
        let response = provider.generate(request).await?;
        let mut completed = run
            .event(
                events::AI_MODEL_RESPONSE,
                StepKind::Generation,
                StepStatus::Completed,
            )
            .with_step(model_step)
            .with_parent(agent_step.clone())
            .with_model(model.clone());
        if let Some(usage) = response.usage {
            completed = completed.with_usage(usage);
        }
        run.emit(completed);

        if response.tool_calls.is_empty() {
            let value = response
                .structured_value()
                .cloned()
                .or_else(|| serde_json::from_str(&response.text_output()).ok())
                .ok_or_else(|| {
                    AgenkitError::validation(format!("agent `{}` returned no JSON", A::ID))
                })?;
            return serde_json::from_value(value).map_err(|err| {
                AgenkitError::validation(format!("agent `{}` output: {err}", A::ID))
            });
        }

        // Record the assistant's tool-request turn so the next request carries
        // the protocol linkage real providers (OpenAI, ...) require — keeping
        // any text the model emitted alongside the calls.
        messages.push(Message::assistant_tool_calls(
            response.content.clone(),
            response.tool_calls.clone(),
        ));

        for call in &response.tool_calls {
            // Enforce the agent's explicit tool allowlist (§D5/§D10).
            if !config.tool_ids.iter().any(|id| id == &call.tool_id) {
                return Err(AgenkitError::tool_policy(format!(
                    "agent `{}` called non-allowlisted tool `{}`",
                    A::ID,
                    call.tool_id
                )));
            }
            let tool = run.inner.tools.get(&call.tool_id).ok_or_else(|| {
                AgenkitError::tool_policy(format!("tool `{}` is not registered", call.tool_id))
            })?;
            let tool_step = run.next_step_id();
            run.emit(
                run.event(events::AI_TOOL_STARTED, StepKind::Tool, StepStatus::Started)
                    .with_step(tool_step.clone())
                    .with_parent(agent_step.clone())
                    .with_field("tool_id", call.tool_id.clone()),
            );
            let output = tool.call_json(call.args.clone(), ctx.clone()).await?;
            run.emit(
                run.event(
                    events::AI_TOOL_COMPLETED,
                    StepKind::Tool,
                    StepStatus::Completed,
                )
                .with_step(tool_step)
                .with_parent(agent_step.clone())
                .with_field("tool_id", call.tool_id.clone()),
            );
            messages.push(Message::tool_result(
                call.id.clone(),
                serde_json::to_string(&output).unwrap_or_default(),
            ));
        }
    }

    Err(AgenkitError::budget_exhausted(format!(
        "agent `{}` exceeded its {}-step loop",
        A::ID,
        config.max_steps
    )))
}
