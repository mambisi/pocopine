//! RFC-123 §5.3 — with the `otel` feature, the request span adopts an
//! incoming W3C `traceparent` as its remote parent and echoes its own
//! context back on the response. No exporter is needed: an in-process
//! tracer provider is enough to mint valid span contexts.

#![cfg(all(not(target_arch = "wasm32"), feature = "otel"))]
#![allow(clippy::await_holding_lock)]

use std::sync::{Mutex, MutexGuard, OnceLock};

use pocopine_server::axum::Router;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_server::axum::routing::get;
use pocopine_server::tower::ServiceExt;
use pocopine_server::{RequestEventOptions, Server, request_event_layer_with};
use tracing_subscriber::prelude::*;

fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

const INCOMING: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

fn with_otel<T>(f: impl FnOnce() -> T) -> T {
    use opentelemetry::trace::TracerProvider as _;
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
    let layer = tracing_opentelemetry::layer().with_tracer(provider.tracer("test"));
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, f)
}

fn router(options: RequestEventOptions) -> Router {
    Server::new(Router::new().route("/ping", get(|| async { StatusCode::NO_CONTENT })))
        .layer(request_event_layer_with(options))
        .try_finalize()
        .expect("finalize")
}

async fn traceparent_of(router: Router, incoming: Option<&str>) -> Option<String> {
    let mut request = Request::builder().uri("/ping");
    if let Some(value) = incoming {
        request = request.header("traceparent", value);
    }
    let response = router
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    response
        .headers()
        .get("traceparent")
        .map(|v| v.to_str().unwrap().to_owned())
}

#[test]
fn request_span_adopts_incoming_traceparent_and_echoes_its_own() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let echoed = with_otel(|| {
        rt.block_on(traceparent_of(
            router(RequestEventOptions::default()),
            Some(INCOMING),
        ))
    })
    .expect("response carries traceparent");

    let parts: Vec<&str> = echoed.split('-').collect();
    assert_eq!(parts.len(), 4, "{echoed}");
    assert_eq!(parts[0], "00");
    assert_eq!(
        parts[1], "4bf92f3577b34da6a3ce929d0e0e4736",
        "same trace as the caller"
    );
    assert_ne!(
        parts[2], "00f067aa0ba902b7",
        "the echoed span id is the request span's own, not the caller's"
    );
    assert_eq!(parts[2].len(), 16);
}

#[test]
fn incoming_traceparent_is_ignored_when_opted_out() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let echoed = with_otel(|| {
        rt.block_on(traceparent_of(
            router(RequestEventOptions::new().with_accept_trace_context(false)),
            Some(INCOMING),
        ))
    })
    .expect("response carries traceparent");
    let trace_id = echoed.split('-').nth(1).unwrap();
    assert_ne!(
        trace_id, "4bf92f3577b34da6a3ce929d0e0e4736",
        "fresh root trace"
    );
    assert_ne!(trace_id, "00000000000000000000000000000000");
}

#[test]
fn without_incoming_context_the_request_is_a_fresh_root() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let echoed =
        with_otel(|| rt.block_on(traceparent_of(router(RequestEventOptions::default()), None)))
            .expect("response carries traceparent");
    let trace_id = echoed.split('-').nth(1).unwrap();
    assert_eq!(trace_id.len(), 32);
    assert_ne!(trace_id, "00000000000000000000000000000000");
}

#[test]
fn echoed_traceparent_can_be_disabled() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let echoed = with_otel(|| {
        rt.block_on(traceparent_of(
            router(RequestEventOptions::new().with_trace_context_header(false)),
            Some(INCOMING),
        ))
    });
    assert_eq!(
        echoed, None,
        "no traceparent on the response when opted out"
    );
}
