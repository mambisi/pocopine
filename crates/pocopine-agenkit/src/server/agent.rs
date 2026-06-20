//! Typed agents (RFC-093 Phase 2.6, §D4, §D7).
//!
//! An [`AiAgent`] is a typed runnable AI unit: a model alias, a system prompt,
//! an optional tool allowlist, and typed input/output. [`AgentRun`] executes a
//! bounded model+tool loop, validating structured output, and is `'static` so
//! it can also run as a parallel branch (Phase 2.6b).

use std::marker::PhantomData;
use std::sync::Arc;

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Message, ModelRef, StepId, StepKind, StepStatus, ToolDescriptor,
    events,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::context::AiContext;
use super::loop_core::{self, ToolErrorMode};
use super::overflow::{context_headroom, estimate_input_tokens};
use super::provider::{GenerateRequest, Provider, ProviderContext};
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

        // The thread turn-append + compaction now happen inside `run_loop` on
        // the success path (it holds the resolved credential context).
        result
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
    // Honor the provider's declared tool capability: there is no graceful
    // degrade for tool calling, so an agent that needs tools on a provider that
    // can't call them fails fast rather than silently looping without them.
    if !tools.is_empty() && !provider.capabilities().tools {
        return Err(AgenkitError::config(format!(
            "provider `{}` does not support tool calling, but agent `{}` declares tools",
            provider.id(),
            A::ID
        )));
    }
    let schema = super::schema::json_schema_for::<A::Output>();

    let mut messages = Vec::new();
    if let Some(thread) = thread {
        // The compacted view (from the last checkpoint) — keeps a long-running
        // thread inside the model's context window (W7 compaction). Already
        // `Vec<Message>`, so it carries any persisted tool-call linkage verbatim.
        messages.extend(thread.active_history().await?);
    }
    // Serialize the input once (reused when persisting the turn). A failure is a
    // real error, not an empty prompt sent to the model.
    let input_json = serde_json::to_string(input)
        .map_err(|e| AgenkitError::internal(format!("agent `{}` input encode: {e}", A::ID)))?;
    messages.push(Message::user(input_json.clone()));

    // Resolve the provider credential once (W6): the principal and provider are
    // fixed across the agent loop, so the per-request context is too.
    let cx = {
        let credential = run
            .inner
            .credentials
            .resolve(provider.id(), &run.principal)
            .await?;
        super::provider::ProviderContext::for_request(credential)
    };

    // The shared loop core projects each step onto a `LoopObserver`; for the
    // typed run that's the trace projection (model request/response + usage, tool
    // spans), all parented to this agent step.
    let observer = loop_core::TraceObserver {
        run,
        agent_step: agent_step.clone(),
    };
    let agent_label = format!("agent `{}`", A::ID);

    for _ in 0..config.max_steps {
        let request = GenerateRequest {
            model: model.clone(),
            system: config.system.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            json_schema: Some(schema.clone()),
            max_tokens: config.max_tokens,
            // The agent loop doesn't request reasoning (W4 is scoped to the `Ai`
            // generate builder); defaults to `ThinkingLevel::Off`.
            thinking: Default::default(),
        };
        let response = loop_core::run_model_step(provider, request, &cx, model, &observer).await?;

        if response.tool_calls.is_empty() {
            let value = response
                .structured_value()
                .cloned()
                .or_else(|| serde_json::from_str(&response.text_output()).ok())
                .ok_or_else(|| {
                    AgenkitError::validation(format!("agent `{}` returned no JSON", A::ID))
                })?;
            let output: A::Output = serde_json::from_value(value).map_err(|err| {
                AgenkitError::validation(format!("agent `{}` output: {err}", A::ID))
            })?;
            // Persist this turn to the thread, then compact it if the history now
            // overflows the model's context window (W7).
            if let Some(thread) = thread {
                // A serialize failure persists nothing and errors, rather than
                // writing an empty turn to the durable thread.
                let output_json = serde_json::to_string(&output).map_err(|e| {
                    AgenkitError::internal(format!("agent `{}` output encode: {e}", A::ID))
                })?;
                thread
                    .store
                    .append(
                        &thread.id,
                        thread.owner(),
                        vec![
                            Message::user(input_json.clone()),
                            Message::assistant(output_json),
                        ],
                    )
                    .await?;
                maybe_compact::<A>(run, config, model, provider, &cx, thread).await?;
            }
            return Ok(output);
        }

        // Run the tool batch via the shared dispatcher. A tool error propagates
        // (a typed run fails as a whole); no `before_tool_call` hook on this path.
        messages.extend(
            loop_core::dispatch_tool_calls(
                &run.inner,
                &config.tool_ids,
                &ctx,
                &response,
                &agent_label,
                // No steering hook on the typed run; `+ Sync` keeps the loop's
                // future `Send` (it runs inside Send flow handlers).
                None::<&(dyn Fn(&str, &serde_json::Value) -> loop_core::ToolDecision + Sync)>,
                ToolErrorMode::Propagate,
                &observer,
            )
            .await?,
        );
    }

    Err(AgenkitError::budget_exhausted(format!(
        "agent `{}` exceeded its {}-step loop",
        A::ID,
        config.max_steps
    )))
}

/// After a turn is persisted, compact the thread if its (compacted) history
/// would overflow the model's context window (W7 — consumes the W3 headroom
/// signal). Summarizes the history into a checkpoint; `active_history` then
/// resumes from it, while the full log is retained for audit/fork.
async fn maybe_compact<A: AiAgent>(
    run: &Arc<RunState>,
    config: &AiAgentBuilder<A>,
    model: &ModelRef,
    provider: &Arc<dyn Provider>,
    cx: &ProviderContext,
    thread: &AgentThreadHandle,
) -> AgenkitResult<()> {
    let max_output = config.max_tokens.unwrap_or(1024);
    if let Some((folded, kept)) = compact_thread(thread, model, provider, cx, max_output).await? {
        run.emit(
            run.event(
                events::AI_THREAD_CHECKPOINTED,
                StepKind::Custom,
                StepStatus::Completed,
            )
            .with_field("thread_id", thread.id().as_str())
            .with_field("agent_id", A::ID)
            .with_field("compacted_messages", folded)
            .with_field("kept_verbatim", kept),
        );
    }
    Ok(())
}

/// Compact a thread if its active history would overflow the model's window
/// (consumes the W3 headroom signal → writes a W7 checkpoint). Keeps the recent
/// tail verbatim (token-bounded) and folds the older prefix into one summary.
/// Returns `Some((folded, kept))` when it summarized, `None` otherwise.
///
/// Shared by the single-shot agent loop ([`maybe_compact`]) and the long-lived
/// runtime ([`super::runtime`]); the caller emits its own checkpoint event.
pub(crate) async fn compact_thread(
    thread: &AgentThreadHandle,
    model: &ModelRef,
    provider: &Arc<dyn Provider>,
    cx: &ProviderContext,
    max_output: u32,
) -> AgenkitResult<Option<(u64, u64)>> {
    // Without catalog metadata we can't size the window — skip (degrade, no error).
    let Some(catalog_model) = super::catalog::lookup(model) else {
        return Ok(None);
    };
    let history = thread.active_history().await?;
    if history.len() < 4 {
        return Ok(None); // nothing meaningful to summarize yet
    }
    // `active_history` is already `Vec<Message>` (full fidelity), so size and
    // split it directly — no lossy reconstruction, and the kept tail retains any
    // tool-call linkage verbatim.
    if !context_headroom(catalog_model, &history, max_output).over {
        return Ok(None); // still fits — no compaction
    }

    // Keep the recent tail verbatim (token-bounded — see `recent_keep_count`),
    // fold the older prefix into the summary.
    let keep = recent_keep_count(&history, KEEP_RECENT_VERBATIM_TOKENS);
    let (older, recent) = history.split_at(history.len() - keep);

    let transcript = older
        .iter()
        .map(|m| format!("[{:?}] {}", m.role, m.content.as_text()))
        .collect::<Vec<_>>()
        .join("\n");
    let summary = summarize(transcript, model, provider, cx).await?;
    let (folded, kept) = (older.len() as u64, recent.len() as u64);
    thread
        .checkpoint(Message::system(summary), recent.to_vec())
        .await?;
    Ok(Some((folded, kept)))
}

/// The token budget for the recent tail [`maybe_compact`] keeps verbatim. Older
/// turns fold into one summary; the most recent turns stay literal up to this
/// budget, so the model still sees the live turns it is mid-way through — but
/// bounded in tokens, so the kept tail can't re-overflow the window.
const KEEP_RECENT_VERBATIM_TOKENS: u64 = 2048;

/// How many of the most recent `messages` to keep verbatim on compaction: the
/// newest turns whose cumulative token estimate stays within `budget`, always
/// leaving at least one message to summarize. A single turn larger than `budget`
/// is kept zero times (summarized instead) — so the kept tail can never itself
/// re-overflow the window, and a huge recent turn isn't copied into the
/// checkpoint payload (which would re-inline its externalized blob).
fn recent_keep_count(messages: &[Message], budget: u64) -> usize {
    let max_keep = messages.len().saturating_sub(1);
    let mut kept_tokens = 0u64;
    let mut keep = 0usize;
    for message in messages.iter().rev() {
        if keep == max_keep {
            break;
        }
        let cost = estimate_input_tokens(std::slice::from_ref(message));
        if kept_tokens.saturating_add(cost) > budget {
            break;
        }
        kept_tokens = kept_tokens.saturating_add(cost);
        keep += 1;
    }
    keep
}

/// Summarize a conversation transcript into a single compaction summary via a
/// plain-text model call (no tools, no schema).
async fn summarize(
    transcript: String,
    model: &ModelRef,
    provider: &Arc<dyn Provider>,
    cx: &ProviderContext,
) -> AgenkitResult<String> {
    let request = GenerateRequest {
        model: model.clone(),
        system: Some(
            "You are compacting an agent conversation to fit its context window. \
             Summarize the transcript below concisely, preserving facts, decisions, \
             tool results, and open tasks. Output only the summary."
                .to_string(),
        ),
        messages: vec![Message::user(transcript)],
        tools: Vec::new(),
        json_schema: None,
        max_tokens: Some(1024),
        thinking: Default::default(),
    };
    Ok(provider
        .generate(request, cx)
        .await
        .map_err(super::generate::reclassify_overflow)?
        .text_output())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_agenkit_core::Role;

    fn msg(text: &str) -> Message {
        Message::new(Role::User, text)
    }

    #[test]
    fn recent_keep_count_is_token_bounded_not_count_bounded() {
        // Tiny messages + a generous budget → keep all but one (≥1 to summarize).
        let tiny: Vec<Message> = (0..4).map(|i| msg(&format!("m{i}"))).collect();
        assert_eq!(recent_keep_count(&tiny, 10_000), 3);

        // A huge most-recent turn that alone exceeds the budget → keep 0, so it is
        // summarized; the kept tail can never itself re-overflow the window.
        let huge = msg(&"x".repeat(40_000)); // ~10k tokens
        let mixed = vec![msg("a"), msg("b"), msg("c"), huge];
        assert_eq!(recent_keep_count(&mixed, 2048), 0);

        // Small recent turns accumulate up to the budget, then stop (~1004 tok
        // each: 2 fit in 2500, the 3rd would be 3012).
        let many: Vec<Message> = (0..10).map(|_| msg(&"y".repeat(4000))).collect();
        assert_eq!(recent_keep_count(&many, 2500), 2);
    }
}
