#![cfg(not(target_arch = "wasm32"))]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pocopine_events::{EventDraft, MemoryEventBackend};
use pocopine_live::{KIND_COLLECTION_CHANGED, LIVE_STREAM_PATH, LiveHub, collection_topic, routes};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn forbidden_topic_returns_403() {
    let backend = MemoryEventBackend::new();
    let app = routes(LiveHub::new(backend));
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("{LIVE_STREAM_PATH}?collection=posts"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn no_topics_returns_400() {
    let backend = MemoryEventBackend::new();
    let app = routes(LiveHub::new(backend).allow_all_topics());
    let response = app
        .oneshot(
            Request::builder()
                .uri(LIVE_STREAM_PATH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn allowed_topic_opens_sse_stream() {
    let backend = MemoryEventBackend::new();
    let posts = collection_topic("posts").unwrap();
    let app = routes(LiveHub::new(backend).allow_topics([posts]));
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("{LIVE_STREAM_PATH}?collection=posts"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
    );
}

#[tokio::test]
async fn replayed_event_is_emitted_after_ready_frame() {
    let backend = MemoryEventBackend::new();
    backend
        .publish_now(
            EventDraft::new(
                "collection:posts",
                KIND_COLLECTION_CHANGED,
                json!({ "collection": "posts" }),
            )
            .unwrap(),
        )
        .unwrap();
    let posts = collection_topic("posts").unwrap();
    let app = routes(LiveHub::new(backend).allow_topics([posts]));
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("{LIVE_STREAM_PATH}?collection=posts"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body();
    let body_text = read_sse_frames(body, 2).await;

    assert!(body_text.contains("event: ready"));
    assert!(body_text.contains("event: collection.changed"));
    assert!(body_text.contains("id: memory:1"));
}

async fn read_sse_frames(mut body: Body, count: usize) -> String {
    let mut out = String::new();
    for _ in 0..count {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
            .await
            .expect("timed out waiting for SSE frame")
            .expect("SSE body ended before expected frame")
            .expect("SSE body returned an error");
        if let Ok(data) = frame.into_data() {
            out.push_str(std::str::from_utf8(&data).expect("SSE data should be utf-8"));
        }
    }
    out
}
