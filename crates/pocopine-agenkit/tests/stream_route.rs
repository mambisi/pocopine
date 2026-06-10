#![cfg(not(target_arch = "wasm32"))]
//! Phase 3.3: the AI-flow SSE route streams redacted FlowStreamEvents and the
//! final result, driven in-process via tower's `oneshot`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pocopine_agenkit::server::{Agenkit, AiFlowContext, Flow, MockProvider, ai_flow_stream_router};
use pocopine_agenkit_core::{AgenkitResult, ModelRef};
use tower::ServiceExt;

async fn echo(_input: serde_json::Value, _ctx: AiFlowContext) -> AgenkitResult<serde_json::Value> {
    Ok(serde_json::json!({"ok": true, "secret_field_api_key": "should-not-be-in-stream"}))
}

fn runtime() -> Agenkit {
    Agenkit::builder()
        .provider(MockProvider::new("local"))
        .default_model(ModelRef::new("local/default"))
        .flow(Flow::new("echo", echo).public())
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_route_emits_sse_events_and_result() {
    let app = ai_flow_stream_router(runtime());
    let request = Request::builder()
        .method("POST")
        .uri("/__pocopine/agenkit/v1/flow/echo/stream?mode=progress")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream",
        "expected an SSE response"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);

    // The public stream carries lifecycle + the final output.
    assert!(text.contains("flow_started"), "body: {text}");
    assert!(text.contains("flow_completed"), "body: {text}");
    assert!(text.contains("output_completed"), "body: {text}");
    // The final result is surfaced as user-visible output.
    assert!(text.contains("\\\"ok\\\":true"), "body: {text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn final_only_mode_omits_step_progress() {
    let app = ai_flow_stream_router(runtime());
    let request = Request::builder()
        .method("POST")
        .uri("/__pocopine/agenkit/v1/flow/echo/stream?mode=final")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);

    assert!(text.contains("flow_completed"));
    // No model/step/tool progress events in FinalOnly mode.
    assert!(!text.contains("ai_model"), "body: {text}");
    assert!(!text.contains("step_started"), "body: {text}");
}
