// `#[pocopine::job]` only generates the host-side worker, client, and
// dispatch helpers; on `wasm32` the macro emits stubs and the
// `<fn>_job` modules don't exist. Skip the whole file under wasm.
#![cfg(not(target_arch = "wasm32"))]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pocopine::JobResult;
use serde::{Deserialize, Serialize};
use support::TraceCapture;

static MEMORY_APP_COUNTER: AtomicUsize = AtomicUsize::new(1);
static MEMORY_JOB_RUNS: AtomicUsize = AtomicUsize::new(0);
static RETRY_JOB_RUNS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JobPayload {
    value: String,
}

#[pocopine::job(queue = "tests", retries = 2)]
async fn macro_registered_job(input: JobPayload) -> JobResult<()> {
    assert_eq!(input.value, "ok");
    Ok(())
}

#[pocopine::job(queue = "memory", retries = 0)]
async fn memory_backend_job(input: JobPayload) -> JobResult<()> {
    assert_eq!(input.value, "memory");
    MEMORY_JOB_RUNS.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[pocopine::job(queue = "trace", retries = 0)]
async fn traced_memory_job(input: JobPayload) -> JobResult<()> {
    assert_eq!(input.value, "trace-payload");
    Ok(())
}

#[pocopine::job(queue = "retry", retries = 1)]
async fn retrying_memory_job(input: JobPayload) -> JobResult<()> {
    assert_eq!(input.value, "retry-payload");
    RETRY_JOB_RUNS.fetch_add(1, Ordering::SeqCst);
    Err(pocopine::JobError::Handler("retry failure".into()))
}

#[pocopine::job(queue = "panic", retries = 0)]
async fn panicking_memory_job(_input: JobPayload) -> JobResult<()> {
    panic!("intentional panic for memory backend test");
}

#[pocopine::job(queue = "periodic", every = "15m", retries = 1)]
async fn refresh_periodic_cache() -> JobResult<()> {
    Ok(())
}

#[pocopine::job(queue = "periodic", cron = "0 0 2 * * * *")]
async fn nightly_periodic_cleanup() -> JobResult<()> {
    Ok(())
}

#[test]
fn job_descriptor_is_registered_by_inventory() {
    let descriptor = pocopine::jobs::registered_jobs()
        .find(|descriptor| descriptor.name.ends_with("::macro_registered_job"))
        .expect("job descriptor should be registered");

    assert_eq!(descriptor.queue, "tests");
    assert_eq!(descriptor.retry_policy.max_attempts, 3);
}

#[test]
fn periodic_job_descriptor_is_registered_by_inventory() {
    let descriptor = pocopine::jobs::registered_jobs()
        .find(|descriptor| descriptor.name.ends_with("::refresh_periodic_cache"))
        .expect("periodic job descriptor should be registered");

    assert_eq!(descriptor.queue, "periodic");
    assert_eq!(descriptor.retry_policy.max_attempts, 2);
    assert_eq!(
        descriptor.periodic,
        Some(pocopine::PeriodicSchedule::Every {
            interval_ms: 15 * 60 * 1_000
        })
    );

    let descriptor = pocopine::jobs::registered_jobs()
        .find(|descriptor| descriptor.name.ends_with("::nightly_periodic_cleanup"))
        .expect("cron job descriptor should be registered");

    assert_eq!(
        descriptor.periodic,
        Some(pocopine::PeriodicSchedule::Cron {
            expr: "0 0 2 * * * *"
        })
    );
}

#[test]
fn generated_dispatch_decodes_payload() {
    let payload = serde_json::to_vec(&JobPayload { value: "ok".into() }).unwrap();
    let future = macro_registered_job_job::__pocopine_dispatch_macro_registered_job(payload);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(future).unwrap();
}

#[test]
fn generated_periodic_dispatch_decodes_unit_payload() {
    let payload = serde_json::to_vec(&()).unwrap();
    let future = refresh_periodic_cache_job::__pocopine_dispatch_refresh_periodic_cache(payload);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(future).unwrap();
}

#[test]
fn memory_backend_worker_runs_enqueued_job() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        MEMORY_JOB_RUNS.store(0, Ordering::SeqCst);
        let app = format!(
            "job-macro-memory-{}-{}",
            std::process::id(),
            MEMORY_APP_COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let client = pocopine::JobClient::memory(app.clone());
        memory_backend_job_job::enqueue_with(
            &client,
            JobPayload {
                value: "memory".into(),
            },
        )
        .await
        .unwrap();

        let worker = pocopine::Worker::new(pocopine::WorkerConfig {
            backend: pocopine::JobBackend::Memory,
            app,
            queues: vec!["memory".to_string()],
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
        assert_eq!(worker.run_once().await.unwrap(), 0);
        assert_eq!(MEMORY_JOB_RUNS.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn memory_backend_routes_handler_panic_through_dead_letter() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    capture.run(|| {
        rt.block_on(async {
            let app = format!(
                "job-macro-panic-{}-{}",
                std::process::id(),
                MEMORY_APP_COUNTER.fetch_add(1, Ordering::SeqCst)
            );
            let client = pocopine::JobClient::memory(app.clone());
            panicking_memory_job_job::enqueue_with(
                &client,
                JobPayload {
                    value: "panic".into(),
                },
            )
            .await
            .unwrap();

            let worker = pocopine::Worker::new(pocopine::WorkerConfig {
                backend: pocopine::JobBackend::Memory,
                app,
                queues: vec!["panic".to_string()],
                group: "test".to_string(),
                consumer: "test".to_string(),
                block_ms: 0,
                visibility_timeout: Duration::from_secs(60),
                scheduler_interval: Duration::from_millis(1),
                max_periodic_catch_up: 16,
                batch_size: 10,
            })
            .unwrap();

            // retries = 0 → max_attempts = 1, so a panic on the first attempt
            // dead-letters immediately. The worker must NOT propagate the
            // panic; if catch-unwind were broken, the spawned task would
            // unwind through `run_once` and crash this test.
            assert_eq!(worker.run_once().await.unwrap(), 1);
            let dead = worker.drain_dead_letter().unwrap();
            assert_eq!(dead.len(), 1);
            assert!(
                dead[0].error.contains("panic"),
                "expected panic message, got `{}`",
                dead[0].error
            );
            assert_eq!(dead[0].attempt, 1);
            assert_eq!(dead[0].max_attempts, 1);

            // Drain is single-shot.
            assert!(worker.drain_dead_letter().unwrap().is_empty());
        });
    });

    let events = capture.events_with_message("pocopine.log", "job moved to dead letter");
    let event = events
        .iter()
        .find(|event| event.field("queue") == Some("panic"))
        .expect("dead-letter event should be captured");
    assert_eq!(event.field("backend"), Some("memory"));
    assert_eq!(event.field("attempt"), Some("1"));
    assert_eq!(event.field("max_attempts"), Some("1"));
    assert!(
        event
            .field("error")
            .is_some_and(|value| value.contains("panic")),
        "dead-letter event should include the terminal error class"
    );
}

#[test]
fn memory_backend_emits_job_lifecycle_trace_without_payload() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    capture.run(|| {
        rt.block_on(async {
            let app = format!(
                "job-macro-trace-{}-{}",
                std::process::id(),
                MEMORY_APP_COUNTER.fetch_add(1, Ordering::SeqCst)
            );
            let client = pocopine::JobClient::memory(app.clone());
            traced_memory_job_job::enqueue_with(
                &client,
                JobPayload {
                    value: "trace-payload".into(),
                },
            )
            .await
            .unwrap();

            let worker = pocopine::Worker::new(pocopine::WorkerConfig {
                backend: pocopine::JobBackend::Memory,
                app,
                queues: vec!["trace".to_string()],
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
        });
    });

    let started = capture.events_with_message("pocopine.trace", "job started");
    let started = started
        .iter()
        .find(|event| event.field("queue") == Some("trace"))
        .expect("job-start event should be captured");
    assert_eq!(started.field("backend"), Some("memory"));
    assert!(
        started
            .field("job_name")
            .is_some_and(|value| value.ends_with("::traced_memory_job"))
    );
    assert_eq!(started.field("attempt"), Some("1"));
    assert_eq!(started.field("max_attempts"), Some("1"));

    let completed = capture.events_with_message("pocopine.trace", "job completed");
    let completed = completed
        .iter()
        .find(|event| event.field("queue") == Some("trace"))
        .expect("job-completed event should be captured");
    assert_eq!(completed.field("backend"), Some("memory"));
    assert!(
        completed
            .field("duration_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "duration_ms should be a numeric tracing field"
    );
    assert!(
        started
            .fields
            .values()
            .chain(completed.fields.values())
            .all(|value| !value.contains("trace-payload")),
        "job lifecycle logs must not include raw payloads"
    );

    // RFC-123 §3.3 — both events hang from one `pocopine.job.run` span
    // that carries the attempt's structural fields and closed OK.
    assert_eq!(started.ancestry(), ["pocopine.job.run"]);
    assert_eq!(completed.ancestry(), ["pocopine.job.run"]);
    let span = capture
        .spans_named("pocopine.job.run")
        .into_iter()
        .find(|span| span.field("pocopine.job.queue") == Some("trace"))
        .expect("job.run span for the traced queue");
    assert_eq!(span.target, "pocopine.trace");
    assert_eq!(span.field("otel.kind"), Some("consumer"));
    assert_eq!(span.field("pocopine.job.backend"), Some("memory"));
    assert_eq!(span.field("pocopine.job.attempt"), Some("1"));
    assert_eq!(span.field("pocopine.job.max_attempts"), Some("1"));
    assert_eq!(span.field("otel.status_code"), Some("OK"));
    assert_eq!(span.field("error.type"), None);
    assert!(
        span.field("pocopine.job.name")
            .is_some_and(|value| value.ends_with("::traced_memory_job"))
    );
    assert!(
        !span
            .fields
            .values()
            .any(|value| value.contains("trace-payload")),
        "the span carries structural fields only"
    );
}

#[test]
fn memory_backend_retry_emits_schedule_log_without_payload() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = TraceCapture::new();

    capture.run(|| {
        rt.block_on(async {
            RETRY_JOB_RUNS.store(0, Ordering::SeqCst);
            let app = format!(
                "job-macro-retry-{}-{}",
                std::process::id(),
                MEMORY_APP_COUNTER.fetch_add(1, Ordering::SeqCst)
            );
            let client = pocopine::JobClient::memory(app.clone());
            retrying_memory_job_job::enqueue_with(
                &client,
                JobPayload {
                    value: "retry-payload".into(),
                },
            )
            .await
            .unwrap();

            let worker = pocopine::Worker::new(pocopine::WorkerConfig {
                backend: pocopine::JobBackend::Memory,
                app,
                queues: vec!["retry".to_string()],
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
            assert_eq!(RETRY_JOB_RUNS.load(Ordering::SeqCst), 1);
            assert!(worker.drain_dead_letter().unwrap().is_empty());
        });
    });

    let events = capture.events_with_message("pocopine.log", "job retry scheduled");
    let event = events
        .iter()
        .find(|event| event.field("queue") == Some("retry"))
        .expect("retry-scheduled event should be captured");
    assert_eq!(event.field("backend"), Some("memory"));
    assert!(
        event
            .field("job_name")
            .is_some_and(|value| value.ends_with("::retrying_memory_job"))
    );
    assert_eq!(event.field("attempt"), Some("1"));
    assert_eq!(event.field("next_attempt"), Some("2"));
    assert_eq!(event.field("max_attempts"), Some("2"));
    assert!(
        event
            .field("due_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "due_ms should be a numeric tracing field"
    );
    assert!(
        event
            .field("duration_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "duration_ms should be a numeric tracing field"
    );
    assert!(
        event
            .field("error")
            .is_some_and(|value| value.contains("retry failure")),
        "retry event should include the retry cause"
    );
    assert!(
        event
            .fields
            .values()
            .all(|value| !value.contains("retry-payload")),
        "retry logs must not include raw job payloads"
    );
}
