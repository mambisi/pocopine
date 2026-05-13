//! RFC-077 Phase 3 — typed server-function lifecycle hooks.
//!
//! `#[server]`-generated routes emit a started event before
//! guard/body work, completed/failed events on user-handler return,
//! and rejected events for guard/body-read/body-parse failures —
//! beside (not instead of) the existing `pocopine.trace` /
//! `pocopine.log` tracing emissions.
#![cfg(not(target_arch = "wasm32"))]
// `registry_lock` holds the test-only `MutexGuard` across `.await`
// to serialize tests that share the process-global plugin registry.
#![allow(clippy::await_holding_lock)]

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use pocopine::ServerResult;
use pocopine_auth::{AuthFuture, AuthProvider, AuthUser, RequestContext};
use pocopine_server::axum::body::{to_bytes, Body};
use pocopine_server::axum::http::{Method, Request};
use pocopine_server::axum::response::Response;
use pocopine_server::axum::Router;
use pocopine_server::tower::ServiceExt;
use pocopine_server::{
    request_event_layer, RouterAuthExt, Server, ServerFunctionCompleted, ServerFunctionFailed,
    ServerFunctionRejected, ServerFunctionStarted, ServerHook, ServerPlugin,
};

/// Tests share the process-global plugin registry, so they must
/// run one-at-a-time within this binary.
fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[derive(Clone, Debug)]
enum Event {
    Started(ServerFunctionStarted),
    Completed(ServerFunctionCompleted),
    Rejected(ServerFunctionRejected),
    Failed(ServerFunctionFailed),
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

impl ServerHook<ServerFunctionStarted> for EventLog {
    fn call(&self, event: ServerFunctionStarted) {
        self.record(Event::Started(event));
    }
}

impl ServerHook<ServerFunctionCompleted> for EventLog {
    fn call(&self, event: ServerFunctionCompleted) {
        self.record(Event::Completed(event));
    }
}

impl ServerHook<ServerFunctionRejected> for EventLog {
    fn call(&self, event: ServerFunctionRejected) {
        self.record(Event::Rejected(event));
    }
}

impl ServerHook<ServerFunctionFailed> for EventLog {
    fn call(&self, event: ServerFunctionFailed) {
        self.record(Event::Failed(event));
    }
}

fn server_function_event_plugin(events: EventLog) -> impl ServerPlugin {
    move |server: Server| {
        server
            .provide_plugin(events)
            .layer(request_event_layer())
            .hook_plugin::<EventLog, ServerFunctionStarted>()
            .hook_plugin::<EventLog, ServerFunctionCompleted>()
            .hook_plugin::<EventLog, ServerFunctionRejected>()
            .hook_plugin::<EventLog, ServerFunctionFailed>()
    }
}

/// Same as `server_function_event_plugin` but without
/// `request_event_layer()` — pins the macro's fallback path that
/// self-allocates a `request_id` when the HTTP layer hasn't stamped
/// one in extensions.
fn server_function_event_plugin_without_layer(events: EventLog) -> impl ServerPlugin {
    move |server: Server| {
        server
            .provide_plugin(events)
            .hook_plugin::<EventLog, ServerFunctionStarted>()
            .hook_plugin::<EventLog, ServerFunctionCompleted>()
            .hook_plugin::<EventLog, ServerFunctionRejected>()
            .hook_plugin::<EventLog, ServerFunctionFailed>()
    }
}

#[derive(Clone)]
struct StubProvider;

impl AuthProvider for StubProvider {
    fn authenticate<'a>(&'a self, ctx: &'a RequestContext) -> AuthFuture<'a, Option<AuthUser>> {
        Box::pin(async move {
            match ctx.bearer_token() {
                Some("good") => Ok(Some(AuthUser::new("user-1"))),
                Some(_) | None => Ok(None),
            }
        })
    }
}

#[pocopine::server(public)]
async fn echo(text: String) -> ServerResult<String> {
    Ok(format!("echo:{text}"))
}

#[pocopine::server(public)]
async fn fail_with_app(_label: String) -> ServerResult<()> {
    Err(pocopine::ServerError::App("nope".into()))
}

#[pocopine::server(guard = pocopine::auth::require_login)]
async fn guarded(text: String) -> ServerResult<String> {
    Ok(format!("ok:{text}"))
}

fn build_public_router() -> Router {
    Router::new()
}

fn build_guarded_router() -> Router {
    Router::new()
}

fn finalize(router: Router, events: EventLog) -> Router {
    Server::new(router)
        .plugin(server_function_event_plugin(events))
        .try_finalize()
        .expect("plugin validation succeeds")
}

fn finalize_with_auth(router: Router, events: EventLog) -> Router {
    Server::new(router)
        .router_mut(|router| router.with_auth(StubProvider))
        .plugin(server_function_event_plugin(events))
        .try_finalize()
        .expect("plugin validation succeeds")
}

async fn send(router: &Router, request: Request<Body>) -> Response {
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
async fn successful_call_emits_started_then_completed_with_shared_request_id() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let events = EventLog::default();
    let router = finalize(build_public_router(), events.clone());

    let response = send(
        &router,
        Request::builder()
            .method(Method::POST)
            .uri(__echo_path())
            .header("content-type", "application/json")
            .body(Body::from("[\"hi\"]"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body = body_text(response).await;
    assert!(body.contains("echo:hi"), "unexpected body: {body}");

    let log = events.snapshot();
    let started = log.iter().find_map(|e| match e {
        Event::Started(s) => Some(s.clone()),
        _ => None,
    });
    let completed = log.iter().find_map(|e| match e {
        Event::Completed(c) => Some(c.clone()),
        _ => None,
    });
    let started = started.expect("expected ServerFunctionStarted");
    let completed = completed.expect("expected ServerFunctionCompleted");

    assert_eq!(started.function, "echo");
    assert_eq!(completed.function, "echo");
    assert_eq!(
        started.request_id, completed.request_id,
        "started/completed must share request_id (HTTP layer correlation)"
    );
    assert!(completed.duration_ms >= 0.0);
}

#[tokio::test]
async fn app_error_emits_failed_with_error_class() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let events = EventLog::default();
    let router = finalize(build_public_router(), events.clone());

    let response = send(
        &router,
        Request::builder()
            .method(Method::POST)
            .uri(__fail_with_app_path())
            .header("content-type", "application/json")
            .body(Body::from("[\"x\"]"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);

    let log = events.snapshot();
    let failed = log.iter().find_map(|e| match e {
        Event::Failed(f) => Some(f.clone()),
        _ => None,
    });
    let failed = failed.expect("expected ServerFunctionFailed");
    assert_eq!(failed.function, "fail_with_app");
    assert_eq!(failed.error_class, "app");

    let completed = log.iter().any(|e| matches!(e, Event::Completed(_)));
    assert!(
        !completed,
        "Err result must not also emit ServerFunctionCompleted: {log:?}"
    );
}

#[tokio::test]
async fn body_parse_failure_emits_rejected_with_bad_request() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let events = EventLog::default();
    let router = finalize(build_public_router(), events.clone());

    let response = send(
        &router,
        Request::builder()
            .method(Method::POST)
            .uri(__echo_path())
            .header("content-type", "application/json")
            .body(Body::from("not json"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);

    let log = events.snapshot();
    let rejected = log.iter().find_map(|e| match e {
        Event::Rejected(r) => Some(r.clone()),
        _ => None,
    });
    let rejected = rejected.expect("expected ServerFunctionRejected");
    assert_eq!(rejected.function, "echo");
    assert_eq!(rejected.status, 400);
    assert_eq!(rejected.reason, "bad_request");

    let started = log.iter().any(|e| matches!(e, Event::Started(_)));
    assert!(started, "Started should fire before body parse: {log:?}");
}

#[tokio::test]
async fn guard_rejection_emits_rejected_with_unauthorized() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let events = EventLog::default();
    let router = finalize_with_auth(build_guarded_router(), events.clone());

    let response = send(
        &router,
        Request::builder()
            .method(Method::POST)
            .uri(__guarded_path())
            .header("content-type", "application/json")
            .body(Body::from("[\"hi\"]"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);

    let log = events.snapshot();
    let rejected = log.iter().find_map(|e| match e {
        Event::Rejected(r) => Some(r.clone()),
        _ => None,
    });
    let rejected = rejected.expect("expected ServerFunctionRejected for missing bearer");
    assert_eq!(rejected.function, "guarded");
    assert_eq!(rejected.status, 401);
    assert_eq!(rejected.reason, "unauthorized");

    let completed = log.iter().any(|e| matches!(e, Event::Completed(_)));
    assert!(
        !completed,
        "guard rejection must not emit Completed: {log:?}"
    );
}

#[tokio::test]
async fn guarded_call_with_good_token_emits_started_then_completed() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let events = EventLog::default();
    let router = finalize_with_auth(build_guarded_router(), events.clone());

    let response = send(
        &router,
        Request::builder()
            .method(Method::POST)
            .uri(__guarded_path())
            .header("content-type", "application/json")
            .header("authorization", "Bearer good")
            .body(Body::from("[\"hi\"]"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body = body_text(response).await;
    assert!(body.contains("ok:hi"), "unexpected body: {body}");

    let log = events.snapshot();
    let has_started = log
        .iter()
        .any(|e| matches!(e, Event::Started(s) if s.function == "guarded"));
    let has_completed = log
        .iter()
        .any(|e| matches!(e, Event::Completed(c) if c.function == "guarded"));
    assert!(
        has_started && has_completed,
        "expected started + completed for authorized call: {log:?}"
    );

    let no_rejected = log.iter().any(|e| matches!(e, Event::Rejected(_)));
    let no_failed = log.iter().any(|e| matches!(e, Event::Failed(_)));
    assert!(!no_rejected, "no rejection on authorized call: {log:?}");
    assert!(!no_failed, "no failure on successful call: {log:?}");
}

#[tokio::test]
async fn server_function_events_self_allocate_request_id_without_http_layer() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let events = EventLog::default();
    // Build a Server WITHOUT request_event_layer so the macro's
    // RequestId fallback path runs.
    let router = Server::new(build_public_router())
        .plugin(server_function_event_plugin_without_layer(events.clone()))
        .try_finalize()
        .expect("plugin validation succeeds");

    // Two independent calls — each should produce its own
    // started/completed pair with a unique request_id, since
    // there's no HTTP layer to share an id with.
    let r1 = send(
        &router,
        Request::builder()
            .method(Method::POST)
            .uri(__echo_path())
            .header("content-type", "application/json")
            .body(Body::from("[\"first\"]"))
            .unwrap(),
    )
    .await;
    assert_eq!(r1.status(), 200);

    let r2 = send(
        &router,
        Request::builder()
            .method(Method::POST)
            .uri(__echo_path())
            .header("content-type", "application/json")
            .body(Body::from("[\"second\"]"))
            .unwrap(),
    )
    .await;
    assert_eq!(r2.status(), 200);

    let log = events.snapshot();
    let starts: Vec<u64> = log
        .iter()
        .filter_map(|e| match e {
            Event::Started(s) => Some(s.request_id),
            _ => None,
        })
        .collect();
    let completes: Vec<u64> = log
        .iter()
        .filter_map(|e| match e {
            Event::Completed(c) => Some(c.request_id),
            _ => None,
        })
        .collect();

    assert_eq!(starts.len(), 2, "expected 2 starts: {log:?}");
    assert_eq!(completes.len(), 2, "expected 2 completes: {log:?}");

    // Each call's start/complete share a request_id…
    assert_eq!(
        starts, completes,
        "started/completed must share request_id within a single call: {log:?}"
    );
    // …and the two calls don't collide.
    assert_ne!(
        starts[0], starts[1],
        "two independent calls must allocate distinct request_ids: {log:?}"
    );
}
