//! RFC-123 §5.5 — the browser trace relay route: a valid batch becomes
//! `client_span_closed` events carrying the client's ids; anything
//! malformed, oversized, unknown, or too frequent is refused whole.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::await_holding_lock)]

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pocopine_logging::{ServerObservabilityConfig, server_observability_with_config};
use pocopine_observe::client_relay::{ClientSpanRecord, MAX_BODY_BYTES, PATH};
use pocopine_observe::test_support::SpanCapture;
use pocopine_server::Server;
use pocopine_server::axum::Router;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Method, Request, StatusCode};
use pocopine_server::tower::ServiceExt;

fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1_000.0
}

fn call_record(session: &str) -> ClientSpanRecord {
    let mut fields = BTreeMap::new();
    fields.insert("session.id".to_owned(), session.to_owned());
    fields.insert("http.request.method".to_owned(), "POST".to_owned());
    fields.insert("http.route".to_owned(), "/api/summarize".to_owned());
    fields.insert("http.response.status_code".to_owned(), "200".to_owned());
    fields.insert("pocopine.request_id".to_owned(), "42".to_owned());
    fields.insert("otel.status_code".to_owned(), "OK".to_owned());
    let now = now_ms();
    ClientSpanRecord {
        name: "pocopine.client.server_function".to_owned(),
        trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".to_owned(),
        span_id: "c1f2e3d4a5b6c7d8".to_owned(),
        parent_span_id: Some("a1b2c3d4e5f60718".to_owned()),
        start_unix_ms: now - 830.0,
        end_unix_ms: now,
        fields,
    }
}

fn router() -> Router {
    Server::new(Router::new())
        .plugin(server_observability_with_config(
            ServerObservabilityConfig::new().with_client_trace_relay(true),
        ))
        .try_finalize()
        .expect("finalize")
}

fn post(router: &Router, body: String) -> StatusCode {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(PATH)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    })
}

#[test]
fn a_valid_batch_becomes_client_span_closed_events() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let capture = SpanCapture::new();
    let record = call_record("7f3a9c1e5b2d4f60a8c1e2d3f4a5b6c7");
    let status = capture.run(|| post(&router(), serde_json::to_string(&vec![record]).unwrap()));
    assert_eq!(status, StatusCode::NO_CONTENT);

    let closed: Vec<_> = capture
        .events()
        .into_iter()
        .filter(|e| e.field("event_name") == Some("client_span_closed"))
        .collect();
    assert_eq!(closed.len(), 1, "{closed:?}");
    let event = &closed[0];
    assert_eq!(event.target, "pocopine.trace");
    let all: String = event.fields.values().cloned().collect::<Vec<_>>().join(" ");
    assert!(all.contains("pocopine.client.server_function"), "{event:?}");
    assert!(
        all.contains("4bf92f3577b34da6a3ce929d0e0e4736"),
        "trace id: {event:?}"
    );
    assert!(all.contains("c1f2e3d4a5b6c7d8"), "span id: {event:?}");
    assert!(all.contains("/api/summarize"), "{event:?}");
    // The relay route is an ordinary request: it has its own request span.
    assert_eq!(event.ancestry().first(), Some(&"pocopine.http.request"));
}

#[test]
fn refused_batches_emit_nothing() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let capture = SpanCapture::new();
    let router = router();

    let mut unknown = call_record("refused-session-1");
    unknown.name = "pocopine.http.request".into();
    let mut bad_id = call_record("refused-session-2");
    bad_id.trace_id = "not-hex".into();
    let mut free_text = call_record("refused-session-3");
    free_text
        .fields
        .insert("message".into(), "hello; drop table".into());
    let good_and_bad = vec![call_record("refused-session-4"), unknown.clone()];

    capture.run(|| {
        for (body, expected) in [
            (
                serde_json::to_string(&vec![unknown]).unwrap(),
                StatusCode::BAD_REQUEST,
            ),
            (
                serde_json::to_string(&vec![bad_id]).unwrap(),
                StatusCode::BAD_REQUEST,
            ),
            (
                serde_json::to_string(&vec![free_text]).unwrap(),
                StatusCode::BAD_REQUEST,
            ),
            // One bad record refuses the whole batch.
            (
                serde_json::to_string(&good_and_bad).unwrap(),
                StatusCode::BAD_REQUEST,
            ),
            ("[]".to_owned(), StatusCode::BAD_REQUEST),
            ("not json".to_owned(), StatusCode::BAD_REQUEST),
            (
                "x".repeat(MAX_BODY_BYTES + 1),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ] {
            assert_eq!(post(&router, body), expected);
        }
    });
    assert!(
        !capture
            .events()
            .iter()
            .any(|e| e.field("event_name") == Some("client_span_closed")),
        "nothing re-emitted"
    );
}

#[test]
fn a_session_is_rate_limited() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let router = router();
    let body = serde_json::to_string(&vec![call_record("rate-limited-session")]).unwrap();
    let statuses: Vec<StatusCode> = (0..11).map(|_| post(&router, body.clone())).collect();
    assert!(
        statuses[..10].iter().all(|s| *s == StatusCode::NO_CONTENT),
        "{statuses:?}"
    );
    assert_eq!(statuses[10], StatusCode::TOO_MANY_REQUESTS);
}
