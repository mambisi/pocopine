//! The public AI-flow streaming route (RFC-093 Phase 3.3, §D8, §D10).
//!
//! An SSE route (matching `pocopine-server`/`pocopine-live`, axum 0.7) that
//! runs a flow streaming and forwards [`FlowStreamEvent`]s to the client. Every
//! event passes through [`stream_filter`] — the single redaction chokepoint —
//! as the last transform before the wire. The flow runs under the caller
//! [`Principal`] read from request extensions (§D15 DC-5).

use std::convert::Infallible;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use futures::StreamExt;
use pocopine_agenkit_core::{FlowStreamEvent, StreamMode};
use pocopine_auth::Principal;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::agenkit::{Agenkit, with_principal};

/// The route path; `:id` is the flow id.
pub const AI_FLOW_STREAM_PATH: &str = "/__pocopine/agenkit/v1/flow/:id/stream";

/// The redaction chokepoint (§D8/§D10).
///
/// Every [`FlowStreamEvent`] variant is client-safe by construction — it can
/// only carry ids, kinds, counts, error *kinds*, and user-visible output, never
/// raw prompts, tool args, hidden reasoning, or provider payloads. This gate
/// additionally enforces the [`StreamMode`] visibility contract: it is the one
/// place that decides what crosses to the wire.
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
        | ParallelCompleted { .. }
        | ReducerStarted { .. }
        | ReducerDecision { .. }
        | ReducerCompleted { .. }
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

/// Visibility rank for clamping; higher exposes strictly more.
fn mode_rank(mode: StreamMode) -> u8 {
    match mode {
        StreamMode::FinalOnly => 0,
        StreamMode::OutputDeltas => 1,
        StreamMode::Progress => 2,
        StreamMode::DebugSafe => 3,
    }
}

/// Clamp the client-requested mode to the author-declared cap (§D8): the
/// query string can lower visibility, never raise it above `Flow::stream_mode`.
fn clamp_mode(requested: StreamMode, declared: StreamMode) -> StreamMode {
    if mode_rank(requested) <= mode_rank(declared) {
        requested
    } else {
        declared
    }
}

fn parse_mode(raw: Option<&str>) -> StreamMode {
    match raw {
        Some("final") => StreamMode::FinalOnly,
        Some("output") => StreamMode::OutputDeltas,
        Some("debug") => StreamMode::DebugSafe,
        // A streaming subscription defaults to redacted progress.
        _ => StreamMode::Progress,
    }
}

fn to_sse(event: &FlowStreamEvent) -> SseEvent {
    // The JSON carries the tagged `event` discriminant; clients dispatch on it.
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    SseEvent::default().data(data)
}

/// Query string for the stream route.
#[derive(serde::Deserialize)]
struct StreamQuery {
    mode: Option<String>,
}

async fn stream_handler(
    State(agenkit): State<Agenkit>,
    Path(id): Path<String>,
    Query(query): Query<StreamQuery>,
    principal: Option<Extension<Principal>>,
    Json(input): Json<serde_json::Value>,
) -> Response {
    // Same authorization boundary as the non-streaming bridge: only public
    // flows are reachable, and a private flow is indistinguishable from an
    // unknown one (§D9/§D10).
    if !agenkit.flow_is_public(&id) {
        return (StatusCode::NOT_FOUND, "unknown AI flow").into_response();
    }

    // The client can request *less* visibility than the flow author declared,
    // never more (§D8).
    let mode = clamp_mode(
        parse_mode(query.mode.as_deref()),
        agenkit.flow_stream_mode(&id),
    );
    let principal = principal
        .map(|ext| ext.0)
        .unwrap_or_else(Principal::anonymous);

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    // Run the flow under the caller principal; the task-local seam lets
    // `run_flow_streaming` pick it up (§D15 DC-5). The run emits the final
    // result (OutputDelta/OutputCompleted) before FlowCompleted, and FlowFailed
    // on error — so the handler only forwards the channel.
    tokio::spawn(with_principal(principal, async move {
        let _ = agenkit.run_flow_streaming(&id, input, tx).await;
    }));

    let stream = UnboundedReceiverStream::new(rx).filter_map(move |event| async move {
        stream_filter(&event, mode).then(|| Ok::<SseEvent, Infallible>(to_sse(&event)))
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Build the AI-flow streaming router. Mount it alongside the app's other
/// routes (and `Server::with_auth(...)` so the principal is populated).
pub fn ai_flow_stream_router(agenkit: Agenkit) -> Router {
    Router::new()
        .route(AI_FLOW_STREAM_PATH, post(stream_handler))
        .with_state(agenkit)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn requested_mode_is_clamped_to_the_declared_cap() {
        // A client cannot escalate above the author's declared visibility...
        assert_eq!(
            clamp_mode(StreamMode::DebugSafe, StreamMode::FinalOnly),
            StreamMode::FinalOnly
        );
        assert_eq!(
            clamp_mode(StreamMode::Progress, StreamMode::OutputDeltas),
            StreamMode::OutputDeltas
        );
        // ...but can always request less.
        assert_eq!(
            clamp_mode(StreamMode::FinalOnly, StreamMode::DebugSafe),
            StreamMode::FinalOnly
        );
    }
}
