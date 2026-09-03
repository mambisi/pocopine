//! RFC-123 §5.5 — with OTLP on, a relayed browser span is re-emitted
//! through the global tracer provider under the client's own trace id,
//! span id, and parent, so the backend renders one tree from the browser
//! down.
#![cfg(all(not(target_arch = "wasm32"), feature = "otlp"))]

use std::collections::BTreeMap;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use pocopine_logging::client_relay;
use pocopine_observe::client_relay::ClientSpanRecord;

#[test]
fn relayed_span_keeps_the_clients_ids() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    let _keep = provider.tracer("keep");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1_000.0;
    let mut fields = BTreeMap::new();
    fields.insert(
        "session.id".to_owned(),
        "7f3a9c1e5b2d4f60a8c1e2d3f4a5b6c7".to_owned(),
    );
    fields.insert("http.route".to_owned(), "/api/summarize".to_owned());
    fields.insert("http.response.status_code".to_owned(), "200".to_owned());
    fields.insert("otel.status_code".to_owned(), "OK".to_owned());
    let record = ClientSpanRecord {
        name: "pocopine.client.server_function".to_owned(),
        trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".to_owned(),
        span_id: "c1f2e3d4a5b6c7d8".to_owned(),
        parent_span_id: Some("a1b2c3d4e5f60718".to_owned()),
        start_unix_ms: now - 500.0,
        end_unix_ms: now,
        fields,
    };
    client_relay::accept(vec![record]).expect("accepted");

    let spans = exporter.get_finished_spans().expect("exported spans");
    let span = spans
        .iter()
        .find(|s| s.name == "pocopine.client.server_function")
        .unwrap_or_else(|| panic!("client span exported: {spans:?}"));
    assert_eq!(
        span.span_context.trace_id().to_string(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
    assert_eq!(span.span_context.span_id().to_string(), "c1f2e3d4a5b6c7d8");
    assert_eq!(span.parent_span_id.to_string(), "a1b2c3d4e5f60718");
    assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Client);
    let attrs: BTreeMap<String, String> = span
        .attributes
        .iter()
        .map(|kv| (kv.key.to_string(), kv.value.to_string()))
        .collect();
    assert_eq!(
        attrs.get("http.route").map(String::as_str),
        Some("/api/summarize")
    );
    assert_eq!(
        attrs.get("http.response.status_code").map(String::as_str),
        Some("200")
    );
    assert!(span.end_time > span.start_time);
}
