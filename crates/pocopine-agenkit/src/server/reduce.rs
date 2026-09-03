//! Reducers / fan-in checkers (RFC-093 Phase 2.6, §D7).
//!
//! `ctx.reduce(name, candidates)` combines parallel branch outputs into one
//! typed result, either via a model judge (`.schema::<O>()`) or a
//! deterministic Rust fold (`.fold(...)`). Each records a [`ReducerDecision`]
//! and emits `ai_reducer_*` trace events.

use pocopine_agenkit_core::{
    AgenkitResult, FlowStreamEvent, ReducerDecision, ReducerKind, StepId, StepKind, StepStatus,
    events,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::Instrument as _;

use super::flow::AiFlowContext;

/// Builder returned by `ctx.reduce(...)`.
pub struct ReduceBuilder<'a, C> {
    ctx: &'a AiFlowContext,
    name: String,
    candidates: Vec<C>,
    system: Option<String>,
}

impl<'a, C> ReduceBuilder<'a, C> {
    pub(crate) fn new(ctx: &'a AiFlowContext, name: impl Into<String>, candidates: Vec<C>) -> Self {
        Self {
            ctx,
            name: name.into(),
            candidates,
            system: None,
        }
    }

    /// Set the judge's system prompt (model-judge mode).
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Mint the reducer step and its `pocopine.ai.step` span (RFC-123 §4);
    /// the started event is emitted inside the span.
    fn emit_started(&self, kind: ReducerKind) -> (StepId, tracing::Span) {
        let run = self.ctx.run_state();
        let step_id = run.next_step_id();
        let span = super::spans::step_span("reducer", &step_id, &self.name);
        span.in_scope(|| {
            run.emit(
                run.event(
                    events::AI_REDUCER_STARTED,
                    StepKind::Reducer,
                    StepStatus::Started,
                )
                .with_step(step_id.clone())
                .with_field("name", self.name.clone())
                .with_field("reducer_kind", format!("{kind:?}"))
                .with_field("candidates", self.candidates.len() as u64),
            )
        });
        run.stream(FlowStreamEvent::ReducerStarted {
            step_id: step_id.clone(),
        });
        (step_id, span)
    }

    fn emit_decision(&self, step_id: StepId, decision: &ReducerDecision) {
        let run = self.ctx.run_state();
        run.emit(
            run.event(
                events::AI_REDUCER_COMPLETED,
                StepKind::Reducer,
                StepStatus::Completed,
            )
            .with_step(step_id.clone())
            .with_field("name", self.name.clone())
            .with_field("accepted", decision.accepted)
            .with_field("reason", decision.reason.clone()),
        );
        run.stream(FlowStreamEvent::ReducerDecision {
            step_id: step_id.clone(),
            accepted: decision.accepted,
        });
        run.stream(FlowStreamEvent::ReducerCompleted { step_id });
    }
}

impl<C: Serialize> ReduceBuilder<'_, C> {
    /// Reduce via a model judge, validating the merged answer into `O`.
    pub async fn schema<O: DeserializeOwned + schemars::JsonSchema>(self) -> AgenkitResult<O> {
        let (step_id, span) = self.emit_started(ReducerKind::ModelJudge);
        let candidates_json = serde_json::to_string(&self.candidates).unwrap_or_default();
        let prompt = format!(
            "Candidate answers (JSON array):\n{candidates_json}\n\nReturn the single best, \
             evidence-supported final answer."
        );
        let system = self.system.clone().unwrap_or_else(|| {
            "You are a strict judge. Keep only claims supported by evidence.".to_string()
        });
        let result = self
            .ctx
            .ai()
            .system(system)
            .prompt(prompt)
            .schema::<O>()
            .generate_structured()
            .instrument(span.clone())
            .await;
        super::spans::close(&span, &result);

        let decision = match &result {
            Ok(_) => ReducerDecision::accept(ReducerKind::ModelJudge, "judge merged candidates", 0)
                .with_candidates(self.candidates.len() as u32),
            // Use the stable error KIND, not the message: the reason is written
            // to a Public trace field and a provider message could quote
            // host/credential internals (§D10/§D12).
            Err(error) => ReducerDecision::reject(ReducerKind::ModelJudge, error.kind()),
        };
        span.in_scope(|| self.emit_decision(step_id, &decision));
        result
    }

    /// Reduce deterministically with a Rust function.
    pub async fn fold<O, F>(self, reducer: F) -> AgenkitResult<O>
    where
        F: FnOnce(Vec<C>) -> AgenkitResult<O>,
    {
        let (step_id, span) = self.emit_started(ReducerKind::Deterministic);
        // Move candidates out via destructure so the closure can consume them
        // without partially moving `self` (the decision emit needs ctx + name).
        let ReduceBuilder {
            ctx,
            name,
            candidates,
            ..
        } = self;
        let count = candidates.len() as u32;
        let result = span.in_scope(|| reducer(candidates));
        super::spans::close(&span, &result);
        let decision = match &result {
            Ok(_) => ReducerDecision::accept(ReducerKind::Deterministic, "deterministic fold", 0)
                .with_candidates(count),
            Err(error) => ReducerDecision::reject(ReducerKind::Deterministic, error.kind()),
        };
        let run = ctx.run_state();
        span.in_scope(|| {
            run.emit(
                run.event(
                    events::AI_REDUCER_COMPLETED,
                    StepKind::Reducer,
                    StepStatus::Completed,
                )
                .with_step(step_id.clone())
                .with_field("name", name)
                .with_field("accepted", decision.accepted)
                .with_field("reason", decision.reason),
            )
        });
        run.stream(FlowStreamEvent::ReducerDecision {
            step_id: step_id.clone(),
            accepted: decision.accepted,
        });
        run.stream(FlowStreamEvent::ReducerCompleted { step_id });
        result
    }
}
