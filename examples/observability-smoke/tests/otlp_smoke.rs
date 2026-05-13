#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use observability_smoke::router;
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, KeyValue};
use opentelemetry_proto::tonic::trace::v1::Span;
use pocopine::logging::{init_server_logging, OtlpConfig, ServerLoggingConfig};
use pocopine_server::axum::body::{to_bytes, Body};
use pocopine_server::axum::http::{Method, Request};
use pocopine_server::tower::ServiceExt;
use pocopine_server::Server;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Response, Status};

const PRIVATE_PAYLOAD: &str = "raw-private-ci-payload";
const REQUEST_COUNT: usize = 32;
const MAX_P95_MS: u128 = 250;

#[derive(Clone, Default)]
struct CaptureTraceService {
    requests: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
}

#[tonic::async_trait]
impl TraceService for CaptureTraceService {
    async fn export(
        &self,
        request: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        self.requests.lock().unwrap().push(request.into_inner());
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_smoke_exports_expected_spans_without_payload_leak() {
    let collector = start_collector().await;

    std::env::set_var("OTEL_BSP_SCHEDULE_DELAY", "100");
    std::env::set_var("OTEL_BSP_EXPORT_TIMEOUT", "2000");

    init_server_logging(
        ServerLoggingConfig::json()
            .with_env_filter("info,pocopine=debug")
            .with_otlp(
                OtlpConfig::grpc(format!("http://{}", collector.addr))
                    .with_service_name("pocopine-otlp-smoke-ci")
                    .with_timeout(Duration::from_secs(2)),
            ),
    )
    .expect("install OTLP logging subscriber");

    let mut latencies = Vec::with_capacity(REQUEST_COUNT);
    for index in 0..REQUEST_COUNT {
        let started = Instant::now();
        let body = call_observe_echo(PRIVATE_PAYLOAD, index).await;
        latencies.push(started.elapsed());
        assert!(
            body.contains(r#""Ok":{"message_len":22,"accepted":true}"#),
            "unexpected response body: {body}"
        );
    }

    assert_latency_budget(&latencies);

    let spans = wait_for_spans(&collector.capture, REQUEST_COUNT * 2).await;
    let server_span = span_named(&spans, "pocopine.server_function");
    let echo_span = span_named(&spans, "observability_smoke.echo");

    assert_eq!(
        server_span.attrs.get("function").map(String::as_str),
        Some("observe_echo")
    );
    assert_eq!(
        server_span.attrs.get("route").map(String::as_str),
        Some("/_pocopine/observe_echo")
    );
    assert_eq!(
        echo_span.attrs.get("message_len").map(String::as_str),
        Some("22")
    );
    assert!(
        spans
            .iter()
            .any(|span| span.resource.get("service.name").map(String::as_str)
                == Some("pocopine-otlp-smoke-ci")),
        "missing service.name resource attribute in exported spans: {spans:#?}"
    );

    let exported = format!("{:?}", collector.capture.requests.lock().unwrap());
    assert!(
        !exported.contains(PRIVATE_PAYLOAD),
        "raw request payload leaked into exported telemetry"
    );

    let _keep_collector_alive_until_test_shutdown = collector.handle;
}

struct TestCollector {
    addr: SocketAddr,
    capture: CaptureTraceService,
    handle: tokio::task::JoinHandle<()>,
}

async fn start_collector() -> TestCollector {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake OTLP collector");
    let addr = listener.local_addr().expect("fake collector local addr");
    let capture = CaptureTraceService::default();
    let service = TraceServiceServer::new(capture.clone());
    let incoming = TcpListenerStream::new(listener);
    let handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(incoming)
            .await
            .expect("fake OTLP collector");
    });

    TestCollector {
        addr,
        capture,
        handle,
    }
}

async fn call_observe_echo(message: &str, index: usize) -> String {
    let request_body = serde_json::json!([{ "message": message }]).to_string();
    let app = Server::new(router())
        .try_finalize()
        .expect("server-function routes should finalize");
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/_pocopine/observe_echo")
                .header("content-type", "application/json")
                .header("x-request-index", index.to_string())
                .body(Body::from(request_body))
                .unwrap(),
        )
        .await
        .expect("server-function route response");

    assert!(
        response.status().is_success(),
        "unexpected status: {}",
        response.status()
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    String::from_utf8(bytes.to_vec()).expect("response body is utf-8")
}

fn assert_latency_budget(latencies: &[Duration]) {
    let mut millis = latencies
        .iter()
        .map(Duration::as_millis)
        .collect::<Vec<_>>();
    millis.sort_unstable();
    let p95_index = ((millis.len() * 95).div_ceil(100)).saturating_sub(1);
    let p95 = millis[p95_index];

    assert!(
        p95 <= MAX_P95_MS,
        "observability smoke p95 latency {p95}ms exceeded {MAX_P95_MS}ms; all latencies: {millis:?}"
    );
}

async fn wait_for_spans(capture: &CaptureTraceService, min_spans: usize) -> Vec<CapturedSpan> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let spans = flatten_spans(&capture.requests.lock().unwrap());
        let has_server = spans
            .iter()
            .any(|span| span.name == "pocopine.server_function");
        let has_echo = spans
            .iter()
            .any(|span| span.name == "observability_smoke.echo");

        if spans.len() >= min_spans && has_server && has_echo {
            return spans;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for exported spans; got {spans:#?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[derive(Clone, Debug)]
struct CapturedSpan {
    name: String,
    attrs: BTreeMap<String, String>,
    resource: BTreeMap<String, String>,
}

fn flatten_spans(requests: &[ExportTraceServiceRequest]) -> Vec<CapturedSpan> {
    let mut spans = Vec::new();
    for request in requests {
        for resource_spans in &request.resource_spans {
            let resource = resource_spans
                .resource
                .as_ref()
                .map(|resource| key_values(&resource.attributes))
                .unwrap_or_default();

            for scope_spans in &resource_spans.scope_spans {
                for span in &scope_spans.spans {
                    spans.push(captured_span(span, resource.clone()));
                }
            }
        }
    }
    spans
}

fn captured_span(span: &Span, resource: BTreeMap<String, String>) -> CapturedSpan {
    CapturedSpan {
        name: span.name.clone(),
        attrs: key_values(&span.attributes),
        resource,
    }
}

fn span_named<'a>(spans: &'a [CapturedSpan], name: &str) -> &'a CapturedSpan {
    spans
        .iter()
        .find(|span| span.name == name)
        .unwrap_or_else(|| panic!("missing span {name}; exported spans: {spans:#?}"))
}

fn key_values(attrs: &[KeyValue]) -> BTreeMap<String, String> {
    attrs
        .iter()
        .filter_map(|attr| {
            attr.value
                .as_ref()
                .map(|value| (attr.key.clone(), any_value_to_string(value)))
        })
        .collect()
}

fn any_value_to_string(value: &AnyValue) -> String {
    match value.value.as_ref() {
        Some(any_value::Value::StringValue(value)) => value.clone(),
        Some(any_value::Value::BoolValue(value)) => value.to_string(),
        Some(any_value::Value::IntValue(value)) => value.to_string(),
        Some(any_value::Value::DoubleValue(value)) => value.to_string(),
        Some(any_value::Value::ArrayValue(value)) => format!("{value:?}"),
        Some(any_value::Value::KvlistValue(value)) => format!("{value:?}"),
        Some(any_value::Value::BytesValue(value)) => format!("{value:?}"),
        None => String::new(),
    }
}
