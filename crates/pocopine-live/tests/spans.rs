#![cfg(not(target_arch = "wasm32"))]
//! RFC-123 Phase 4 — a live SSE stream runs inside `pocopine.http.request`
//! for its whole life (the body wrapper), and every delivered event —
//! replayed or live — is a short `pocopine.live.event` child span.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pocopine_events::{EventDraft, MemoryEventBackend};
use pocopine_live::{KIND_COLLECTION_CHANGED, LIVE_STREAM_PATH, LiveHub, collection_topic, routes};
use pocopine_observe::test_support::SpanCapture;
use pocopine_server::{RequestEventOptions, Server};
use serde_json::json;
use tower::ServiceExt;

async fn next_frame_text(body: &mut Body) -> String {
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
        .await
        .expect("timed out waiting for SSE frame")
        .expect("SSE body ended before expected frame")
        .expect("SSE body returned an error");
    frame
        .into_data()
        .map(|data| String::from_utf8(data.to_vec()).expect("utf-8"))
        .unwrap_or_default()
}

fn publish(backend: &MemoryEventBackend) {
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
}

#[test]
fn live_events_hang_from_the_request_span_for_the_streams_life() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = SpanCapture::new();
    let probe = capture.clone();

    capture.run(|| {
        rt.block_on(async {
            let backend = MemoryEventBackend::new();
            publish(&backend); // replayed on open
            let posts = collection_topic("posts").unwrap();
            let app = Server::new(routes(LiveHub::new(backend.clone()).allow_topics([posts])))
                .request_events(RequestEventOptions::default())
                .try_finalize()
                .expect("finalize");

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
            let mut body = response.into_body();

            let ready = next_frame_text(&mut body).await;
            assert!(ready.contains("event: ready"), "{ready}");
            let replayed = next_frame_text(&mut body).await;
            assert!(replayed.contains("id: memory:1"), "{replayed}");

            // The request span is still open while the body streams.
            assert!(!probe.span("pocopine.http.request").closed);

            publish(&backend); // delivered live
            let live = next_frame_text(&mut body).await;
            assert!(live.contains("id: memory:2"), "{live}");
            drop(body);
        });
    });

    let request = capture.span("pocopine.http.request");
    assert!(request.closed, "closed when the body was dropped");
    assert_eq!(request.field("http.route"), Some(LIVE_STREAM_PATH));

    let events = capture.spans_named("pocopine.live.event");
    assert_eq!(events.len(), 2, "{events:?}");
    for (event, cursor) in events.iter().zip(["memory:1", "memory:2"]) {
        assert_eq!(event.parent, Some(request.id), "{event:?}");
        assert_eq!(
            event.field("pocopine.live.kind"),
            Some(KIND_COLLECTION_CHANGED)
        );
        assert_eq!(event.field("pocopine.live.cursor"), Some(cursor));
        assert!(event.closed);
    }
}
