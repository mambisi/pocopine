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
    /// The model produced reasoning ("thinking") content (roadmap W5).
    ///
    /// `chars` is always a redaction-safe count, so a client can show a
    /// "thinking…" indicator and roughly how much the model reasoned
    /// (e.g. ChatGPT's "Thought for a moment"). `text` carries the reasoning
    /// itself and is present **only when the flow author opts in**
    /// (`Flow::expose_reasoning` / `#[ai_flow(reasoning)]`); otherwise the
    /// boundary strips it to `None` so reasoning stays server-side (§D10).
    ThinkingDelta {
        /// Reasoning characters in this block (always present).
        chars: u32,
        /// The reasoning text — present only for a flow that opted into exposing
        /// it; `None` keeps the reasoning server-side (the redacted default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// An optional aggregate usage update (token counts only — client-safe).
    UsageUpdate {
        /// Aggregate uncached input tokens so far.
        input_tokens: u64,
        /// Aggregate output tokens so far.
        output_tokens: u64,
        /// Aggregate cached input tokens read so far.
        #[serde(default)]
        cache_read_tokens: u64,
        /// Aggregate cache-creation tokens so far.
        #[serde(default)]
        cache_creation_tokens: u64,
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

/// The public, client-safe projection of a conversational agent turn — the
/// session analogue of [`FlowStreamEvent`]. Produced by
/// `pocopine_agenkit::server::AgentEventRedactor`, the stateful adapter that
/// maps the trusted host-side `AgentEvent` firehose through a
/// [`Redactor`](crate::redact::Redactor) — classifying the *assembled* delta
/// text so a fragment-split secret can't stream out — so every UI bridge
/// (SSE, websocket, CLI) shares one redaction-by-construction path instead of
/// hand-rolling its own trusted→wire mapping.
///
/// Tool `args`/`output` ride the wire *redacted* (size-capped, sensitive keys
/// blanked, classifier-checked) — present because a chat harness renders them;
/// a bridge that wants them off the wire entirely can drop the fields in its
/// own mapping.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentWireEvent {
    /// A prompt was accepted; the turn begins.
    Started,
    /// An incremental fragment of the assistant's answer (a live view — the
    /// assembled message follows as [`AssistantText`](AgentWireEvent::AssistantText)).
    AssistantDelta {
        /// The next fragment of the assistant's text.
        text: String,
    },
    /// An assistant text response (assembled; may precede tool calls).
    AssistantText {
        /// The assistant's text.
        text: String,
    },
    /// A tool call is about to run.
    ToolStarted {
        /// The provider's call id.
        id: String,
        /// The tool's registry id.
        tool: String,
        /// The call arguments (redacted).
        args: serde_json::Value,
    },
    /// A tool call returned.
    ToolCompleted {
        /// The provider's call id.
        id: String,
        /// The tool's registry id.
        tool: String,
        /// The tool's output (redacted).
        output: serde_json::Value,
    },
    /// A tool call failed (the loop continues).
    ToolFailed {
        /// The provider's call id.
        id: String,
        /// The tool's registry id.
        tool: String,
        /// The error text (redacted).
        error: String,
    },
    /// A hook blocked a tool call.
    ToolBlocked {
        /// The provider's call id.
        id: String,
        /// The tool's registry id.
        tool: String,
        /// Why it was blocked (redacted).
        reason: String,
    },
    /// The thread was compacted.
    Compacted {
        /// How many messages were folded into the summary.
        folded: u64,
    },
    /// Terminal: the turn finished successfully.
    Stopped {
        /// Why the turn ended.
        reason: WireStopReason,
    },
    /// Terminal: the turn failed.
    Failed {
        /// The error text (redacted).
        error: String,
    },
    /// Forward-compatibility fallback: an event tag this build does not know.
    /// Never constructed by the server; produced on the decode side so an
    /// older client skips a newer server's events instead of failing.
    #[serde(other)]
    Unknown,
}

/// Why a turn ended, on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WireStopReason {
    /// The model answered with no tool calls — waiting for the next prompt.
    Idle,
    /// The per-turn step budget was hit.
    MaxSteps,
    /// The turn was cancelled.
    Aborted,
    /// Forward-compatibility fallback for a reason this build does not know.
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
    fn thinking_delta_redacted_form_is_count_only() {
        // The default (non-exposed) form carries only the count — `text` is
        // skipped, so the serialized event cannot hold reasoning (§D10).
        let event = FlowStreamEvent::ThinkingDelta {
            chars: 16,
            text: None,
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(value["event"], "thinking_delta");
        assert_eq!(value["chars"], 16);
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["chars", "event"]);
        assert_eq!(
            serde_json::from_value::<FlowStreamEvent>(value).unwrap(),
            event
        );
    }

    #[test]
    fn thinking_delta_exposed_form_round_trips_with_text() {
        // When a flow opts in, the reasoning text rides the event.
        let event = FlowStreamEvent::ThinkingDelta {
            chars: 16,
            text: Some("let me reason".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"thinking_delta""#));
        assert!(json.contains(r#""text":"let me reason""#));
        assert_eq!(
            serde_json::from_str::<FlowStreamEvent>(&json).unwrap(),
            event
        );
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

    #[test]
    fn agent_wire_events_are_tagged_and_degrade_to_unknown() {
        let delta = AgentWireEvent::AssistantDelta {
            text: "hel".to_string(),
        };
        let json = serde_json::to_string(&delta).unwrap();
        assert!(json.contains(r#""event":"assistant_delta""#));
        assert_eq!(
            serde_json::from_str::<AgentWireEvent>(&json).unwrap(),
            delta
        );

        let stopped = AgentWireEvent::Stopped {
            reason: WireStopReason::MaxSteps,
        };
        let json = serde_json::to_string(&stopped).unwrap();
        assert!(json.contains(r#""reason":"max_steps""#));

        // An old client reading a newer server's stream skips, not fails.
        let event: AgentWireEvent =
            serde_json::from_str(r#"{"event":"added_in_a_future_version"}"#).unwrap();
        assert_eq!(event, AgentWireEvent::Unknown);
        let reason: WireStopReason = serde_json::from_str(r#""new_reason""#).unwrap();
        assert_eq!(reason, WireStopReason::Unknown);
    }
}
