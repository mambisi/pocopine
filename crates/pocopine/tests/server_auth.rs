// `#[pocopine::server(guard = ...)]` and the `protected!` macro only
// emit the route helper, guard function, and `RequestContext` plumbing
// on the host target; the umbrella `pocopine` crate also gates the
// `RequestContext` re-export to non-wasm. Skip the whole file under
// wasm.
#![cfg(not(target_arch = "wasm32"))]

mod support;

use pocopine::{AuthUser, Role, ServerError, ServerResult};
use pocopine_server::axum::{
    body::{Body, to_bytes},
    http::{Extensions, HeaderMap, Method, Request, Uri},
};
use pocopine_server::tower::ServiceExt;
use support::TraceCapture;

async fn require_token(ctx: pocopine_server::auth::RequestContext) -> ServerResult<()> {
    match ctx.bearer_token() {
        Some("test-token") => Ok(()),
        Some(_) => Err(ServerError::forbidden("invalid bearer token")),
        None => Err(ServerError::unauthorized("missing bearer token")),
    }
}

#[pocopine::server(guard = require_token)]
async fn guarded_echo(value: String) -> ServerResult<String> {
    Ok(value)
}

#[pocopine::server(public)]
async fn public_echo(value: String) -> ServerResult<String> {
    Ok(value)
}

#[pocopine::server(public)]
async fn failing_echo(value: String) -> ServerResult<String> {
    assert_eq!(value, "fail-input");
    Err(ServerError::from("app failure"))
}

pocopine::protected! {
    require |ctx| ctx.user.has_role(&Role::admin());

    async fn admin_echo(value: String) -> ServerResult<String> {
        Ok(value)
    }
}

#[test]
fn guarded_and_public_route_helpers_typecheck() {
    let router = pocopine_server::axum::Router::new();
    let router = __guarded_echo_route(router);
    let router = __public_echo_route(router);
    let router = __failing_echo_route(router);
    let _router = __admin_echo_route(router);
}

#[test]
fn request_context_extracts_user_from_extensions() {
    let mut extensions = Extensions::new();
    extensions.insert(AuthUser::new("user-1").with_role(Role::admin()));

    let ctx = pocopine::auth::RequestContext::from_parts(
        Method::GET,
        Uri::from_static(__admin_echo_path()),
        HeaderMap::new(),
        extensions,
    );

    assert!(ctx.user.is_authenticated());
    assert!(ctx.user.has_role(&Role::admin()));
}

#[test]
fn protected_macro_guard_checks_inline_policy() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let allowed = pocopine::auth::RequestContext::new(
        Method::POST,
        Uri::from_static(__admin_echo_path()),
        HeaderMap::new(),
    )
    .with_user(AuthUser::new("admin").with_role(Role::admin()));
    rt.block_on(__pocopine_guard_admin_echo(allowed)).unwrap();

    let denied = pocopine::auth::RequestContext::new(
        Method::POST,
        Uri::from_static(__admin_echo_path()),
        HeaderMap::new(),
    )
    .with_user(AuthUser::new("user").with_role(Role::user()));
    let err = rt
        .block_on(__pocopine_guard_admin_echo(denied))
        .unwrap_err();
    assert!(matches!(err, ServerError::Forbidden(_)));
}

#[test]
fn oversized_body_returns_bad_request() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    let result = capture.run(|| {
        rt.block_on(async {
            let router = __public_echo_route(pocopine_server::axum::Router::new());
            let mut oversized_body = b"oversized-secret".to_vec();
            oversized_body.resize(
                pocopine_server::DEFAULT_SERVER_FUNCTION_BODY_LIMIT + 1,
                b'a',
            );
            let response = router
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(__public_echo_path())
                        .header("content-type", "application/json")
                        .body(Body::from(oversized_body))
                        .unwrap(),
                )
                .await
                .unwrap();

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            serde_json::from_slice::<ServerResult<String>>(&bytes).unwrap()
        })
    });

    assert!(matches!(result, Err(ServerError::BadRequest(_))));

    let events =
        capture.events_with_message("pocopine.log", "server function request body read failed");
    let event = events
        .iter()
        .find(|event| event.field("route") == Some(__public_echo_path()))
        .expect("body-read failure event should be captured");

    assert_eq!(event.field("function"), Some("public_echo"));
    assert_eq!(event.field("error_kind"), Some("bad_request"));
    assert!(
        event
            .field("duration_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "duration_ms should be a numeric tracing field"
    );
    assert!(
        event
            .fields
            .values()
            .all(|value| !value.contains("oversized-secret")),
        "body-read failure logs must not include raw request payloads"
    );
}

#[test]
fn malformed_body_returns_bad_request_and_emits_parse_log_without_payload() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    let result = capture.run(|| {
        rt.block_on(async {
            let router = __public_echo_route(pocopine_server::axum::Router::new());
            let response = router
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(__public_echo_path())
                        .header("content-type", "application/json")
                        .body(Body::from("\"parse-secret\""))
                        .unwrap(),
                )
                .await
                .unwrap();

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            serde_json::from_slice::<ServerResult<String>>(&bytes).unwrap()
        })
    });

    assert!(matches!(result, Err(ServerError::BadRequest(_))));

    let events =
        capture.events_with_message("pocopine.log", "server function request body parse failed");
    let event = events
        .iter()
        .find(|event| event.field("route") == Some(__public_echo_path()))
        .expect("body-parse failure event should be captured");

    assert_eq!(event.field("function"), Some("public_echo"));
    assert_eq!(event.field("body_bytes"), Some("14"));
    assert_eq!(event.field("error_kind"), Some("bad_request"));
    assert_eq!(event.field("error"), Some("request body parse failed"));
    assert!(
        event
            .field("duration_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "duration_ms should be a numeric tracing field"
    );
    assert!(
        event
            .fields
            .values()
            .all(|value| !value.contains("parse-secret")),
        "body-parse failure logs must not include raw request payloads"
    );
}

#[test]
fn server_function_route_emits_trace_without_payload() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    let result = capture.run(|| {
        rt.block_on(async {
            let router = __public_echo_route(pocopine_server::axum::Router::new());
            let response = router
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(__public_echo_path())
                        .header("content-type", "application/json")
                        .body(Body::from("[\"hello\"]"))
                        .unwrap(),
                )
                .await
                .unwrap();

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            serde_json::from_slice::<ServerResult<String>>(&bytes).unwrap()
        })
    });

    assert_eq!(result.unwrap(), "hello");

    let events = capture.events_with_message("pocopine.trace", "server function completed");
    let event = events
        .iter()
        .find(|event| event.field("route") == Some(__public_echo_path()))
        .expect("server function completion event should be captured");

    assert_eq!(event.field("function"), Some("public_echo"));
    assert_eq!(event.field("body_bytes"), Some("9"));
    assert!(
        event
            .field("duration_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "duration_ms should be a numeric tracing field"
    );
    assert!(
        event.fields.values().all(|value| !value.contains("hello")),
        "server-function logs must not include raw request payloads"
    );
}

#[test]
fn server_function_guard_rejection_emits_log_without_payload() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    let result = capture.run(|| {
        rt.block_on(async {
            let router = __guarded_echo_route(pocopine_server::axum::Router::new());
            let response = router
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(__guarded_echo_path())
                        .header("content-type", "application/json")
                        .body(Body::from("[\"guard-secret\"]"))
                        .unwrap(),
                )
                .await
                .unwrap();

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            serde_json::from_slice::<ServerResult<String>>(&bytes).unwrap()
        })
    });

    assert!(matches!(result, Err(ServerError::Unauthorized(_))));

    let events =
        capture.events_with_message("pocopine.log", "server function guard rejected request");
    let event = events
        .iter()
        .find(|event| event.field("route") == Some(__guarded_echo_path()))
        .expect("guard rejection event should be captured");

    assert_eq!(event.field("function"), Some("guarded_echo"));
    assert_eq!(event.field("error_kind"), Some("unauthorized"));
    assert!(
        event
            .field("duration_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "duration_ms should be a numeric tracing field"
    );
    assert!(
        event
            .fields
            .values()
            .all(|value| !value.contains("guard-secret")),
        "guard rejection logs must not include raw request payloads"
    );
}

#[test]
fn server_function_app_failure_emits_log_without_payload() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    let result = capture.run(|| {
        rt.block_on(async {
            let router = __failing_echo_route(pocopine_server::axum::Router::new());
            let response = router
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(__failing_echo_path())
                        .header("content-type", "application/json")
                        .body(Body::from("[\"fail-input\"]"))
                        .unwrap(),
                )
                .await
                .unwrap();

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            serde_json::from_slice::<ServerResult<String>>(&bytes).unwrap()
        })
    });

    assert!(matches!(result, Err(ServerError::App(_))));

    let events = capture.events_with_message("pocopine.log", "server function failed");
    let event = events
        .iter()
        .find(|event| event.field("route") == Some(__failing_echo_path()))
        .expect("server function failure event should be captured");

    assert_eq!(event.field("function"), Some("failing_echo"));
    assert_eq!(event.field("body_bytes"), Some("14"));
    assert_eq!(event.field("error_kind"), Some("app"));
    assert!(
        event
            .field("duration_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "duration_ms should be a numeric tracing field"
    );
    assert!(
        event
            .fields
            .values()
            .all(|value| !value.contains("fail-input")),
        "server-function failure logs must not include raw request payloads"
    );
}
