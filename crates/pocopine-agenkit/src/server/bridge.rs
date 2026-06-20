//! Client-safe error boundary for flows (RFC-093 §D9, §D10).
//!
//! Flows are internal logic; an app exposes one through a plain `#[server]`
//! function that runs the flow and maps its error here:
//!
//! ```ignore
//! #[server(public)]
//! pub async fn summarize(input: In) -> ServerResult<Out> {
//!     active_plugin::<Agenkit>().unwrap()
//!         .flow(Summarize).input(input).run().await   // typed marker from #[ai_flow]
//!         .map_err(|e| pocopine_agenkit::server::to_server_error(&e))
//! }
//! ```
//!
//! [`to_server_error`] maps [`AgenkitError`] to `pocopine_core::ServerError`,
//! dropping provider internals so they never cross to the client. The
//! `flow_is_public` / `flow_stream_mode` probes and [`stream_filter`] gate flow
//! streaming, which an app exposes as a streaming `#[server]` fn via
//! `agenkit.flow(..).stream()` (RFC-107).

use pocopine_agenkit_core::{AgenkitError, FlowStreamEvent, StreamMode};
use pocopine_core::ServerError;

use super::agenkit::Agenkit;

/// Map an [`AgenkitError`] to a client-safe [`ServerError`].
///
/// Validation and tool-policy errors carry their (Agenkit-internal) message;
/// everything that could quote provider internals (provider, config, budget,
/// cancellation, reducer) collapses to the error *kind* only (§D10).
pub fn to_server_error(error: &AgenkitError) -> ServerError {
    match error {
        AgenkitError::Validation { message } => {
            ServerError::bad_request(format!("invalid input: {message}"))
        }
        AgenkitError::ToolPolicy { message } => {
            ServerError::forbidden(format!("tool policy: {message}"))
        }
        AgenkitError::NotFound { .. } => ServerError::bad_request("unknown AI flow"),
        AgenkitError::Json { .. } => ServerError::bad_request("invalid AI payload"),
        // The one provider failure a client can act on (compact + retry): the
        // stable kind makes it detectable, while the raw message stays server-side.
        AgenkitError::ContextOverflow { .. } => {
            ServerError::App(format!("ai error ({})", error.kind()))
        }
        // Provider / Config / BudgetExhausted / Cancelled / ReducerDisagreement:
        // never leak internals — surface the stable kind only.
        other => ServerError::App(format!("ai error ({})", other.kind())),
    }
}

impl Agenkit {
    /// Whether a registered flow is public (callable from the client boundary).
    pub fn flow_is_public(&self, id: &str) -> bool {
        self.inner
            .flows
            .get(id)
            .map(|handler| handler.descriptor().public)
            .unwrap_or(false)
    }

    /// The stream-visibility cap the flow author declared via
    /// `Flow::stream_mode` (§D8). Defaults to [`StreamMode::FinalOnly`]: a
    /// flow opts *into* exposing progress events; clients can request less
    /// visibility than the cap, never more.
    pub fn flow_stream_mode(&self, id: &str) -> StreamMode {
        self.inner
            .flows
            .get(id)
            .map(|handler| handler.descriptor().stream_mode)
            .unwrap_or_default()
    }

    /// Whether the flow author opted into streaming reasoning ("thinking") text
    /// to the client via `Flow::expose_reasoning` (§D10). Off by default — the
    /// boundary then strips reasoning text from every stream event.
    pub fn flow_exposes_reasoning(&self, id: &str) -> bool {
        self.inner
            .flows
            .get(id)
            .map(|handler| handler.descriptor().expose_reasoning)
            .unwrap_or(false)
    }
}

/// Drop the reasoning text from a stream event unless the flow opted into
/// exposing it (§D10). This is the per-flow reasoning gate, applied at the wire
/// boundary *before* [`stream_filter`]: a non-exposing flow's `ThinkingDelta`
/// keeps only its redacted character count; everything else passes untouched.
pub fn redact_reasoning(event: FlowStreamEvent, expose: bool) -> FlowStreamEvent {
    match event {
        FlowStreamEvent::ThinkingDelta {
            chars,
            text: Some(_),
        } if !expose => FlowStreamEvent::ThinkingDelta { chars, text: None },
        other => other,
    }
}

/// The flow-stream redaction chokepoint (§D8/§D10).
///
/// Every [`FlowStreamEvent`] variant is client-safe by construction — it can
/// only carry ids, kinds, counts, error *kinds*, and user-visible output, never
/// raw prompts, tool args, hidden reasoning, or provider payloads. This gate
/// additionally enforces the [`StreamMode`] visibility contract: it is the one
/// place that decides what crosses to the wire. `stream` applies it
/// to every event before it reaches the SSE frame.
pub fn stream_filter(event: &FlowStreamEvent, mode: StreamMode) -> bool {
    use FlowStreamEvent::*;
    enum Visibility {
        /// Run lifecycle: crosses in every mode.
        Lifecycle,
        /// User-visible output: crosses in every mode (FinalOnly would stream
        /// nothing useful without the final result).
        Output,
        /// The redacted step/tool/parallel/reducer tree: Progress+ only.
        Progress,
    }
    // Classify every variant EXPLICITLY: a new FlowStreamEvent variant must
    // not compile until someone decides its wire visibility here (§D10).
    let visibility = match event {
        FlowStarted { .. }
        | FlowCompleted { .. }
        | FlowFailed { .. }
        | Error { .. }
        | OutputCompleted => Visibility::Lifecycle,
        OutputDelta { .. } | ObjectDelta { .. } => Visibility::Output,
        // Reasoning the author chose to expose (text survived `redact_reasoning`)
        // is intentional user-facing content — it rides with the output deltas.
        ThinkingDelta { text: Some(_), .. } => Visibility::Output,
        StepStarted { .. }
        | StepProgress { .. }
        | StepCompleted { .. }
        | StepFailed { .. }
        | ToolStarted { .. }
        | ToolCompleted { .. }
        | ToolFailed { .. }
        | ParallelStarted { .. }
        | BranchStarted { .. }
        | BranchCompleted { .. }
        | BranchFailed { .. }
        | BranchCancelled { .. }
        | ParallelCompleted { .. }
        | ReducerStarted { .. }
        | ReducerDecision { .. }
        | ReducerCompleted { .. }
        // A redacted reasoning *count* (text stripped, or never exposed): exposing
        // even that a model reasoned, and how much, is opt-in progress metadata.
        | ThinkingDelta { text: None, .. }
        | UsageUpdate { .. } => Visibility::Progress,
        // The decode-side fallback; the server never constructs it. Drop
        // defensively rather than forwarding an empty shell.
        Unknown => return false,
    };
    match mode {
        StreamMode::FinalOnly | StreamMode::OutputDeltas => {
            !matches!(visibility, Visibility::Progress)
        }
        StreamMode::Progress | StreamMode::DebugSafe => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_errors_do_not_leak_details() {
        let error = AgenkitError::provider("503 from secret-host:8443 with bearer abc");
        let mapped = to_server_error(&error);
        let rendered = mapped.to_string();
        assert!(rendered.contains("ai error (provider)"));
        assert!(!rendered.contains("secret-host"));
        assert!(!rendered.contains("bearer"));
    }

    #[test]
    fn validation_maps_to_bad_request() {
        let mapped = to_server_error(&AgenkitError::validation("missing field `q`"));
        assert!(matches!(mapped, ServerError::BadRequest(_)));
    }

    #[test]
    fn tool_policy_maps_to_forbidden() {
        let mapped = to_server_error(&AgenkitError::tool_policy("not allowlisted"));
        assert!(matches!(mapped, ServerError::Forbidden(_)));
    }

    #[test]
    fn context_overflow_surfaces_a_stable_detectable_kind() {
        // The raw provider message stays server-side; the client sees a stable
        // "context_overflow" kind it can detect to trigger compaction.
        let mapped = to_server_error(&AgenkitError::context_overflow(
            "prompt is too long: 250000 > 200000",
        ));
        let rendered = mapped.to_string();
        assert!(rendered.contains("context_overflow"));
        assert!(!rendered.contains("250000"));
    }

    use pocopine_agenkit_core::{ParallelGroupId, StepId};

    fn lifecycle_event() -> FlowStreamEvent {
        FlowStreamEvent::FlowCompleted {
            run_id: "run-0".to_string(),
        }
    }

    fn progress_event() -> FlowStreamEvent {
        FlowStreamEvent::BranchStarted {
            group_id: ParallelGroupId::new("g"),
            step_id: StepId::new("s1"),
            name: "b0".to_string(),
        }
    }

    #[test]
    fn final_only_drops_progress_keeps_lifecycle() {
        assert!(stream_filter(&lifecycle_event(), StreamMode::FinalOnly));
        assert!(!stream_filter(&progress_event(), StreamMode::FinalOnly));
    }

    #[test]
    fn output_deltas_passes_output_not_progress() {
        let delta = FlowStreamEvent::OutputDelta {
            text: "hi".to_string(),
        };
        assert!(stream_filter(&delta, StreamMode::OutputDeltas));
        assert!(!stream_filter(&progress_event(), StreamMode::OutputDeltas));
    }

    #[test]
    fn progress_passes_the_tree() {
        assert!(stream_filter(&progress_event(), StreamMode::Progress));
        assert!(stream_filter(&progress_event(), StreamMode::DebugSafe));
    }

    #[test]
    fn redacted_thinking_count_is_progress_gated() {
        // The redacted reasoning signal (count only, text stripped) is progress
        // metadata: it crosses only under Progress+ visibility (§D10).
        let thinking = FlowStreamEvent::ThinkingDelta {
            chars: 16,
            text: None,
        };
        assert!(!stream_filter(&thinking, StreamMode::FinalOnly));
        assert!(!stream_filter(&thinking, StreamMode::OutputDeltas));
        assert!(stream_filter(&thinking, StreamMode::Progress));
        assert!(stream_filter(&thinking, StreamMode::DebugSafe));
    }

    #[test]
    fn exposed_thinking_text_rides_with_output() {
        // Reasoning an author chose to expose is user-facing content: it crosses
        // wherever output deltas do (OutputDeltas+), not just under Progress.
        let thinking = FlowStreamEvent::ThinkingDelta {
            chars: 16,
            text: Some("reasoning".to_string()),
        };
        assert!(stream_filter(&thinking, StreamMode::OutputDeltas));
        assert!(stream_filter(&thinking, StreamMode::Progress));
    }

    #[test]
    fn redact_reasoning_strips_text_unless_exposed() {
        let with_text = || FlowStreamEvent::ThinkingDelta {
            chars: 9,
            text: Some("reasoning".to_string()),
        };
        // A non-exposing flow loses the text but keeps the count.
        assert_eq!(
            redact_reasoning(with_text(), false),
            FlowStreamEvent::ThinkingDelta {
                chars: 9,
                text: None,
            }
        );
        // An exposing flow keeps the text verbatim.
        assert_eq!(redact_reasoning(with_text(), true), with_text());
        // Non-reasoning events pass through untouched regardless.
        let output = FlowStreamEvent::OutputDelta {
            text: "hi".to_string(),
        };
        assert_eq!(redact_reasoning(output.clone(), false), output);
    }

    #[test]
    fn unknown_never_crosses_the_wire() {
        assert!(!stream_filter(
            &FlowStreamEvent::Unknown,
            StreamMode::DebugSafe
        ));
        assert!(!stream_filter(
            &FlowStreamEvent::Unknown,
            StreamMode::FinalOnly
        ));
    }
}
