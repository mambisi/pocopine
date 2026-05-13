#![cfg(all(not(target_arch = "wasm32"), feature = "logging"))]
// `registry_lock` serializes tests that share pocopine-server's process-global
// plugin registry while driving async axum services on a current-thread runtime.
#![allow(clippy::await_holding_lock)]

mod support;

use std::sync::{Mutex, MutexGuard, OnceLock};

use pocopine::{ServerError, ServerResult};
use pocopine_server::axum::body::{to_bytes, Body};
use pocopine_server::axum::http::{Method, Request};
use pocopine_server::axum::routing::get;
use pocopine_server::axum::Router;
use pocopine_server::tower::ServiceExt;
use pocopine_server::Server;
use support::{CapturedEvent, TraceCapture};

fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[pocopine::server(public)]
async fn observed_echo(value: String) -> ServerResult<String> {
    Ok(format!("echo:{value}"))
}

#[pocopine::server(public)]
async fn observed_fail(value: String) -> ServerResult<()> {
    assert_eq!(value, "fail-secret");
    Err(ServerError::App("boom".into()))
}

fn config() -> pocopine::logging::ServerObservabilityConfig {
    pocopine::logging::ServerObservabilityConfig::new()
        .with_service("server-observability-test")
        .with_environment("test")
}

fn finalize(router: Router) -> Router {
    Server::new(router)
        .plugin(pocopine::logging::server_observability_with_config(config()))
        .try_finalize()
        .expect("server observability plugin should validate")
}

async fn send(
    router: &Router,
    request: Request<Body>,
) -> pocopine_server::axum::response::Response {
    router
        .clone()
        .oneshot(request)
        .await
        .expect("oneshot dispatch")
}

async fn response_body(response: pocopine_server::axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    String::from_utf8(bytes.to_vec()).expect("utf8 response")
}

#[test]
fn server_observability_http_events_use_route_patterns_and_redact_request_details() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    capture.run(|| {
        rt.block_on(async {
            let router = finalize(Router::new().route("/accounts/:id", get(|| async { "ok" })));

            let response = send(
                &router,
                Request::builder()
                    .method(Method::GET)
                    .uri("/accounts/path-secret?api_key=query-secret")
                    .header("authorization", "Bearer auth-secret")
                    .header("cookie", "session=cookie-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

            assert_eq!(response.status(), 200);
            assert_eq!(response_body(response).await, "ok");
        });
    });

    let events = capture.events();
    let started = find_observed_event(&events, "pocopine.trace", "http_request_started");
    let completed = find_observed_event(&events, "pocopine.trace", "http_request_completed");

    assert_eq!(
        request_id(started),
        request_id(completed),
        "HTTP start/completed events should share request_id"
    );

    assert!(fields(started).contains("/accounts/:id"));
    assert!(fields(completed).contains("/accounts/:id"));
    assert!(context(started).contains("server-observability-test"));
    assert!(context(started).contains("/accounts/:id"));
    assert_no_values_contain(
        &events,
        &[
            "path-secret",
            "query-secret",
            "auth-secret",
            "cookie-secret",
        ],
    );
}

#[test]
fn server_observability_correlates_http_and_server_function_events_without_payload_leaks() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    capture.run(|| {
        rt.block_on(async {
            let router = finalize(Router::new());
            let response = send(
                &router,
                Request::builder()
                    .method(Method::POST)
                    .uri(__observed_echo_path())
                    .header("content-type", "application/json")
                    .body(Body::from("[\"payload-secret\"]"))
                    .unwrap(),
            )
            .await;

            assert_eq!(response.status(), 200);
            assert!(
                response_body(response).await.contains("payload-secret"),
                "response sanity check should prove the handler saw the payload"
            );
        });
    });

    let events = capture.events();
    let http_started = find_observed_event(&events, "pocopine.trace", "http_request_started");
    let http_completed = find_observed_event(&events, "pocopine.trace", "http_request_completed");
    let function_started =
        find_observed_event(&events, "pocopine.trace", "server_function_started");
    let function_completed =
        find_observed_event(&events, "pocopine.trace", "server_function_completed");

    let shared_request_id = request_id(http_started);
    assert_eq!(request_id(http_completed), shared_request_id);
    assert_eq!(request_id(function_started), shared_request_id);
    assert_eq!(request_id(function_completed), shared_request_id);
    assert!(fields(function_started).contains("observed_echo"));
    assert!(fields(function_started).contains(__observed_echo_function_path()));
    assert!(fields(function_completed).contains("observed_echo"));
    assert!(fields(function_completed).contains(__observed_echo_function_path()));
    assert_no_values_contain(&events, &["payload-secret"]);
}

#[test]
fn server_observability_logs_server_function_rejections_and_failures_without_payloads() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    capture.run(|| {
        rt.block_on(async {
            let router = finalize(Router::new());

            let rejected = send(
                &router,
                Request::builder()
                    .method(Method::POST)
                    .uri(__observed_echo_path())
                    .header("content-type", "application/json")
                    .body(Body::from("\"reject-secret\""))
                    .unwrap(),
            )
            .await;
            assert_eq!(rejected.status(), 200);

            let failed = send(
                &router,
                Request::builder()
                    .method(Method::POST)
                    .uri(__observed_fail_path())
                    .header("content-type", "application/json")
                    .body(Body::from("[\"fail-secret\"]"))
                    .unwrap(),
            )
            .await;
            assert_eq!(failed.status(), 200);
        });
    });

    let events = capture.events();
    let rejected = find_observed_event(&events, "pocopine.log", "server_function_rejected");
    let failed = find_observed_event(&events, "pocopine.log", "server_function_failed");

    assert!(fields(rejected).contains("observed_echo"));
    assert!(fields(rejected).contains(__observed_echo_function_path()));
    assert!(fields(rejected).contains("bad_request"));
    assert!(fields(rejected).contains("U64(400)"));
    assert!(fields(failed).contains("observed_fail"));
    assert!(fields(failed).contains(__observed_fail_function_path()));
    assert!(fields(failed).contains("app"));
    assert_no_values_contain(&events, &["reject-secret", "fail-secret"]);
}

#[test]
fn server_observability_emits_boot_failure_for_invalid_address() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    let err = capture.run(|| {
        rt.block_on(async {
            Server::new(Router::new())
                .plugin(pocopine::logging::server_observability_with_config(config()))
                .serve("not an address")
                .await
                .expect_err("invalid address should fail before bind")
        })
    });

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let events = capture.events();
    let started = find_observed_event(&events, "pocopine.trace", "server_boot_started");
    let failed = find_observed_event(&events, "pocopine.log", "server_boot_failed");

    assert!(fields(started).contains("not an address"));
    assert!(fields(failed).contains("address_parse"));
}

fn find_observed_event<'a>(
    events: &'a [CapturedEvent],
    target: &str,
    name: &str,
) -> &'a CapturedEvent {
    events
        .iter()
        .find(|event| event.target == target && event.field("event_name") == Some(name))
        .unwrap_or_else(|| {
            panic!("missing observed event `{name}` on target `{target}`: {events:#?}")
        })
}

fn fields(event: &CapturedEvent) -> String {
    if let Some(fields) = event.field("fields") {
        return fields.to_owned();
    }

    let slots = event
        .field("observed_field_slots")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut rendered = String::new();
    for index in 0..slots {
        if event.field(&format!("observed_field_{index}_present")) != Some("true") {
            continue;
        }
        let name = event
            .field(&format!("observed_field_{index}_name"))
            .unwrap_or("");
        let value = observed_field_value(event, index).unwrap_or_default();
        rendered.push_str(name);
        rendered.push('=');
        rendered.push_str(&value);
        rendered.push(';');
    }
    rendered
}

fn context(event: &CapturedEvent) -> String {
    if let Some(context) = event.field("context") {
        return context.to_owned();
    }

    [
        event.field("observed_context_service"),
        event.field("observed_context_environment"),
        event.field("observed_context_route"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

fn request_id(event: &CapturedEvent) -> u64 {
    let fields = fields(event);
    if let Some(value) = observed_named_field_value(event, "request_id") {
        return value
            .parse()
            .unwrap_or_else(|_| panic!("request_id is not numeric: {fields}"));
    }

    let needle = "\"request_id\": ObservedField { value: U64(";
    let start = fields
        .find(needle)
        .unwrap_or_else(|| panic!("request_id missing from observed fields: {fields}"))
        + needle.len();
    let rest = &fields[start..];
    let end = rest
        .find(')')
        .unwrap_or_else(|| panic!("request_id field is malformed: {fields}"));
    rest[..end]
        .parse()
        .unwrap_or_else(|_| panic!("request_id is not numeric: {fields}"))
}

fn observed_named_field_value(event: &CapturedEvent, name: &str) -> Option<String> {
    let slots = event
        .field("observed_field_slots")
        .and_then(|value| value.parse::<usize>().ok())?;
    for index in 0..slots {
        if event.field(&format!("observed_field_{index}_name")) == Some(name) {
            return observed_field_raw_value(event, index);
        }
    }
    None
}

fn observed_field_value(event: &CapturedEvent, index: usize) -> Option<String> {
    let kind = event.field(&format!("observed_field_{index}_kind"))?;
    let raw = observed_field_raw_value(event, index)?;
    Some(match kind {
        "u64" => format!("U64({raw})"),
        "i64" => format!("I64({raw})"),
        "f64" => format!("F64({raw})"),
        "bool" => format!("Bool({raw})"),
        _ => raw,
    })
}

fn observed_field_raw_value(event: &CapturedEvent, index: usize) -> Option<String> {
    let kind = event.field(&format!("observed_field_{index}_kind"))?;
    let suffix = match kind {
        "u64" => "u64",
        "i64" => "i64",
        "f64" => "f64",
        "bool" => "bool",
        "string" => "string",
        _ => return None,
    };
    event
        .field(&format!("observed_field_{index}_value_{suffix}"))
        .map(ToOwned::to_owned)
}

fn assert_no_values_contain(events: &[CapturedEvent], needles: &[&str]) {
    for event in events {
        for value in event.fields.values() {
            for needle in needles {
                assert!(
                    !value.contains(needle),
                    "captured event leaked `{needle}` in value `{value}`: {event:#?}"
                );
            }
        }
    }
}
