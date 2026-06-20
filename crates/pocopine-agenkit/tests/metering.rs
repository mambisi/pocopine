#![cfg(not(target_arch = "wasm32"))]
//! W2 metering: cache-aware [`Usage`] flows through the trace as a populated
//! cost (resolved from the model catalog), and the streaming path emits the
//! `UsageUpdate` event carrying the token counts (incl. cache).
//!
//! Both assertions live in one test on purpose: the cost check reads emitted
//! `pocopine.trace` events through a thread-local subscriber, so a second test
//! emitting traces in parallel in this binary would race it.

mod common;

use common::{block_on, capture};
use pocopine_agenkit::server::{Agenkit, AiFlowContext, Flow, MockProvider};
use pocopine_agenkit_core::{AgenkitResult, FlowStreamEvent, ModelRef, Usage};

/// A catalog model so cost is resolvable.
const MODEL: &str = "anthropic/claude-sonnet-4-6";

/// Explicit cache-bearing usage the mock returns: input 1000, output 500,
/// cache-read 2000, cache-creation 100.
fn usage() -> Usage {
    Usage::new(1_000, 500).with_cache(2_000, 100)
}

async fn chat(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    ctx.ai().prompt("hello").stream_text().await
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(
            MockProvider::new("anthropic")
                .default_text("hi there")
                .with_usage(usage()),
        )
        .default_model(ModelRef::new(MODEL))
        .flow(Flow::new("chat", chat).public())
        .build()
        .unwrap()
}

#[test]
fn metering_carries_cost_in_trace_and_usage_update_on_stream() {
    // claude-sonnet-4-6 rates: in 3.0, out 15.0, cache_read 0.30, cache_create 3.75.
    // (1000*3.0 + 500*15.0 + 2000*0.30 + 100*3.75) / 1e6 = 11475 / 1e6 = 0.011475
    let expected = (1_000.0 * 3.0 + 500.0 * 15.0 + 2_000.0 * 0.30 + 100.0 * 3.75) / 1_000_000.0;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cap = capture(move || {
        let agenkit = runtime();
        block_on(async {
            agenkit
                .flow("chat")
                .input(serde_json::Value::Null)
                .stream_into(tx)
                .await
                .unwrap();
        });
    });

    // (1) The streaming path emits a UsageUpdate carrying the cache token counts.
    let mut found = None;
    while let Ok(event) = rx.try_recv() {
        if let FlowStreamEvent::UsageUpdate {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        } = event
        {
            found = Some((
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
            ));
        }
    }
    assert_eq!(
        found,
        Some((1_000, 500, 2_000, 100)),
        "UsageUpdate should be emitted carrying the cache token counts"
    );

    // (2) The model-response trace carries the catalog-derived cost.
    assert!(
        cap.contains("ai_model_response"),
        "no ai_model_response event: {:?}",
        cap.names()
    );
    assert!(
        cap.costs().iter().any(|c| (c - expected).abs() < 1e-9),
        "expected a cost ~{expected}, captured {:?}",
        cap.costs()
    );
}
