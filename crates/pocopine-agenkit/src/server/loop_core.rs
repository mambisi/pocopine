//! Shared model↔tool **loop primitives** used by BOTH the single-shot typed
//! agent loop ([`run_loop`](super::agent)) and the long-lived conversational
//! runtime ([`AgentLoop`](super::runtime)).
//!
//! The two loops differ in *lifecycle* (one typed `input → output` run vs. a
//! persistent conversation) and in *termination* (structured output vs. idle /
//! steering) — but a single **step** is identical: build the request, call the
//! model (emitting the request/response trace + token usage), then dispatch the
//! tool batch (allowlist, `before_tool_call` hook, execute, feed back). Those
//! mechanics live here once, parameterized by a [`LoopObserver`] so each loop
//! projects the same step onto its own surface: the typed run emits only
//! `pocopine.trace` events ([`TraceObserver`]); the runtime emits the
//! [`AgentEvent`](super::runtime::AgentEvent) firehose *and* the same trace (its
//! observer wraps a [`TraceObserver`]) — which is how conversational turns get
//! cost/usage metering, closing the formerly-duplicated loops.

use std::sync::Arc;

use futures::StreamExt;
use futures::future::BoxFuture;
use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Content, ContentPart, Message, ModelRef, StepId, StepKind,
    StepStatus, ToolCall, Usage, events,
};

use super::agenkit::AgenkitInner;
use super::context::AiContext;
use super::provider::{
    FinishReason, GenerateRequest, GenerateResponse, Provider, ProviderContext, StreamChunk,
};
use super::run::RunState;

/// A `before_tool_call` hook's decision for one tool call (L3). Mirrors the
/// (parked) WIT `hook-decision` so the extension world projects onto it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ToolDecision {
    /// Run the tool with its requested arguments.
    Proceed,
    /// Don't run the tool; feed `reason` back to the model (an approval/trust
    /// gate, or a policy block).
    Block {
        /// Why the call is blocked.
        reason: String,
    },
    /// Run the tool, but with these arguments instead (e.g. inject a sandbox
    /// path, redact a field).
    ReplaceArgs {
        /// The arguments to use.
        args: serde_json::Value,
    },
}

/// How the loop reacts to a tool returning an error.
#[derive(Clone, Copy)]
pub(crate) enum ToolErrorMode {
    /// Propagate it — aborts the loop. The single-shot typed run uses this: a
    /// tool error means the run as a whole failed.
    Propagate,
    /// Feed the error back to the model as the tool result so it can recover.
    /// The conversational runtime uses this: one bad call shouldn't kill a long
    /// session (it's bounded by the per-turn step budget).
    FeedBack,
}

/// The observable points of one model↔tool step. Each loop implements only the
/// surface it has — the default no-ops let the typed run skip the
/// firehose-specific points (it propagates tool errors and has no hooks, so
/// `tool_failed`/`tool_blocked` never fire). The [`StepId`] returned by
/// `model_request`/`tool_started` is paired back into the matching
/// `model_response`/`tool_completed` so a tracer can span the call.
pub(crate) trait LoopObserver {
    /// A model request is about to go out; returns the trace step it opened.
    fn model_request(&self, _model: &ModelRef) -> Option<StepId> {
        None
    }
    /// A user-visible fragment of the assistant's answer arrived on the model's
    /// response stream (only [`run_model_step_streamed`] reports these). The
    /// assembled text still arrives whole via the response — deltas are a live
    /// view, not a substitute.
    fn assistant_delta(&self, _delta: &str) {}
    /// The model responded (`usage` = token accounting, if the provider gave it).
    fn model_response(&self, _step: Option<StepId>, _model: &ModelRef, _usage: Option<Usage>) {}
    /// A tool is about to run; returns the trace step it opened.
    fn tool_started(&self, _call: &ToolCall) -> Option<StepId> {
        None
    }
    /// A tool returned successfully.
    fn tool_completed(&self, _step: Option<StepId>, _call: &ToolCall, _output: &serde_json::Value) {
    }
    /// A tool failed (its error is fed back to the model under [`ToolErrorMode::FeedBack`]).
    fn tool_failed(&self, _call: &ToolCall, _error: &str) {}
    /// A `before_tool_call` hook blocked a tool (its reason replaces the result).
    fn tool_blocked(&self, _call: &ToolCall, _reason: &str) {}
}

/// Call the model for one step: emit the request span, generate (reclassifying an
/// "input too long" provider error as `ContextOverflow`, W3), then emit the
/// response span with token usage. Returns the raw response; the *caller* decides
/// termination (structured output vs. idle/steer) and surfaces any assistant text.
pub(crate) async fn run_model_step(
    provider: &Arc<dyn Provider>,
    request: GenerateRequest,
    cx: &ProviderContext,
    model: &ModelRef,
    // `+ Sync`: the reference is held across the model `.await` inside a turn that
    // runs on a spawned (Send) task, so the observer must be shareable.
    observer: &(dyn LoopObserver + Sync),
) -> AgenkitResult<GenerateResponse> {
    let step = observer.model_request(model);
    let response = provider
        .generate(request, cx)
        .await
        .map_err(super::generate::reclassify_overflow)?;
    observer.model_response(step, model, response.usage);
    Ok(response)
}

/// Call the model for one step, **streaming**: like [`run_model_step`], but
/// consumes the provider's incremental stream — reporting each user-visible text
/// fragment through [`LoopObserver::assistant_delta`] as it arrives — and then
/// assembles the same finished [`GenerateResponse`] (reasoning + text + tool
/// calls + usage) the one-shot path returns, so the caller's termination logic
/// is unchanged. A provider without native streaming degrades to a single delta
/// carrying the whole answer (§D13). The assembly mirrors the flow path's
/// `run_streamed` (`server/generate.rs`): reasoning rides the response content
/// server-side only — it is never reported as a delta (§D10).
pub(crate) async fn run_model_step_streamed(
    provider: &Arc<dyn Provider>,
    request: GenerateRequest,
    cx: &ProviderContext,
    model: &ModelRef,
    observer: &(dyn LoopObserver + Sync),
) -> AgenkitResult<GenerateResponse> {
    let step = observer.model_request(model);

    // Honor the declared streaming capability: stream natively when supported,
    // else adapt a one-shot generation to the streaming contract (§D13).
    let mut stream = if provider.capabilities().streaming {
        provider.generate_stream(request, cx)
    } else {
        super::provider::fallback_stream(provider.as_ref(), request, cx)
    };

    let mut text = String::new();
    let mut thinking_parts: Vec<ContentPart> = Vec::new();
    let mut tool_calls = Vec::new();
    let mut usage = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(StreamChunk::Text(delta)) => {
                observer.assistant_delta(&delta);
                text.push_str(&delta);
            }
            Ok(StreamChunk::Thinking { text, signature }) => {
                thinking_parts.push(ContentPart::thinking(text, signature));
            }
            Ok(StreamChunk::ToolCall(call)) => tool_calls.push(call),
            Ok(StreamChunk::Usage(reported)) => usage = Some(reported),
            // A mid-stream failure fails the step (partial text is discarded —
            // the caller's turn fails and nothing is persisted, so a consumer
            // never sees deltas confirmed by a final message).
            Err(error) => return Err(super::generate::reclassify_overflow(error)),
        }
    }
    drop(stream);

    let finish_reason = if tool_calls.is_empty() {
        FinishReason::Stop
    } else {
        FinishReason::ToolCalls
    };
    // Reasoning parts (if any) precede the answer text — parity with the
    // non-streamed response shape, so replay/persistence see one contract.
    let content = if thinking_parts.is_empty() {
        Content::text(text)
    } else {
        thinking_parts.push(ContentPart::text(text));
        Content::from_parts(thinking_parts)
    };
    let response = GenerateResponse {
        content,
        tool_calls,
        usage,
        finish_reason,
    };
    observer.model_response(step, model, response.usage);
    Ok(response)
}

/// Dispatch one assistant turn's tool calls. Records the assistant tool-request
/// message (keeping the provider protocol linkage real providers require on the
/// next request), then for each call: enforce the allowlist, run the
/// `before_tool_call` hook (block / replace), and execute — appending each tool
/// result. Returns the messages to append to the transcript. A tool error follows
/// `error_mode`. `agent_label` prefixes the allowlist-violation error (e.g.
/// ``agent `weather` `` for the typed run, `agent` for the runtime).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_tool_calls<H>(
    inner: &Arc<AgenkitInner>,
    allowlist: &[String],
    ctx: &AiContext,
    response: &GenerateResponse,
    agent_label: &str,
    before_tool_call: Option<&H>,
    error_mode: ToolErrorMode,
    observer: &(dyn LoopObserver + Sync),
) -> AgenkitResult<Vec<Message>>
where
    H: for<'a> Fn(&'a ToolCall) -> BoxFuture<'a, ToolDecision> + ?Sized,
{
    let mut out = Vec::with_capacity(response.tool_calls.len() + 1);
    // The assistant's tool-request turn — kept (with any text the model emitted
    // alongside the calls) so the next request carries the protocol linkage.
    out.push(Message::assistant_tool_calls(
        response.content.clone(),
        response.tool_calls.clone(),
    ));

    for call in &response.tool_calls {
        // Enforce the explicit tool allowlist (§D5/§D10), then that the tool
        // exists. Both refuse to run the call; what differs is whether
        // refusing should end the run.
        //
        // `Propagate` (the typed run) keeps the hard error: a typed agent's
        // allowlist is written next to its tools, so a call outside it is a
        // programming error and failing loudly is the point.
        //
        // `FeedBack` (the runtime) tells the model instead. A chat model that
        // names a tool it was not given — because the system prompt mentions
        // one, or because the profile silently lost its tools — would
        // otherwise destroy the user's entire turn over a recoverable mistake.
        // The model can answer without it or say what it would need. The call
        // is still NOT executed, so the policy guarantee is unchanged; only the
        // blast radius shrinks. Repetition is bounded by the caller's step
        // limit.
        let refusal = if allowlist.iter().any(|id| id == &call.tool_id) {
            inner
                .tools
                .get(&call.tool_id)
                .is_none()
                .then(|| format!("tool `{}` is not registered", call.tool_id))
        } else {
            Some(format!(
                "{agent_label} called non-allowlisted tool `{}`",
                call.tool_id
            ))
        };
        if let Some(reason) = refusal {
            match error_mode {
                ToolErrorMode::Propagate => return Err(AgenkitError::tool_policy(reason)),
                ToolErrorMode::FeedBack => {
                    // Observed, never swallowed: a call outside the allowlist
                    // can be the visible edge of a prompt injection, so it has
                    // to stay findable in logs and traces even though the run
                    // continues.
                    tracing::warn!(
                        target: "pocopine.log",
                        %reason,
                        "tool call refused by policy"
                    );
                    observer.tool_blocked(call, &reason);
                    out.push(Message::tool_result(
                        call.id.clone(),
                        serde_json::json!({
                            "error": reason,
                            "hint": "This tool is not available in this conversation. \
                                     Continue without it.",
                        })
                        .to_string(),
                    ));
                    continue;
                }
            }
        }
        // Both checks passed above, so the registry has it.
        let Some(tool) = inner.tools.get(&call.tool_id) else {
            continue;
        };
        let step = observer.tool_started(call);

        // `before_tool_call` hook (L3): block (approval/trust gate) or replace the
        // arguments before running. Receives the full [`ToolCall`] so a policy
        // layer can correlate its decision with the invocation id (the same id
        // the `ToolStarted`/`ToolBlocked` events carry), and is async so an
        // approval gate can await a human decision. Absent on the typed run
        // (`None`).
        let decision = match before_tool_call {
            Some(hook) => hook(call).await,
            None => ToolDecision::Proceed,
        };

        let result_text = match decision {
            ToolDecision::Block { reason } => {
                observer.tool_blocked(call, &reason);
                serde_json::json!({ "blocked": reason }).to_string()
            }
            decision => {
                let args = match decision {
                    ToolDecision::ReplaceArgs { args } => args,
                    _ => call.args.clone(),
                };
                match tool.call_json(args, ctx.clone()).await {
                    Ok(output) => {
                        observer.tool_completed(step, call, &output);
                        serde_json::to_string(&output).map_err(|e| {
                            AgenkitError::internal(format!(
                                "tool `{}` output encode: {e}",
                                call.tool_id
                            ))
                        })?
                    }
                    Err(error) => match error_mode {
                        ToolErrorMode::Propagate => return Err(error),
                        ToolErrorMode::FeedBack => {
                            let kind = error.to_string();
                            observer.tool_failed(call, &kind);
                            serde_json::json!({ "error": kind }).to_string()
                        }
                    },
                }
            }
        };
        out.push(Message::tool_result(call.id.clone(), result_text));
    }
    Ok(out)
}

/// A [`LoopObserver`] that emits the `pocopine.trace` step events for one agent
/// step — the model request/response (with token usage) and tool start/complete
/// spans, all parented to the enclosing agent step. Shared by the typed run and
/// (wrapped) the runtime, so trace emission lives in exactly one place.
pub(crate) struct TraceObserver<'a> {
    pub(crate) run: &'a Arc<RunState>,
    pub(crate) agent_step: StepId,
}

impl LoopObserver for TraceObserver<'_> {
    fn model_request(&self, model: &ModelRef) -> Option<StepId> {
        let step = self.run.next_step_id();
        self.run.emit(
            self.run
                .event(
                    events::AI_MODEL_REQUEST,
                    StepKind::Generation,
                    StepStatus::Started,
                )
                .with_step(step.clone())
                .with_parent(self.agent_step.clone())
                .with_model(model.clone()),
        );
        Some(step)
    }

    fn model_response(&self, step: Option<StepId>, model: &ModelRef, usage: Option<Usage>) {
        let mut completed = self
            .run
            .event(
                events::AI_MODEL_RESPONSE,
                StepKind::Generation,
                StepStatus::Completed,
            )
            .with_parent(self.agent_step.clone())
            .with_model(model.clone());
        if let Some(step) = step {
            completed = completed.with_step(step);
        }
        if let Some(usage) = usage {
            completed = completed.with_usage(usage);
            // Resolve the catalog-derived cost (W2) so agent/conversational turns
            // are *metered*, not just token-counted — parity with the `Ai` path.
            if let Some(meta) = super::catalog::lookup(model) {
                completed = completed.with_cost(meta.estimate_cost(&usage));
            }
        }
        self.run.emit(completed);
    }

    fn tool_started(&self, call: &ToolCall) -> Option<StepId> {
        let step = self.run.next_step_id();
        self.run.emit(
            self.run
                .event(events::AI_TOOL_STARTED, StepKind::Tool, StepStatus::Started)
                .with_step(step.clone())
                .with_parent(self.agent_step.clone())
                .with_field("tool_id", call.tool_id.clone()),
        );
        Some(step)
    }

    fn tool_completed(&self, step: Option<StepId>, call: &ToolCall, _output: &serde_json::Value) {
        let mut completed = self
            .run
            .event(
                events::AI_TOOL_COMPLETED,
                StepKind::Tool,
                StepStatus::Completed,
            )
            .with_parent(self.agent_step.clone())
            .with_field("tool_id", call.tool_id.clone());
        if let Some(step) = step {
            completed = completed.with_step(step);
        }
        self.run.emit(completed);
    }
}
