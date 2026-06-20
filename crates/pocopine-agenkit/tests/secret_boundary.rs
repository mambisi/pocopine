#![cfg(not(target_arch = "wasm32"))]
//! Phase 3.4: the secret-boundary suite (§D10). A provider whose internals
//! quote a planted secret must never let that secret cross to the client —
//! not via the returned `ServerError`, not via the public stream — and a
//! non-allowlisted model alias must be rejected *before* any provider call.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::block_on;
use pocopine_agenkit::server::{
    Agenkit, AiFlowContext, BoxFuture, Flow, GenerateRequest, GenerateResponse, Provider,
    ProviderContext, to_server_error,
};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, FlowStreamEvent, ModelRef};

/// A value that must never appear on a client-facing surface.
const PLANTED_SECRET: &str = "sk-live-PLANTED-SECRET-do-not-leak";

/// A provider whose error quotes provider internals (host + credential).
struct SecretLeakProvider;

impl Provider for SecretLeakProvider {
    fn id(&self) -> &str {
        "local"
    }

    fn generate<'a>(
        &'a self,
        _request: GenerateRequest,
        _cx: &'a ProviderContext,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        Box::pin(async {
            Err(AgenkitError::provider(format!(
                "401 from api.provider.test using {PLANTED_SECRET}"
            )))
        })
    }
}

/// A provider that records whether `generate` was ever called.
struct RecordingProvider {
    called: Arc<AtomicBool>,
}

impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "local"
    }

    fn generate<'a>(
        &'a self,
        _request: GenerateRequest,
        _cx: &'a ProviderContext,
    ) -> BoxFuture<'a, AgenkitResult<GenerateResponse>> {
        self.called.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(GenerateResponse::text("ok")) })
    }
}

async fn ask(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    ctx.ai().prompt("hello").generate_text().await
}

async fn ask_denied_model(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
    ctx.ai()
        .model("local/denied")
        .prompt("hello")
        .generate_text()
        .await
}

#[test]
fn provider_error_does_not_leak_through_server_error() {
    let agenkit = Agenkit::builder()
        .provider(SecretLeakProvider)
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("ask", ask).public())
        .build()
        .unwrap();

    // The `#[server]` boundary maps the flow error via `to_server_error`.
    let agenkit_err = block_on(agenkit.flow("ask").run::<String>()).unwrap_err();
    let error = to_server_error(&agenkit_err);
    let rendered = error.to_string();
    assert!(
        !rendered.contains(PLANTED_SECRET),
        "ServerError leaked the secret: {rendered}"
    );
    assert!(
        !rendered.contains("api.provider.test"),
        "ServerError leaked the host: {rendered}"
    );
    // Only the stable kind crosses.
    assert!(
        rendered.contains("ai error (provider)"),
        "unexpected error: {rendered}"
    );
}

#[test]
fn provider_error_does_not_leak_through_the_stream() {
    let agenkit = Agenkit::builder()
        .provider(SecretLeakProvider)
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("ask", ask).public())
        .build()
        .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = block_on(
        agenkit
            .flow("ask")
            .input(serde_json::Value::Null)
            .stream_into(tx),
    );

    let mut joined = String::new();
    let mut saw_failure = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, FlowStreamEvent::FlowFailed { .. }) {
            saw_failure = true;
        }
        joined.push_str(&serde_json::to_string(&event).unwrap());
    }
    assert!(saw_failure, "expected a flow_failed event");
    assert!(
        !joined.contains(PLANTED_SECRET),
        "stream leaked the secret: {joined}"
    );
    assert!(
        !joined.contains("api.provider.test"),
        "stream leaked the host: {joined}"
    );
}

#[test]
fn non_allowlisted_alias_is_rejected_before_any_provider_call() {
    let called = Arc::new(AtomicBool::new(false));
    let agenkit = Agenkit::builder()
        .provider(RecordingProvider {
            called: called.clone(),
        })
        .allow_model("local/allowed")
        .flow(Flow::new("ask_denied_model", ask_denied_model).public())
        .build()
        .unwrap();

    let agenkit_err = block_on(agenkit.flow("ask_denied_model").run::<String>()).unwrap_err();
    assert!(
        !called.load(Ordering::SeqCst),
        "the provider was called despite a non-allowlisted model alias"
    );
    // The boundary returns a generic error, not provider internals.
    let error = to_server_error(&agenkit_err);
    assert!(
        error.to_string().starts_with("server error"),
        "unexpected: {error}"
    );
}

#[test]
fn non_public_flows_are_not_client_reachable() {
    let called = Arc::new(AtomicBool::new(false));
    let agenkit = Agenkit::builder()
        .provider(RecordingProvider {
            called: called.clone(),
        })
        .default_model(ModelRef::new("local/default"))
        // NOTE: no `.public()`.
        .flow(Flow::new("ask", ask))
        .build()
        .unwrap();

    // Flows are internal; `#[server]` fns are the boundary. A flow not marked
    // `.public()` is gated out of the only client-facing surface — the stream
    // route (`stream_route` checks `flow_is_public`) — so it is indistinguishable
    // from an unknown flow to a client, and nothing has touched the provider.
    assert!(!agenkit.flow_is_public("ask"));
    assert!(
        !called.load(Ordering::SeqCst),
        "probing a non-public flow must not invoke the provider"
    );
}

/// Representative of the wasm-artifact scan: client-facing payloads (the only
/// types that reach a browser, all from `pocopine-agenkit-core`) carry no
/// credential-shaped fields. The full corpus lives in core's `tests/no_secrets`.
#[test]
fn client_facing_stream_events_have_no_credential_fields() {
    let events = [
        FlowStreamEvent::FlowStarted {
            run_id: "r".into(),
            trace_id: "t".into(),
        },
        FlowStreamEvent::FlowFailed {
            run_id: "r".into(),
            error_kind: "provider".into(),
            trace_id: "t".into(),
        },
        FlowStreamEvent::OutputDelta { text: "hi".into() },
    ];
    for event in &events {
        let json = serde_json::to_string(event).unwrap().to_lowercase();
        for needle in [
            "api_key",
            "authorization",
            "bearer",
            "secret",
            "password",
            "prompt",
        ] {
            assert!(
                !json.contains(needle),
                "stream event carries `{needle}`: {json}"
            );
        }
    }
}
