//! RFC-077 Phase 2 — HTTP request telemetry layer.
//!
//! Pins the `request_event_layer()`: events fire for matched and
//! unmatched routes, the `route_pattern` field carries axum's
//! [`MatchedPath`] when present, success/failure are split by
//! status code, and no headers, cookies, query strings, or bodies
//! enter framework events.

// `pocopine-server` is host-only; this test must follow.
#![cfg(not(target_arch = "wasm32"))]
// See `server_plugin.rs` for the rationale.
#![allow(clippy::await_holding_lock)]

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// Tests in this file mutate the process-global plugin registry, so
/// they must not run concurrently with each other or with the
/// `server_plugin` test file. `cargo test` runs every integration
/// test binary in its own process, so this lock only needs to
/// serialize within a single binary.
fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

use pocopine_server::axum::Router;
use pocopine_server::axum::body::{Body, to_bytes};
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_server::axum::response::Response;
use pocopine_server::axum::routing::get;
use pocopine_server::tower::ServiceExt;
use pocopine_server::{
    HttpRequestCompleted, HttpRequestFailed, HttpRequestStarted, Server, ServerHook, ServerPlugin,
    request_event_layer,
};

#[derive(Clone, Debug)]
enum Event {
    Started(HttpRequestStarted),
    Completed(HttpRequestCompleted),
    Failed(HttpRequestFailed),
}

#[derive(Clone, Default)]
struct EventLog {
    events: Arc<Mutex<Vec<Event>>>,
}

impl EventLog {
    fn record(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }

    fn snapshot(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
}

impl ServerHook<HttpRequestStarted> for EventLog {
    fn call(&self, event: HttpRequestStarted) {
        self.record(Event::Started(event));
    }
}

impl ServerHook<HttpRequestCompleted> for EventLog {
    fn call(&self, event: HttpRequestCompleted) {
        self.record(Event::Completed(event));
    }
}

impl ServerHook<HttpRequestFailed> for EventLog {
    fn call(&self, event: HttpRequestFailed) {
        self.record(Event::Failed(event));
    }
}

fn http_event_plugin(events: EventLog) -> impl ServerPlugin {
    move |server: Server| {
        server
            .provide_plugin(events)
            .layer(request_event_layer())
            .hook_plugin::<EventLog, HttpRequestStarted>()
            .hook_plugin::<EventLog, HttpRequestCompleted>()
            .hook_plugin::<EventLog, HttpRequestFailed>()
    }
}

fn finalize_with_events(router: Router, events: EventLog) -> Router {
    Server::new(router)
        .plugin(http_event_plugin(events))
        .try_finalize()
        .expect("plugin validation succeeds")
}

async fn send(router: &mut Router, request: Request<Body>) -> Response {
    router
        .clone()
        .oneshot(request)
        .await
        .expect("oneshot dispatch")
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn matched_route_emits_started_then_completed_with_pattern() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let events = EventLog::default();
    let router = Router::new().route("/users/:id", get(|| async { "hello" }));
    let mut router = finalize_with_events(router, events.clone());

    let response = send(
        &mut router,
        Request::builder()
            .uri("/users/42?token=secret")
            .header("authorization", "Bearer leaky")
            .header("cookie", "session=leaky")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(body_text(response).await, "hello");

    let log = events.snapshot();
    assert_eq!(log.len(), 2, "expected start + completed: {log:?}");

    let Event::Started(start) = &log[0] else {
        panic!("expected started, got {:?}", log[0]);
    };
    assert_eq!(start.method, "GET");
    assert_eq!(start.path, "/users/42");
    assert_eq!(start.route_pattern.as_deref(), Some("/users/:id"));

    let Event::Completed(done) = &log[1] else {
        panic!("expected completed, got {:?}", log[1]);
    };
    assert_eq!(done.status, 200);
    assert_eq!(done.path, "/users/42");
    assert_eq!(done.route_pattern.as_deref(), Some("/users/:id"));
    assert!(done.duration_ms >= 0.0);

    assert_no_secrets(&log);
}

#[tokio::test]
async fn server_error_emits_failed_with_classified_reason() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let events = EventLog::default();
    let router = Router::new().route(
        "/boom",
        get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "broken") }),
    );
    let mut router = finalize_with_events(router, events.clone());

    let response = send(
        &mut router,
        Request::builder().uri("/boom").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(response.status(), 500);

    let log = events.snapshot();
    let failed = log.iter().find_map(|e| match e {
        Event::Failed(f) => Some(f),
        _ => None,
    });
    let failed = failed.expect("expected HttpRequestFailed for 500 response");
    assert_eq!(failed.reason, "internal_server_error");
    assert_eq!(failed.path, "/boom");
    assert_eq!(failed.route_pattern.as_deref(), Some("/boom"));

    let completed = log.iter().any(|e| matches!(e, Event::Completed(_)));
    assert!(
        !completed,
        "5xx must not also fire HttpRequestCompleted: {log:?}"
    );
}

#[tokio::test]
async fn unmatched_route_emits_started_with_no_pattern() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let events = EventLog::default();
    let router = Router::new().route("/known", get(|| async { "ok" }));
    let mut router = finalize_with_events(router, events.clone());

    let _ = send(
        &mut router,
        Request::builder()
            .uri("/unknown")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    let log = events.snapshot();
    let started = log.iter().find_map(|e| match e {
        Event::Started(s) => Some(s),
        _ => None,
    });
    let started = started.expect("expected HttpRequestStarted for unmatched route");
    assert_eq!(started.path, "/unknown");
    assert_eq!(started.route_pattern, None);
}

fn assert_no_secrets(events: &[Event]) {
    let banned = ["Bearer leaky", "session=leaky", "secret"];
    for event in events {
        let serialized = format!("{event:?}");
        for needle in banned {
            assert!(
                !serialized.contains(needle),
                "framework event leaked sensitive header/query value `{needle}`: {serialized}"
            );
        }
    }
}
