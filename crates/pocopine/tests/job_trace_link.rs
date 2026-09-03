//! RFC-123 Phase 4 — a job run links back to the trace it was enqueued
//! from: the enqueuer's W3C `traceparent` rides the envelope and lands on
//! `pocopine.job.run` (as a field, and as an OpenTelemetry span link). The
//! run stays a root: a job is not a child of the request that queued it.
#![cfg(all(not(target_arch = "wasm32"), feature = "logging-otlp"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use pocopine::JobResult;
use pocopine_observe::test_support::SpanCapture;
use serde::{Deserialize, Serialize};
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::prelude::*;

static APP_COUNTER: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Payload {
    value: String,
}

#[pocopine::job(queue = "linked", retries = 0)]
async fn linked_job(input: Payload) -> JobResult<()> {
    assert_eq!(input.value, "link me");
    Ok(())
}

#[test]
fn job_run_links_to_the_enqueuers_trace() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
    let capture = SpanCapture::new();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")))
        .with(capture.clone());

    let enqueuer_trace_id = tracing::subscriber::with_default(subscriber, || {
        rt.block_on(async {
            let app = format!(
                "job-trace-link-{}-{}",
                std::process::id(),
                APP_COUNTER.fetch_add(1, Ordering::SeqCst)
            );
            let client = pocopine::JobClient::memory(app.clone());

            // Enqueue from inside a span that has an OpenTelemetry context.
            let enqueuer = tracing::info_span!("enqueuer");
            let trace_id = enqueuer
                .context()
                .span()
                .span_context()
                .trace_id()
                .to_string();
            linked_job_job::enqueue_with(
                &client,
                Payload {
                    value: "link me".into(),
                },
            )
            .instrument(enqueuer)
            .await
            .unwrap();

            let worker = pocopine::Worker::new(pocopine::WorkerConfig {
                backend: pocopine::JobBackend::Memory,
                app,
                queues: vec!["linked".to_string()],
                group: "test".to_string(),
                consumer: "test".to_string(),
                block_ms: 0,
                visibility_timeout: Duration::from_secs(60),
                scheduler_interval: Duration::from_millis(1),
                max_periodic_catch_up: 16,
                batch_size: 10,
            })
            .unwrap();
            assert_eq!(worker.run_once().await.unwrap(), 1);
            trace_id
        })
    });

    let run = capture
        .spans_named("pocopine.job.run")
        .into_iter()
        .find(|span| span.field("pocopine.job.queue") == Some("linked"))
        .expect("job.run span for the linked queue");
    assert_eq!(run.parent, None, "a job run is still a root");
    let trace_parent = run
        .field("pocopine.job.enqueue_traceparent")
        .expect("the enqueuer's traceparent landed on the run span");
    assert!(
        trace_parent.starts_with(&format!("00-{enqueuer_trace_id}-")),
        "{trace_parent} should carry the enqueuer's trace id {enqueuer_trace_id}"
    );
    assert_eq!(run.field("otel.status_code"), Some("OK"));
}
