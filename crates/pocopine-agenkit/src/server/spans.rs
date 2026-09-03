//! RFC-123 §4 — the `pocopine.ai.*` spans, opened **beside** (never instead
//! of) the `TraceEvent` stream. Spans carry topology and timing plus a few
//! structural fields; the stream stays the stable public schema and the
//! metering input. `pocopine.ai.step_id` is the join key between the two.
//!
//! Field names are spelled inline (`tracing` takes identifiers) and match
//! `pocopine_observe::fields`. Nothing here ever records a prompt, output,
//! tool argument, or error message — classifications and ids only.

use pocopine_agenkit_core::{
    AgenkitError, ModelRef, ParallelGroupId, RunId, StepId, TraceId, Usage,
};
use pocopine_observe::{TRACE_TARGET, fields, spans};
use tracing::Span;
use tracing::field::Empty;

/// `pocopine.ai.run` — one flow invocation.
pub(crate) fn run_span(flow: &str, run_id: &RunId, trace_id: &TraceId) -> Span {
    tracing::info_span!(
        target: TRACE_TARGET,
        spans::AI_RUN,
        otel.kind = "internal",
        pocopine.ai.flow = flow,
        pocopine.ai.run_id = run_id.as_str(),
        pocopine.ai.trace_id = trace_id.as_str(),
        otel.status_code = Empty,
        error.type = Empty,
    )
}

/// `pocopine.ai.turn` — one conversational-runtime turn.
pub(crate) fn turn_span(run_id: &RunId, trace_id: &TraceId, model: &ModelRef) -> Span {
    tracing::info_span!(
        target: TRACE_TARGET,
        spans::AI_TURN,
        otel.kind = "internal",
        pocopine.ai.run_id = run_id.as_str(),
        pocopine.ai.trace_id = trace_id.as_str(),
        gen_ai.request.model = model.as_str(),
        gen_ai.usage.input_tokens = Empty,
        gen_ai.usage.output_tokens = Empty,
        otel.status_code = Empty,
        error.type = Empty,
    )
}

/// `pocopine.ai.step` — custom / agent / reducer / retrieval step.
pub(crate) fn step_span(kind: &'static str, step_id: &StepId, name: &str) -> Span {
    tracing::info_span!(
        target: TRACE_TARGET,
        spans::AI_STEP,
        otel.kind = "internal",
        pocopine.ai.step_id = step_id.as_str(),
        pocopine.ai.step_kind = kind,
        pocopine.ai.step_name = name,
        pocopine.ai.parallel_group_id = Empty,
        otel.status_code = Empty,
        error.type = Empty,
    )
}

/// `pocopine.ai.step` for a parallel group or one of its branches.
pub(crate) fn step_span_in_group(
    kind: &'static str,
    step_id: &StepId,
    name: &str,
    group: &ParallelGroupId,
) -> Span {
    let span = step_span(kind, step_id, name);
    span.record(fields::AI_PARALLEL_GROUP_ID, group.as_str());
    span
}

/// `pocopine.ai.model` — one model call (`otel.kind = client`).
pub(crate) fn model_span(model: &ModelRef, provider: &str, step_id: Option<&StepId>) -> Span {
    let span = tracing::info_span!(
        target: TRACE_TARGET,
        spans::AI_MODEL,
        otel.kind = "client",
        gen_ai.operation.name = "chat",
        gen_ai.provider.name = provider,
        gen_ai.request.model = model.as_str(),
        pocopine.ai.step_id = Empty,
        gen_ai.usage.input_tokens = Empty,
        gen_ai.usage.output_tokens = Empty,
        otel.status_code = Empty,
        error.type = Empty,
    );
    if let Some(step_id) = step_id {
        span.record(fields::AI_STEP_ID, step_id.as_str());
    }
    span
}

/// `pocopine.ai.tool` — one tool execution inside an agent loop.
pub(crate) fn tool_span(tool_id: &str, step_id: Option<&StepId>) -> Span {
    let span = tracing::info_span!(
        target: TRACE_TARGET,
        spans::AI_TOOL,
        otel.kind = "internal",
        gen_ai.tool.name = tool_id,
        pocopine.ai.step_id = Empty,
        otel.status_code = Empty,
        error.type = Empty,
    );
    if let Some(step_id) = step_id {
        span.record(fields::AI_STEP_ID, step_id.as_str());
    }
    span
}

/// Record token usage on a model or turn span.
pub(crate) fn record_usage(span: &Span, usage: &Usage) {
    span.record(fields::GEN_AI_USAGE_INPUT_TOKENS, usage.input_tokens);
    span.record(fields::GEN_AI_USAGE_OUTPUT_TOKENS, usage.output_tokens);
}

/// Close a span from an outcome: `OK`, or `ERROR` + the stable error kind.
pub(crate) fn close<T>(span: &Span, result: &Result<T, AgenkitError>) {
    match result {
        Ok(_) => {
            span.record(fields::OTEL_STATUS_CODE, "OK");
        }
        Err(error) => {
            span.record(fields::OTEL_STATUS_CODE, "ERROR");
            span.record(fields::ERROR_TYPE, error.kind());
        }
    }
}
