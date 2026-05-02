use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pocopine::JobResult;
use serde::{Deserialize, Serialize};

static MEMORY_APP_COUNTER: AtomicUsize = AtomicUsize::new(1);
static MEMORY_JOB_RUNS: AtomicUsize = AtomicUsize::new(0);

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
