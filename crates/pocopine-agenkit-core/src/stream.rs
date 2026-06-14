//! Public, client-safe stream events (RFC-093 Phase 1.4, §D8).
//!
//! [`FlowStreamEvent`]s are the *only* shapes a browser client receives for a
//! streaming flow. By construction they carry redacted, allowlisted data —
//! ids, kinds, counts, durations, and user-visible output deltas — never raw
//! model thoughts, reasoning tokens, prompts, tool arguments, or provider
//! payloads. The facade applies a final redaction chokepoint before the wire
//! (§D10), but these types only model client-safe fields to begin with.

use crate::{ParallelGroupId, ParallelJoin, StepId, StepKind};

/// A single public progress event for a streaming flow.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum FlowStreamEvent {
    /// The flow began.
    FlowStarted {
        /// The run id.
        run_id: String,
        /// The trace id for local correlation.
        trace_id: String,
    },
    /// The flow finished successfully.
    FlowCompleted {
        /// The run id.
        run_id: String,
    },
    /// The flow failed (public error kind only, never provider internals).
    FlowFailed {
        /// The run id.
        run_id: String,
        /// The public error kind (`AgenkitError::kind`).
        error_kind: String,
        /// The trace id for local correlation.
        trace_id: String,
    },
    /// A step began.
    StepStarted {
        /// The step id.
        step_id: StepId,
        /// The kind of work.
        kind: StepKind,
        /// A safe step name.
        name: String,
        /// The parent step, if nested.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_step_id: Option<StepId>,
    },
    /// A redacted progress note for a step.
    StepProgress {
        /// The step id.
        step_id: StepId,
        /// A safe, human-readable progress message.
        message: String,
    },
    /// A step completed.
    StepCompleted {
        /// The step id.
        step_id: StepId,
    },
    /// A step failed (public error kind only).
    StepFailed {
        /// The step id.
        step_id: StepId,
        /// The public error kind.
        error_kind: String,
    },
    /// A user-visible output fragment.
    OutputDelta {
        /// The output text fragment (user-visible by definition).
        text: String,
    },
    /// An incremental partial structured-output object: a best-effort parse of
    /// the JSON generated so far, so clients can render a progressively
    /// completing object (the structured analogue of `OutputDelta`).
    ObjectDelta {
        /// The partial output object parsed from the stream so far.
        partial: serde_json::Value,
    },
    /// The user-visible output is complete.
    OutputCompleted,
    /// A tool invocation began (no raw args).
    ToolStarted {
        /// The step id.
        step_id: StepId,
        /// The tool id.
        tool_id: String,
    },
    /// A tool invocation completed (no raw result).
    ToolCompleted {
        /// The step id.
        step_id: StepId,
        /// The tool id.
        tool_id: String,
    },
    /// A tool invocation failed (public error kind only).
    ToolFailed {
        /// The step id.
        step_id: StepId,
        /// The tool id.
        tool_id: String,
        /// The public error kind.
        error_kind: String,
    },
    /// A parallel fan-out group began.
    ParallelStarted {
        /// The parallel group id.
        group_id: ParallelGroupId,
        /// How many branches were launched.
        branch_count: u32,
        /// The join policy.
        join: ParallelJoin,
    },
    /// A branch of a parallel group began.
    BranchStarted {
        /// The parallel group id.
        group_id: ParallelGroupId,
        /// The branch's step id.
        step_id: StepId,
        /// A safe branch name.
        name: String,
    },
    /// A branch completed.
    BranchCompleted {
        /// The parallel group id.
        group_id: ParallelGroupId,
        /// The branch's step id.
        step_id: StepId,
    },
    /// A branch failed (public error kind only).
    BranchFailed {
        /// The parallel group id.
        group_id: ParallelGroupId,
        /// The branch's step id.
        step_id: StepId,
        /// The public error kind.
        error_kind: String,
    },
    /// A branch was cancelled in flight (a losing branch aborted by an
    /// early-exit join). Terminal, so a client can close a branch that started
    /// but never completed or failed.
    BranchCancelled {
        /// The parallel group id.
        group_id: ParallelGroupId,
        /// The branch's step id.
        step_id: StepId,
    },
    /// A parallel group finished.
    ParallelCompleted {
        /// The parallel group id.
        group_id: ParallelGroupId,
        /// How many branches succeeded.
        success_count: u32,
    },
    /// A reducer began.
    ReducerStarted {
        /// The reducer step id.
        step_id: StepId,
    },
    /// A reducer reached a decision (metadata only).
    ReducerDecision {
        /// The reducer step id.
        step_id: StepId,
        /// Whether a final answer was accepted.
        accepted: bool,
    },
    /// A reducer completed.
    ReducerCompleted {
        /// The reducer step id.
        step_id: StepId,
    },
    /// An optional aggregate usage update.
    UsageUpdate {
        /// Aggregate input tokens so far.
        input_tokens: u64,
        /// Aggregate output tokens so far.
        output_tokens: u64,
    },
    /// A public error (kind + trace id, never provider internals).
    Error {
        /// The public error kind.
        error_kind: String,
        /// The trace id for local correlation.
        trace_id: String,
    },
    /// Forward-compatibility fallback: an event tag this build does not know.
    /// Never constructed by the server; produced on the decode side (e.g. a
    /// cached wasm client reading a newer server's stream) so unknown events
    /// are skipped instead of failing the whole stream.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_tagged_and_round_trip() {
        let started = FlowStreamEvent::ParallelStarted {
            group_id: ParallelGroupId::new("candidate_answers"),
            branch_count: 3,
            join: ParallelJoin::AllSettled,
        };
        let json = serde_json::to_string(&started).unwrap();
        assert!(json.contains(r#""event":"parallel_started""#));
        assert_eq!(
            serde_json::from_str::<FlowStreamEvent>(&json).unwrap(),
            started
        );

        let delta = FlowStreamEvent::OutputDelta {
            text: "Uploads use ".to_string(),
        };
        let json = serde_json::to_string(&delta).unwrap();
        assert!(json.contains(r#""event":"output_delta""#));
    }

    #[test]
    fn unit_variant_serializes_as_tag_only() {
        let json = serde_json::to_string(&FlowStreamEvent::OutputCompleted).unwrap();
        assert_eq!(json, r#"{"event":"output_completed"}"#);
    }

    #[test]
    fn unknown_event_tag_decodes_to_unknown_instead_of_failing() {
        // A stale client must skip events a newer server added, not error the
        // whole stream mid-flight.
        let event: FlowStreamEvent =
            serde_json::from_str(r#"{"event":"added_in_a_future_version"}"#).unwrap();
        assert_eq!(event, FlowStreamEvent::Unknown);
    }
}
