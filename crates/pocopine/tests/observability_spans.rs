//! RFC-123 — the server trunk: `pocopine.http.request` →
//! `pocopine.server_function`, with every `pocopine.trace` event hanging
//! from the request span and `request_id` shared between span and events.
#![cfg(all(not(target_arch = "wasm32"), feature = "logging"))]
#![allow(clippy::await_holding_lock)]

mod support;

use std::sync::{Mutex, MutexGuard, OnceLock};

use pocopine::{ServerError, ServerResult};
use pocopine_server::Server;
use pocopine_server::axum::Router;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Method, Request};
use pocopine_server::tower::ServiceExt;
use support::TraceCapture;

fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[pocopine::server(public)]
async fn spanned_echo(value: String) -> ServerResult<String> {
    tracing::warn!(target: "app::spanned", "app warning inside the handler");
    Ok(format!("echo:{value}"))
}

#[pocopine::server(public)]
async fn spanned_fail(value: String) -> ServerResult<()> {
    let _ = value;
    Err(ServerError::Forbidden("nope".into()))
}

fn finalize() -> Router {
    Server::new(Router::new())
        .plugin(pocopine::logging::server_observability())
        .try_finalize()
        .expect("server observability plugin should validate")
}

fn post(uri: &str, body: &'static str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-pocopine-session", "7f3a9c1e5b2d4f60a8c1e2d3f4a5b6c7")
        .body(Body::from(body))
        .unwrap()
}

fn run_request(request: Request<Body>) -> (TraceCapture, u16, Option<String>) {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();
    let (status, request_id_header) = capture.run(|| {
        rt.block_on(async {
            let router = finalize();
            let response = router.oneshot(request).await.expect("oneshot dispatch");
            let header = response
                .headers()
                .get("x-request-id")
                .map(|v| v.to_str().unwrap().to_owned());
            (response.status().as_u16(), header)
        })
    });
    (capture, status, request_id_header)
}

#[test]
fn trace_events_hang_from_the_request_span() {
    let (capture, status, request_id_header) = run_request(post(__spanned_echo_path(), "[\"hi\"]"));
    assert_eq!(status, 200);

    let completed = capture
        .events_with_message("pocopine.trace", "server function completed")
        .pop()
        .expect("server function completed event");
    assert_eq!(
        completed.ancestry(),
        ["pocopine.http.request", "pocopine.server_function"]
    );

    // Every pocopine.trace event emitted during the request — the
    // middleware's hook events included — has the request span as root.
    let trace_events: Vec<_> = capture
        .events()
        .into_iter()
        .filter(|event| event.target == "pocopine.trace")
        .collect();
    assert!(trace_events.len() >= 4, "{trace_events:?}");
    for event in &trace_events {
        assert_eq!(
            event.ancestry().first(),
            Some(&"pocopine.http.request"),
            "{event:?}"
        );
    }

    // App events inside the handler nest for free (§A.6).
    let app_warning = capture
        .events()
        .into_iter()
        .find(|event| event.target == "app::spanned")
        .expect("app warning captured");
    assert_eq!(
        app_warning.ancestry(),
        ["pocopine.http.request", "pocopine.server_function"]
    );

    let request = capture.span("pocopine.http.request");
    assert_eq!(request.target, "pocopine.trace");
    assert_eq!(request.field("otel.kind"), Some("server"));
    assert_eq!(request.field("http.request.method"), Some("POST"));
    assert_eq!(request.field("http.route"), Some(__spanned_echo_path()));
    assert_eq!(
        request.field("otel.name").map(str::to_owned),
        Some(format!("POST {}", __spanned_echo_path()))
    );
    assert_eq!(request.field("url.path"), Some(__spanned_echo_path()));
    assert_eq!(request.field("http.response.status_code"), Some("200"));
    assert_eq!(request.field("otel.status_code"), Some("OK"));
    assert_eq!(
        request.field("session.id"),
        Some("7f3a9c1e5b2d4f60a8c1e2d3f4a5b6c7")
    );
    assert_eq!(request.parent, None, "the request span is a root");

    let function = capture.span("pocopine.server_function");
    assert_eq!(function.parent, Some(request.id));
    assert_eq!(function.field("otel.kind"), Some("internal"));
    assert_eq!(function.field("pocopine.function"), Some("spanned_echo"));
    assert_eq!(
        function.field("pocopine.function_path"),
        Some(__spanned_echo_function_path())
    );
    assert_eq!(function.field("otel.status_code"), Some("OK"));
    assert_eq!(function.field("error.type"), None);

    // One request id: on both spans, on the events, and on the response.
    let request_id = request
        .field("pocopine.request_id")
        .expect("request span carries the request id")
        .to_owned();
    assert_eq!(
        function.field("pocopine.request_id"),
        Some(request_id.as_str())
    );
    assert_eq!(
        completed.field("request_id"),
        None,
        "the completed event never copied it"
    );
    assert_eq!(request_id_header.as_deref(), Some(request_id.as_str()));
    let hook_completed = capture
        .events()
        .into_iter()
        .find(|event| event.field("event_name") == Some("http_request_completed"))
        .expect("http_request_completed observed event");
    assert!(
        hook_completed.fields.values().any(|v| v == &request_id),
        "{hook_completed:?}"
    );
}

#[test]
fn failed_server_function_closes_its_span_as_error() {
    let (capture, status, _) = run_request(post(__spanned_fail_path(), "[\"x\"]"));
    assert_eq!(status, 200, "server-fn errors ride a 200 with an Err body");

    let function = capture.span("pocopine.server_function");
    assert_eq!(function.field("otel.status_code"), Some("ERROR"));
    assert_eq!(function.field("error.type"), Some("forbidden"));

    let request = capture.span("pocopine.http.request");
    assert_eq!(request.field("otel.status_code"), Some("OK"));
    assert_eq!(request.field("error.type"), None);
}

#[test]
fn rejected_body_closes_the_span_as_bad_request() {
    let (capture, status, _) = run_request(post(__spanned_echo_path(), "not json"));
    assert_eq!(status, 200);
    let function = capture.span("pocopine.server_function");
    assert_eq!(function.field("otel.status_code"), Some("ERROR"));
    assert_eq!(function.field("error.type"), Some("bad_request"));
}

#[test]
fn malformed_session_header_is_not_recorded() {
    let request = Request::builder()
        .method(Method::POST)
        .uri(__spanned_echo_path())
        .header("content-type", "application/json")
        .header("x-pocopine-session", "drop table; -- injected text")
        .body(Body::from("[\"hi\"]"))
        .unwrap();
    let (capture, status, _) = run_request(request);
    assert_eq!(status, 200);
    let request = capture.span("pocopine.http.request");
    assert_eq!(request.field("session.id"), None);
    assert!(
        capture
            .spans()
            .iter()
            .all(|span| !span.fields.values().any(|v| v.contains("injected"))),
        "rejected header text must not reach any span"
    );
}
