#![cfg(not(target_arch = "wasm32"))]
//! `stream` (RFC-107): a flow is exposed as a streaming `#[server]`
//! fn returning a redacted `StreamServerResult<FlowStreamEvent>`. The bespoke
//! `ai_flow_stream_router` retired — the SSE transport is now the framework's
//! (`fetch::call_stream` + `sse_stream_response`), and this terminal applies the
//! `stream_filter` redaction chokepoint before events leave the runtime.

mod common;

use common::block_on;
use futures::StreamExt;
use pocopine_agenkit::server::{Agenkit, AiFlowContext, Flow, MockProvider};
use pocopine_agenkit_core::{AgenkitResult, FlowStreamEvent, ModelRef, StreamMode};

const ANSWER: &str = "uploads use presigned URLs";
const THOUGHT: &str = "first, recall how presigned URLs work";

async fn chat(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    ctx.ai().prompt("how do uploads work?").stream_text().await
}

async fn secret(_input: (), _ctx: AiFlowContext) -> AgenkitResult<String> {
    Ok("internal".to_string())
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(MockProvider::new("local").on_prompt_text("uploads", ANSWER))
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("chat", chat).public())
        .flow(Flow::new("secret", secret)) // private — not reachable from a client
        .build()
        .unwrap()
}

#[test]
fn public_flow_streams_redacted_events_to_client() {
    let agenkit = runtime();
    let events: Vec<FlowStreamEvent> = block_on(async {
        let stream = agenkit
            .flow("chat")
            .input(serde_json::Value::Null)
            .stream()
            .expect("public flow yields a stream");
        // Every item is an in-band `Ok` event; the stream ends when the flow
        // finishes (the producer task drops its sender).
        stream
            .map(|item| item.expect("each streamed item is Ok"))
            .collect::<Vec<_>>()
            .await
    });

    // Lifecycle + user-visible output cross under the default FinalOnly cap.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, FlowStreamEvent::FlowStarted { .. })),
        "missing FlowStarted: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, FlowStreamEvent::OutputCompleted)),
        "missing OutputCompleted: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, FlowStreamEvent::FlowCompleted { .. })),
        "missing FlowCompleted: {events:?}"
    );
    // The streamed text reassembles to the full answer.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            FlowStreamEvent::OutputDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, ANSWER);
}

// ── Reasoning gate: text crosses the wire only when the author allowed it AND
// the caller requested it (author ceiling × caller request). ────────────────

async fn reasoned(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    ctx.ai().prompt("reason about uploads").stream_text().await
}

fn reasoning_runtime() -> Agenkit {
    Agenkit::builder()
        .provider(
            MockProvider::new("local")
                .on_prompt_text("reason", ANSWER)
                .thinking(THOUGHT),
        )
        .default_model(ModelRef::new("local/default"))
        // Progress mode so the redacted count-only `ThinkingDelta` also crosses,
        // making the text Some/None distinction observable on the wire.
        .flow(
            Flow::new("reason_allowed", reasoned)
                .public()
                .stream_mode(StreamMode::Progress)
                .expose_reasoning(),
        )
        .flow(
            Flow::new("reason_blocked", reasoned)
                .public()
                .stream_mode(StreamMode::Progress),
        )
        .build()
        .unwrap()
}

/// The first `ThinkingDelta`'s text on the wire: `None` = no reasoning event;
/// `Some(None)` = count-only (text redacted); `Some(Some(t))` = text exposed.
fn streamed_reasoning(agenkit: &Agenkit, id: &str, request: bool) -> Option<Option<String>> {
    block_on(async {
        let stream = agenkit
            .flow(id.to_string())
            .input(serde_json::Value::Null)
            .request_reasoning(request)
            .stream()
            .expect("public flow yields a stream");
        let texts: Vec<Option<String>> = stream
            .filter_map(|item| async move {
                match item.expect("each streamed item is Ok") {
                    FlowStreamEvent::ThinkingDelta { text, .. } => Some(text),
                    _ => None,
                }
            })
            .collect()
            .await;
        texts.into_iter().next()
    })
}

#[test]
fn reasoning_crosses_only_when_author_allows_and_caller_requests() {
    let agenkit = reasoning_runtime();

    // Ceiling open + caller asks → the full reasoning text crosses.
    assert_eq!(
        streamed_reasoning(&agenkit, "reason_allowed", true),
        Some(Some(THOUGHT.to_string())),
        "author allowed + caller requested → reasoning text on the wire"
    );

    // Ceiling open but caller didn't ask → only the redacted count crosses.
    assert_eq!(
        streamed_reasoning(&agenkit, "reason_allowed", false),
        Some(None),
        "author allowed + no request → text stripped to a count"
    );

    // Caller asks but the author never opened the ceiling → still stripped; a
    // caller cannot extract reasoning the author didn't permit.
    assert_eq!(
        streamed_reasoning(&agenkit, "reason_blocked", true),
        Some(None),
        "author disallowed → caller request is ignored, text stripped"
    );
}

#[test]
fn stream_into_is_full_fidelity_including_reasoning() {
    let agenkit = reasoning_runtime();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // `reason_blocked` never opened the wire ceiling, but `stream_into` is the
    // trusted server-side sink — it applies NO redaction, so the dev observing
    // the agent still gets the raw reasoning text.
    let _out: serde_json::Value = block_on(
        agenkit
            .flow("reason_blocked".to_string())
            .input(serde_json::Value::Null)
            .stream_into(tx),
    )
    .expect("flow runs");

    let mut reasoning = None;
    while let Ok(event) = rx.try_recv() {
        if let FlowStreamEvent::ThinkingDelta { text, .. } = event {
            reasoning = Some(text);
        }
    }
    assert_eq!(
        reasoning,
        Some(Some(THOUGHT.to_string())),
        "stream_into delivers raw reasoning regardless of the wire gate"
    );
}

#[test]
fn private_flow_is_indistinguishable_from_unknown() {
    let agenkit = runtime();
    match agenkit
        .flow("secret")
        .input(serde_json::Value::Null)
        .stream()
    {
        Ok(_) => panic!("a non-public flow must not be reachable from a client"),
        Err(err) => assert!(
            err.to_string().contains("unknown AI flow"),
            "expected an unknown-flow error, got: {err}"
        ),
    }
}
