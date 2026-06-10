//! Per-invocation run state and the tracer (RFC-093 Phase 2.5).
//!
//! A [`RunState`] carries the `run_id`/`trace_id` shared by every event in one
//! flow invocation, a monotonic step-id counter, and the handle back to the
//! runtime. It is the single source the flow context, generation builder, and
//! (later) parallel/agent execution use to emit trace events.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pocopine_agenkit_core::{
    FlowStreamEvent, RunId, StepId, StepKind, StepStatus, TraceEvent, TraceId,
};
use pocopine_auth::Principal;
use tokio::sync::mpsc::UnboundedSender;

use super::agenkit::AgenkitInner;
use super::observe::emit_trace_event;

/// Shared state for one flow/agent invocation.
pub(crate) struct RunState {
    pub(crate) inner: Arc<AgenkitInner>,
    pub(crate) run_id: RunId,
    pub(crate) trace_id: TraceId,
    /// The caller principal this run executes under (anonymous by default).
    pub(crate) principal: Principal,
    /// Public client-stream sink, present only for streaming runs. The Phase
    /// 3.3 route's redaction chokepoint sits between this and the wire.
    stream: Option<UnboundedSender<FlowStreamEvent>>,
    /// Set once the flow has streamed user-visible output deltas, so the
    /// runtime does not re-emit the final result as a duplicate delta.
    output_streamed: AtomicBool,
    step_seq: AtomicU64,
}

impl RunState {
    /// Start a run.
    pub(crate) fn new(
        inner: Arc<AgenkitInner>,
        run_id: RunId,
        trace_id: TraceId,
        principal: Principal,
        stream: Option<UnboundedSender<FlowStreamEvent>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner,
            run_id,
            trace_id,
            principal,
            stream,
            output_streamed: AtomicBool::new(false),
            step_seq: AtomicU64::new(0),
        })
    }

    /// Mark that user-visible output has been streamed as deltas.
    pub(crate) fn mark_output_streamed(&self) {
        self.output_streamed.store(true, Ordering::Relaxed);
    }

    /// Whether the flow already streamed its output as deltas.
    pub(crate) fn output_was_streamed(&self) -> bool {
        self.output_streamed.load(Ordering::Relaxed)
    }

    /// Allocate the next unique step id within this run.
    pub(crate) fn next_step_id(&self) -> StepId {
        StepId::new(format!(
            "s{}",
            self.step_seq.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Build a trace event stamped with this run's correlation ids.
    pub(crate) fn event(&self, name: &str, kind: StepKind, status: StepStatus) -> TraceEvent {
        TraceEvent::new(
            name.to_string(),
            self.run_id.clone(),
            self.trace_id.clone(),
            kind,
            status,
        )
    }

    /// Map + emit a trace event through the observe layer.
    pub(crate) fn emit(&self, event: TraceEvent) {
        emit_trace_event(&event);
    }

    /// Emit a public, client-safe stream event, if this run is streaming.
    /// These shapes are client-safe by construction (§D8); the Phase 3.3 route
    /// applies the final redaction chokepoint before the wire.
    pub(crate) fn stream(&self, event: FlowStreamEvent) {
        if let Some(sink) = &self.stream {
            // A closed receiver just means the client disconnected; ignore.
            let _ = sink.send(event);
        }
    }
}
