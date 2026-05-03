// Redis-backed integration tests for `#[pocopine::job]`. Each test
// targets a throwaway Redis container started by testcontainers, so CI
// runs them without any external service.
//
// Requires a working Docker daemon. Local contributors without Docker
// can skip these with `cargo test --workspace -- --skip jobs_redis`.
//
// Host-only.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pocopine::{JobBackend, JobClient, JobError, JobResult, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage};
use tokio::sync::OnceCell;

// `testcontainers-modules`'s default `Redis` image is pinned to 5.0,
// which predates `XAUTOCLAIM` (Redis 6.2). The job worker uses
// `XAUTOCLAIM` in its stale-job reclaim path, so we pin a modern tag
// here via `GenericImage`.
const REDIS_IMAGE: &str = "redis";
const REDIS_TAG: &str = "7-alpine";
const REDIS_PORT: u16 = 6379;

// One Redis container is shared by every test in this binary. Tests
// keep state isolated by using a unique `app` namespace, so all keys
// live under `pocopine:{app}:*` and cannot collide across tests.
static REDIS: OnceCell<ContainerAsync<GenericImage>> = OnceCell::const_new();
static APP_COUNTER: AtomicUsize = AtomicUsize::new(0);

async fn redis_url() -> String {
    let container = REDIS
        .get_or_init(|| async {
            GenericImage::new(REDIS_IMAGE, REDIS_TAG)
                .with_exposed_port(ContainerPort::Tcp(REDIS_PORT))
                .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
                .start()
                .await
                .expect("start redis testcontainer")
        })
        .await;
    let port = container
        .get_host_port_ipv4(REDIS_PORT)
        .await
        .expect("redis container port");
    format!("redis://127.0.0.1:{port}/")
}

fn unique_app(prefix: &str) -> String {
    format!(
        "pocopine-it-{prefix}-{}-{}",
        std::process::id(),
        APP_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

fn worker_config(backend: JobBackend, app: String, queue: &str) -> WorkerConfig {
    WorkerConfig {
        backend,
        app,
        queues: vec![queue.to_string()],
        group: "test".to_string(),
        consumer: format!("test-consumer-{}", std::process::id()),
        block_ms: 0,
        visibility_timeout: Duration::from_secs(60),
        scheduler_interval: Duration::from_millis(1),
        max_periodic_catch_up: 16,
        batch_size: 10,
    }
}

async fn redis_xlen(url: &str, key: &str) -> i64 {
    let client = redis::Client::open(url).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    redis::cmd("XLEN")
        .arg(key)
        .query_async(&mut conn)
        .await
        .unwrap()
}

async fn redis_zcard(url: &str, key: &str) -> i64 {
    let client = redis::Client::open(url).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    redis::cmd("ZCARD")
        .arg(key)
        .query_async(&mut conn)
        .await
        .unwrap()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Payload {
    value: String,
}

// Run-counters are global because `inventory::submit!` registers
// handlers crate-wide. Each test isolates its own counter via the
// `app` namespace + a per-test atomic that the handler reads/writes.
static HAPPY_RUNS: AtomicUsize = AtomicUsize::new(0);
static FLAKY_RUNS: AtomicUsize = AtomicUsize::new(0);
static FLAKY_FAIL_UNTIL: AtomicUsize = AtomicUsize::new(0);
static ALWAYS_FAILING_RUNS: AtomicUsize = AtomicUsize::new(0);
static PANIC_RUNS: AtomicUsize = AtomicUsize::new(0);

#[pocopine::job(queue = "it-happy", retries = 0)]
async fn it_happy_job(input: Payload) -> JobResult<()> {
    assert_eq!(input.value, "hello");
    HAPPY_RUNS.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[pocopine::job(queue = "it-flaky", retries = 3)]
async fn it_flaky_job(_input: Payload) -> JobResult<()> {
    let attempt = FLAKY_RUNS.fetch_add(1, Ordering::SeqCst) + 1;
    if attempt <= FLAKY_FAIL_UNTIL.load(Ordering::SeqCst) {
        return Err(JobError::Env(format!(
            "simulated failure on attempt {attempt}"
        )));
    }
    Ok(())
}

#[pocopine::job(queue = "it-dead", retries = 1)]
async fn it_always_failing_job(_input: Payload) -> JobResult<()> {
    ALWAYS_FAILING_RUNS.fetch_add(1, Ordering::SeqCst);
    Err(JobError::Env("permanent failure".into()))
}

#[pocopine::job(queue = "it-panic", retries = 0)]
async fn it_panicking_job(_input: Payload) -> JobResult<()> {
    PANIC_RUNS.fetch_add(1, Ordering::SeqCst);
    panic!("intentional integration-test panic");
}

async fn drive_until_counter(
    worker: &Worker,
    counter: &AtomicUsize,
    target: usize,
    iter_cap: usize,
    sleep: Duration,
) {
    for _ in 0..iter_cap {
        let _ = worker.run_once().await.unwrap();
        if counter.load(Ordering::SeqCst) >= target {
            return;
        }
        tokio::time::sleep(sleep).await;
    }
}

async fn drive_until_xlen(
    worker: &Worker,
    url: &str,
    key: &str,
    target: i64,
    iter_cap: usize,
    sleep: Duration,
) {
    for _ in 0..iter_cap {
        let _ = worker.run_once().await.unwrap();
        if redis_xlen(url, key).await >= target {
            return;
        }
        tokio::time::sleep(sleep).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_in_promotes_consumes_and_acks() {
    let url = redis_url().await;
    let app = unique_app("happy");
    let client = JobClient::new(url.clone(), app.clone());
    let baseline = HAPPY_RUNS.load(Ordering::SeqCst);

    it_happy_job_job::schedule_in_with(
        &client,
        Payload {
            value: "hello".into(),
        },
        Duration::from_millis(0),
    )
    .await
    .unwrap();

    let scheduled_key = format!("pocopine:{app}:scheduled");
    assert_eq!(redis_zcard(&url, &scheduled_key).await, 1);

    let worker = Worker::new(worker_config(
        JobBackend::Redis { url: url.clone() },
        app.clone(),
        "it-happy",
    ))
    .unwrap();

    drive_until_counter(
        &worker,
        &HAPPY_RUNS,
        baseline + 1,
        20,
        Duration::from_millis(50),
    )
    .await;

    assert_eq!(HAPPY_RUNS.load(Ordering::SeqCst), baseline + 1);
    assert_eq!(redis_zcard(&url, &scheduled_key).await, 0);
    // The promote script atomically ZREMs from the sorted set and
    // XADDs onto the ready stream. Earlier code did the ZREM outside
    // the script and silently dropped XADD; this assertion is the
    // regression check.
    let queue_key = format!("pocopine:{app}:queue:it-happy");
    assert_eq!(redis_xlen(&url, &queue_key).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flaky_handler_retries_through_backoff_and_succeeds() {
    let url = redis_url().await;
    let app = unique_app("flaky");
    let client = JobClient::new(url.clone(), app.clone());

    FLAKY_RUNS.store(0, Ordering::SeqCst);
    FLAKY_FAIL_UNTIL.store(2, Ordering::SeqCst);

    it_flaky_job_job::enqueue_with(
        &client,
        Payload {
            value: "retry".into(),
        },
    )
    .await
    .unwrap();

    let worker = Worker::new(worker_config(
        JobBackend::Redis { url: url.clone() },
        app,
        "it-flaky",
    ))
    .unwrap();

    // First attempt fails, retry scheduled with backoff (base 1s).
    // 200 iters * 100 ms = 20 s — comfortably above the 2-3 s the two
    // retries need.
    drive_until_counter(&worker, &FLAKY_RUNS, 3, 200, Duration::from_millis(100)).await;

    assert_eq!(
        FLAKY_RUNS.load(Ordering::SeqCst),
        3,
        "expected exactly 3 attempts (2 fail + 1 success)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permanently_failing_handler_lands_in_dead_letter_stream() {
    let url = redis_url().await;
    let app = unique_app("dead");
    let client = JobClient::new(url.clone(), app.clone());

    let baseline = ALWAYS_FAILING_RUNS.load(Ordering::SeqCst);

    it_always_failing_job_job::enqueue_with(
        &client,
        Payload {
            value: "dead".into(),
        },
    )
    .await
    .unwrap();

    let worker = Worker::new(worker_config(
        JobBackend::Redis { url: url.clone() },
        app.clone(),
        "it-dead",
    ))
    .unwrap();

    // retries=1 → max_attempts=2 → 2 attempts then dead-letter.
    let dead_key = format!("pocopine:{app}:dead");
    drive_until_xlen(&worker, &url, &dead_key, 1, 200, Duration::from_millis(100)).await;

    assert_eq!(redis_xlen(&url, &dead_key).await, 1);
    assert!(
        ALWAYS_FAILING_RUNS.load(Ordering::SeqCst) >= baseline + 2,
        "handler should have run at least max_attempts=2 times"
    );

    assert!(matches!(
        worker.drain_dead_letter(),
        Err(JobError::Unsupported(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handler_panic_is_caught_and_dead_lettered() {
    let url = redis_url().await;
    let app = unique_app("panic");
    let client = JobClient::new(url.clone(), app.clone());

    let baseline = PANIC_RUNS.load(Ordering::SeqCst);
    it_panicking_job_job::enqueue_with(
        &client,
        Payload {
            value: "boom".into(),
        },
    )
    .await
    .unwrap();

    let worker = Worker::new(worker_config(
        JobBackend::Redis { url: url.clone() },
        app.clone(),
        "it-panic",
    ))
    .unwrap();

    // retries=0 → max_attempts=1 → first panic dead-letters.
    let dead_key = format!("pocopine:{app}:dead");
    drive_until_xlen(&worker, &url, &dead_key, 1, 200, Duration::from_millis(50)).await;

    assert_eq!(PANIC_RUNS.load(Ordering::SeqCst), baseline + 1);
    assert_eq!(redis_xlen(&url, &dead_key).await, 1);
}
