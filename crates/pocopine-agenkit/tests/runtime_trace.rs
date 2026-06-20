#![cfg(not(target_arch = "wasm32"))]
//! The long-lived conversational runtime emits `pocopine.trace` for its turns —
//! the same agent/model spans (with token [`Usage`] → catalog cost) the
//! single-shot agent loop does, now that both drive the shared loop core. This is
//! the regression guard for "conversational turns are metered": before the loop
//! unification the runtime emitted only its `AgentEvent` firehose and no trace.
//!
//! Reads emitted events through a thread-local subscriber, so keep this binary to
//! one trace-driving test (a parallel one would race the capture).

mod common;

use common::{block_on, capture};
use pocopine_agenkit::server::{Agenkit, AgentEvent, AgentSession, MockProvider};
use pocopine_agenkit_core::{ModelRef, Usage};
use tokio_stream::StreamExt;

/// A catalog model so cost is resolvable from usage.
const MODEL: &str = "anthropic/claude-sonnet-4-6";

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(
            MockProvider::new("anthropic")
                .default_text("hi there")
                // Explicit usage so the cost is deterministic (not prompt-derived).
                .with_usage(Usage::new(1_000, 500)),
        )
        .default_model(ModelRef::new(MODEL))
        .build()
        .unwrap()
}

#[test]
fn a_conversational_turn_emits_model_trace_with_cost() {
    // claude-sonnet-4-6 rates: in 3.0, out 15.0 per 1e6 tokens.
    // (1000*3.0 + 500*15.0) / 1e6 = 10500 / 1e6 = 0.0105
    let expected = (1_000.0 * 3.0 + 500.0 * 15.0) / 1_000_000.0;

    let cap = capture(|| {
        let agenkit = runtime();
        block_on(async {
            let session = AgentSession::builder(&agenkit).open(None).await.unwrap();
            // Drain the turn to completion so the spawned task (which emits the
            // trace) runs on this current-thread runtime, under the subscriber.
            let mut stream = session.prompt("hi");
            let mut last = None;
            while let Some(ev) = stream.next().await {
                last = Some(ev);
            }
            assert!(
                matches!(last, Some(AgentEvent::Stopped { .. })),
                "turn should finish cleanly: {last:?}"
            );
        });
    });

    // The turn opened a trace run (an agent step) and emitted a model response...
    assert!(
        cap.contains("ai_step_started"),
        "the turn should open an agent step: {:?}",
        cap.names()
    );
    assert!(
        cap.contains("ai_model_response"),
        "the turn should emit a model response: {:?}",
        cap.names()
    );
    // ...carrying the catalog-derived cost — conversational turns are now metered.
    assert!(
        cap.costs().iter().any(|c| (c - expected).abs() < 1e-9),
        "expected a model-response cost ~{expected}, captured {:?}",
        cap.costs()
    );
}
