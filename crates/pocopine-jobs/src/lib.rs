//! Background jobs for pocopine apps.
//!
//! The public authoring surface normally comes from `#[pocopine::job]`.
//! This crate owns the host runtime: enqueue/schedule helpers,
//! `inventory`-backed job registration, and the worker loop.

use std::fmt;

/// Canonical result type for background jobs.
pub type JobResult<T = ()> = Result<T, JobError>;

/// Failures produced by job enqueueing, scheduling, decoding, and
/// execution.
#[derive(Debug)]
pub enum JobError {
    /// Redis command or connection failure.
    #[cfg(not(target_arch = "wasm32"))]
    Redis(redis::RedisError),
    /// JSON payload/envelope serialization failure.
    Json(serde_json::Error),
    /// Required environment variable is missing or unusable.
    Env(String),
    /// A worker received a job name that has no linked descriptor.
    UnknownJob(String),
    /// System time moved before Unix epoch.
    Time(String),
    /// Background jobs are host-only.
    Unsupported(String),
    /// A job handler panicked or its task was cancelled. The worker
    /// converts these into the same retry/dead-letter path as a normal
    /// `Err(_)` return so panicking handlers do not silently lose jobs.
    Handler(String),
}

impl JobError {
    /// Build a host-only unsupported failure.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        JobError::Unsupported(msg.into())
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            JobError::Redis(err) => write!(f, "redis error: {err}"),
            JobError::Json(err) => write!(f, "json error: {err}"),
            JobError::Env(msg) => write!(f, "environment error: {msg}"),
            JobError::UnknownJob(name) => write!(f, "unknown job: {name}"),
            JobError::Time(msg) => write!(f, "time error: {msg}"),
            JobError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            JobError::Handler(msg) => write!(f, "handler failure: {msg}"),
        }
    }
}

impl std::error::Error for JobError {}

#[cfg(not(target_arch = "wasm32"))]
impl From<redis::RedisError> for JobError {
    fn from(err: redis::RedisError) -> Self {
        JobError::Redis(err)
    }
}

impl From<serde_json::Error> for JobError {
    fn from(err: serde_json::Error) -> Self {
        JobError::Json(err)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::collections::{hash_map::DefaultHasher, BTreeSet, HashMap, VecDeque};
    use std::fs;
    use std::future::Future;
    use std::hash::{Hash, Hasher};
    use std::pin::Pin;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use chrono::{Duration as ChronoDuration, Utc};
    use cron::Schedule;
    use redis::aio::MultiplexedConnection;
    use redis::streams::{
        StreamAutoClaimOptions, StreamAutoClaimReply, StreamReadOptions, StreamReadReply,
    };
    use redis::{AsyncCommands, FromRedisValue, Value};
    use serde::{Deserialize, Serialize};

    use crate::{JobError, JobResult};

    const DEFAULT_APP_NAME: &str = "pocopine";
    const DEFAULT_GROUP: &str = "pocopine-workers";
    const DEFAULT_CONSUMER_PREFIX: &str = "worker";
    const DEFAULT_BLOCK_MS: usize = 1_000;
    const DEFAULT_BATCH_SIZE: usize = 10;
    const DEFAULT_VISIBILITY_TIMEOUT_MS: u64 = 60_000;
    const DEFAULT_SCHEDULER_INTERVAL_MS: u64 = 1_000;
    const DEFAULT_MAX_PROMOTE: isize = 100;
    const DEFAULT_MAX_PERIODIC_CATCH_UP: usize = 16;
    const DEFAULT_WORKER_ERROR_BACKOFF_MS: u64 = 1_000;
    const DEFAULT_WORKER_ERROR_BACKOFF_MAX_MS: u64 = 30_000;
    const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 1_000;
    const DEFAULT_RETRY_MAX_DELAY_MS: u64 = 60_000;
    /// Cap on the in-memory dead-letter buffer. Keeps a long-running
    /// embedded worker from leaking memory when a job is consistently
    /// failing; oldest entries are dropped first. Operators that need
    /// every dead-lettered job should call `Worker::drain_dead_letter`
    /// periodically and persist the results.
    const DEFAULT_MEMORY_DEAD_LETTER_CAP: usize = 1_024;
    const PROMOTE_SCHEDULED_SCRIPT_SRC: &str = r#"
local removed = redis.call('ZREM', KEYS[1], ARGV[1])
if removed == 0 then
  return 0
end
return redis.call(
  'XADD',
  KEYS[2],
  '*',
  'job_id',
  ARGV[2],
  'job_name',
  ARGV[3],
  'queue',
  ARGV[4],
  'payload',
  ARGV[5],
  'attempt',
  ARGV[6],
  'max_attempts',
  ARGV[7],
  'created_at_ms',
  ARGV[8],
  'scheduled_for_ms',
  ARGV[9]
)
"#;
    const SCHEDULE_RETRY_AND_ACK_SCRIPT_SRC: &str = r#"
local acked = redis.call('XACK', KEYS[2], ARGV[3], ARGV[4])
if acked == 0 then
  return 0
end
redis.call('ZADD', KEYS[1], ARGV[1], ARGV[2])
return acked
"#;
    const DEAD_LETTER_AND_ACK_SCRIPT_SRC: &str = r#"
local acked = redis.call('XACK', KEYS[2], ARGV[9], ARGV[10])
if acked == 0 then
  return 0
end
return redis.call(
  'XADD',
  KEYS[1],
  '*',
  'job_id',
  ARGV[1],
  'job_name',
  ARGV[2],
  'queue',
  ARGV[3],
  'attempt',
  ARGV[4],
  'max_attempts',
  ARGV[5],
  'error',
  ARGV[6],
  'envelope',
  ARGV[7],
  'failed_at_ms',
  ARGV[8]
)
"#;

    /// Cached `EVALSHA`-able scripts. The `redis` crate's `Script` LOADs
    /// the body once and uses `EVALSHA` thereafter, avoiding shipping the
    /// Lua source over the wire on every promote/retry/dead-letter call.
    fn promote_scheduled_script() -> &'static redis::Script {
        static SCRIPT: OnceLock<redis::Script> = OnceLock::new();
        SCRIPT.get_or_init(|| redis::Script::new(PROMOTE_SCHEDULED_SCRIPT_SRC))
    }

    fn schedule_retry_and_ack_script() -> &'static redis::Script {
        static SCRIPT: OnceLock<redis::Script> = OnceLock::new();
        SCRIPT.get_or_init(|| redis::Script::new(SCHEDULE_RETRY_AND_ACK_SCRIPT_SRC))
    }

    fn dead_letter_and_ack_script() -> &'static redis::Script {
        static SCRIPT: OnceLock<redis::Script> = OnceLock::new();
        SCRIPT.get_or_init(|| redis::Script::new(DEAD_LETTER_AND_ACK_SCRIPT_SRC))
    }

    static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);
    static MEMORY_STORES: OnceLock<Mutex<HashMap<String, Arc<MemoryStore>>>> = OnceLock::new();
    static PROCESS_IDENTITY: OnceLock<String> = OnceLock::new();

    /// Unique id assigned to an enqueued job.
    #[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
    pub struct JobId(pub String);

    impl JobId {
        /// String value sent to Redis.
        pub fn as_str(&self) -> &str {
            &self.0
        }
    }

    /// Runtime storage backend for background jobs.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum JobBackend {
        /// Redis-backed, durable enough for multi-process workers.
        Redis { url: String },
        /// Process-local memory backend. This is useful for single-process
        /// deployments and tests; it is not shared across server/worker
        /// processes and is not durable across restarts.
        Memory,
    }

    #[derive(Debug, Default)]
    struct MemoryStore {
        state: Mutex<MemoryState>,
    }

    #[derive(Debug, Default)]
    struct MemoryState {
        ready: VecDeque<JobEnvelope>,
        scheduled: Vec<JobEnvelope>,
        dead: VecDeque<(JobEnvelope, String)>,
        periodic_locks: HashMap<String, u64>,
        periodic_last_fired: HashMap<String, u64>,
    }

    /// Retry behavior attached to generated job descriptors.
    #[derive(Clone, Copy, Debug)]
    pub struct RetryPolicy {
        /// Maximum number of total attempts, including the first run.
        pub max_attempts: u32,
    }

    /// Recurring schedule attached to a job descriptor.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum PeriodicSchedule {
        /// Run once per interval. The worker uses Redis locks keyed by
        /// interval bucket so only one worker enqueues each due run.
        Every { interval_ms: u64 },
        /// Run from a cron expression parsed by the `cron` crate.
        Cron { expr: &'static str },
    }

    impl PeriodicSchedule {
        /// Build an interval schedule.
        pub const fn every_millis(interval_ms: u64) -> Self {
            Self::Every { interval_ms }
        }

        /// Build a cron-expression schedule.
        pub const fn cron(expr: &'static str) -> Self {
            Self::Cron { expr }
        }
    }

    impl RetryPolicy {
        /// Build a retry policy from a retry count.
        pub const fn from_retries(retries: u32) -> Self {
            Self {
                max_attempts: retries.saturating_add(1),
            }
        }
    }

    /// Boxed async job handler produced by `#[pocopine::job]`.
    pub type JobFuture = Pin<Box<dyn Future<Output = JobResult<()>> + Send + 'static>>;

    /// Function pointer used to decode and execute one registered job.
    pub type JobHandler = fn(Vec<u8>) -> JobFuture;

    /// Metadata registered by every `#[pocopine::job]` function.
    pub struct JobDescriptor {
        /// Stable job name used in Redis envelopes.
        pub name: &'static str,
        /// Redis queue/stream suffix.
        pub queue: &'static str,
        /// Retry behavior.
        pub retry_policy: RetryPolicy,
        /// Optional recurring schedule. Periodic jobs are zero-arg jobs;
        /// the worker enqueues `()` as the payload.
        pub periodic: Option<PeriodicSchedule>,
        /// Decode and run this job.
        pub handler: JobHandler,
    }

    impl JobDescriptor {
        /// Build a descriptor for `inventory::submit!`.
        pub const fn new(
            name: &'static str,
            queue: &'static str,
            retries: u32,
            handler: JobHandler,
        ) -> Self {
            Self {
                name,
                queue,
                retry_policy: RetryPolicy::from_retries(retries),
                periodic: None,
                handler,
            }
        }

        /// Build a recurring descriptor for `inventory::submit!`.
        pub const fn periodic(
            name: &'static str,
            queue: &'static str,
            retries: u32,
            periodic: PeriodicSchedule,
            handler: JobHandler,
        ) -> Self {
            Self {
                name,
                queue,
                retry_policy: RetryPolicy::from_retries(retries),
                periodic: Some(periodic),
                handler,
            }
        }
    }

    inventory::collect!(JobDescriptor);

    /// Iterate over linked job descriptors.
    pub fn registered_jobs() -> impl Iterator<Item = &'static JobDescriptor> {
        inventory::iter::<JobDescriptor>.into_iter()
    }

    /// Pre-formatted Redis keys for a given app namespace.
    ///
    /// `scheduled` and `dead` are constant per-app and are hit on every
    /// promote, retry, and dead-letter; caching avoids re-formatting them
    /// on the worker hot path.
    #[derive(Debug)]
    struct JobKeys {
        scheduled: String,
        dead: String,
    }

    impl JobKeys {
        fn new(app: &str) -> Self {
            Self {
                scheduled: format!("pocopine:{app}:scheduled"),
                dead: format!("pocopine:{app}:dead"),
            }
        }
    }

    /// Client for enqueueing and scheduling background jobs.
    #[derive(Clone, Debug)]
    pub struct JobClient {
        backend: JobBackend,
        app: String,
        keys: Arc<JobKeys>,
        memory_store: Arc<OnceLock<Arc<MemoryStore>>>,
        redis_connection: Arc<Mutex<Option<MultiplexedConnection>>>,
    }

    impl JobClient {
        /// Build from `POCOPINE_JOB_BACKEND`, `POCOPINE_REDIS_URL`, and
        /// optional `POCOPINE_APP_NAME`.
        pub fn from_env() -> JobResult<Self> {
            let app =
                std::env::var("POCOPINE_APP_NAME").unwrap_or_else(|_| DEFAULT_APP_NAME.to_string());
            Ok(Self::build(job_backend_from_env()?, app))
        }

        /// Build a client with explicit Redis URL and app namespace.
        pub fn new(redis_url: impl Into<String>, app: impl Into<String>) -> Self {
            Self::build(
                JobBackend::Redis {
                    url: redis_url.into(),
                },
                app.into(),
            )
        }

        /// Build a process-local memory client with an app namespace.
        pub fn memory(app: impl Into<String>) -> Self {
            Self::build(JobBackend::Memory, app.into())
        }

        fn for_backend(backend: JobBackend, app: impl Into<String>) -> Self {
            Self::build(backend, app.into())
        }

        fn build(backend: JobBackend, app: String) -> Self {
            let keys = Arc::new(JobKeys::new(&app));
            Self {
                backend,
                app,
                keys,
                memory_store: Arc::new(OnceLock::new()),
                redis_connection: Arc::new(Mutex::new(None)),
            }
        }

        /// Enqueue a typed payload immediately.
        pub async fn enqueue_json<T>(
            &self,
            job_name: &'static str,
            queue: &'static str,
            max_attempts: u32,
            payload: &T,
        ) -> JobResult<JobId>
        where
            T: Serialize,
        {
            let envelope = JobEnvelope::new(job_name, queue, max_attempts, payload, None)?;
            let id = JobId(envelope.job_id.clone());
            match &self.backend {
                JobBackend::Redis { .. } => {
                    let mut conn = self.connection().await?;
                    xadd_envelope(&mut conn, &self.queue_key(queue), &envelope).await?;
                }
                JobBackend::Memory => {
                    let store = self.cached_memory_store()?;
                    memory_enqueue_envelope(&store, envelope)?;
                }
            }
            Ok(id)
        }

        /// Schedule a typed payload for a future timestamp.
        pub async fn schedule_json_at<T>(
            &self,
            job_name: &'static str,
            queue: &'static str,
            max_attempts: u32,
            payload: &T,
            when: SystemTime,
        ) -> JobResult<JobId>
        where
            T: Serialize,
        {
            let due_ms = epoch_ms(when)?;
            self.schedule_json_at_ms(job_name, queue, max_attempts, payload, due_ms)
                .await
        }

        async fn schedule_json_at_ms<T>(
            &self,
            job_name: &'static str,
            queue: &'static str,
            max_attempts: u32,
            payload: &T,
            due_ms: u64,
        ) -> JobResult<JobId>
        where
            T: Serialize,
        {
            let envelope = JobEnvelope::new(job_name, queue, max_attempts, payload, Some(due_ms))?;
            let id = JobId(envelope.job_id.clone());
            match &self.backend {
                JobBackend::Redis { .. } => {
                    let raw = serde_json::to_string(&envelope)?;
                    let mut conn = self.connection().await?;
                    let _: () = redis::cmd("ZADD")
                        .arg(self.scheduled_key())
                        .arg(due_ms)
                        .arg(raw)
                        .query_async(&mut conn)
                        .await?;
                }
                JobBackend::Memory => {
                    let store = self.cached_memory_store()?;
                    memory_schedule_envelope(&store, envelope)?;
                }
            }
            Ok(id)
        }

        /// Schedule a typed payload after a delay.
        pub async fn schedule_json_in<T>(
            &self,
            job_name: &'static str,
            queue: &'static str,
            max_attempts: u32,
            payload: &T,
            delay: Duration,
        ) -> JobResult<JobId>
        where
            T: Serialize,
        {
            let now_ms = match &self.backend {
                JobBackend::Redis { .. } => {
                    let mut conn = self.connection().await?;
                    redis_time_ms(&mut conn).await?
                }
                JobBackend::Memory => epoch_ms(SystemTime::now())?,
            };
            self.schedule_json_at_ms(
                job_name,
                queue,
                max_attempts,
                payload,
                now_ms.saturating_add(duration_ms(delay)),
            )
            .await
        }

        async fn connection(&self) -> JobResult<MultiplexedConnection> {
            let JobBackend::Redis { url } = &self.backend else {
                return Err(JobError::unsupported(
                    "the memory job backend does not have a Redis connection",
                ));
            };
            if let Some(conn) = self.cached_connection()? {
                return Ok(conn);
            }

            let client = redis::Client::open(url.as_str())?;
            let conn = client.get_multiplexed_async_connection().await?;
            let mut cached = self.redis_connection.lock().map_err(|_| {
                JobError::Env("cached Redis job connection mutex was poisoned".into())
            })?;
            if let Some(existing) = cached.as_ref() {
                return Ok(existing.clone());
            }
            *cached = Some(conn.clone());
            Ok(conn)
        }

        fn cached_connection(&self) -> JobResult<Option<MultiplexedConnection>> {
            Ok(self
                .redis_connection
                .lock()
                .map_err(|_| {
                    JobError::Env("cached Redis job connection mutex was poisoned".into())
                })?
                .clone())
        }

        fn clear_connection(&self) {
            if let Ok(mut cached) = self.redis_connection.lock() {
                *cached = None;
            }
        }

        fn queue_key(&self, queue: &str) -> String {
            format!("pocopine:{}:queue:{queue}", self.app)
        }

        fn scheduled_key(&self) -> &str {
            &self.keys.scheduled
        }

        fn dead_key(&self) -> &str {
            &self.keys.dead
        }

        fn periodic_lock_key(&self, job_name: &str, due_ms: u64) -> String {
            format!("pocopine:{}:periodic:{job_name}:{due_ms}", self.app)
        }

        fn periodic_last_key(&self, job_name: &str) -> String {
            format!("pocopine:{}:periodic:last:{job_name}", self.app)
        }

        /// Resolve and cache the per-app `MemoryStore` handle so worker
        /// loops don't lock the global registry on every iteration.
        fn cached_memory_store(&self) -> JobResult<Arc<MemoryStore>> {
            if let Some(store) = self.memory_store.get() {
                return Ok(store.clone());
            }
            let store = memory_store(&self.app)?;
            let _ = self.memory_store.set(store.clone());
            Ok(store)
        }
    }

    /// Worker configuration.
    #[derive(Clone, Debug)]
    pub struct WorkerConfig {
        /// Storage backend.
        pub backend: JobBackend,
        /// App namespace.
        pub app: String,
        /// Queues this worker should consume.
        pub queues: Vec<String>,
        /// Redis Streams consumer group.
        pub group: String,
        /// Consumer name inside the group.
        pub consumer: String,
        /// Blocking read timeout for `XREADGROUP`. `0` means
        /// "non-blocking poll" (the worker returns immediately when no
        /// new entries are ready); a positive value caps the per-call
        /// `BLOCK` argument. Note that Redis itself interprets
        /// `BLOCK 0` as block forever, so this crate special-cases the
        /// zero value rather than passing it through verbatim.
        pub block_ms: usize,
        /// Jobs idle longer than this may be reclaimed.
        pub visibility_timeout: Duration,
        /// Scheduler polling interval.
        pub scheduler_interval: Duration,
        /// Maximum missed periodic slots to enqueue in one loop.
        pub max_periodic_catch_up: usize,
        /// Max jobs read/promoted per loop.
        pub batch_size: usize,
    }

    impl WorkerConfig {
        /// Build from environment variables and linked job descriptors.
        pub fn from_env() -> JobResult<Self> {
            let queues = match std::env::var("POCOPINE_JOB_QUEUES") {
                Ok(raw) => raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                Err(_) => registered_queues(),
            };

            Ok(Self {
                backend: job_backend_from_env()?,
                app: std::env::var("POCOPINE_APP_NAME")
                    .unwrap_or_else(|_| DEFAULT_APP_NAME.to_string()),
                queues,
                group: std::env::var("POCOPINE_JOB_GROUP")
                    .unwrap_or_else(|_| DEFAULT_GROUP.to_string()),
                consumer: std::env::var("POCOPINE_JOB_CONSUMER")
                    .unwrap_or_else(|_| default_consumer_name()),
                block_ms: DEFAULT_BLOCK_MS,
                visibility_timeout: millis_env(
                    "POCOPINE_JOB_VISIBILITY_MS",
                    DEFAULT_VISIBILITY_TIMEOUT_MS,
                )?,
                scheduler_interval: Duration::from_millis(DEFAULT_SCHEDULER_INTERVAL_MS),
                max_periodic_catch_up: usize_env(
                    "POCOPINE_JOB_PERIODIC_CATCH_UP_MAX",
                    DEFAULT_MAX_PERIODIC_CATCH_UP,
                )?,
                batch_size: DEFAULT_BATCH_SIZE,
            })
        }
    }

    /// Background worker.
    pub struct Worker {
        config: WorkerConfig,
        client: JobClient,
        descriptors: HashMap<&'static str, &'static JobDescriptor>,
    }

    impl Worker {
        /// Build from environment.
        pub fn from_env() -> JobResult<Self> {
            Self::new(WorkerConfig::from_env()?)
        }

        /// Build from explicit config.
        pub fn new(config: WorkerConfig) -> JobResult<Self> {
            let client = JobClient::for_backend(config.backend.clone(), config.app.clone());
            let descriptors = registered_jobs()
                .map(|descriptor| (descriptor.name, descriptor))
                .collect();
            Ok(Self {
                config,
                client,
                descriptors,
            })
        }

        /// Take and clear the in-memory dead-letter buffer. Returns
        /// `Unsupported` for the Redis backend; read the dead-letter
        /// stream `pocopine:{app}:dead` directly there.
        pub fn drain_dead_letter(&self) -> JobResult<Vec<DeadLetter>> {
            match &self.config.backend {
                JobBackend::Memory => {
                    let store = self.client.cached_memory_store()?;
                    memory_drain_dead_letter(&store)
                }
                JobBackend::Redis { .. } => Err(JobError::unsupported(
                    "drain_dead_letter is memory-backend only; \
                     read the Redis dead-letter stream directly",
                )),
            }
        }

        /// Run until the process is stopped.
        pub async fn run(&self) -> JobResult<()> {
            self.log_startup_backend();
            let mut backoff = Duration::from_millis(DEFAULT_WORKER_ERROR_BACKOFF_MS);
            loop {
                match self.run_until_error().await {
                    Ok(()) => {
                        backoff = Duration::from_millis(DEFAULT_WORKER_ERROR_BACKOFF_MS);
                    }
                    Err(err) => {
                        self.client.clear_connection();
                        tracing::warn!(
                            target: "pocopine.log",
                            error = %err,
                            retry_in_ms = backoff.as_millis() as u64,
                            "pocopine worker error; retrying"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2)
                            .min(Duration::from_millis(DEFAULT_WORKER_ERROR_BACKOFF_MAX_MS));
                    }
                }
            }
        }

        /// Run one scheduler/read/execute pass.
        ///
        /// This is primarily useful for embedded workers, tests, and hosts that
        /// want to own the outer loop.
        pub async fn run_once(&self) -> JobResult<usize> {
            match &self.config.backend {
                JobBackend::Redis { .. } => {
                    let mut conn = self.client.connection().await?;
                    self.ensure_groups(&mut conn).await?;
                    self.run_redis_once(&mut conn).await
                }
                JobBackend::Memory => self.run_memory_once().await,
            }
        }

        async fn run_until_error(&self) -> JobResult<()> {
            match &self.config.backend {
                JobBackend::Redis { .. } => self.run_redis_until_error().await,
                JobBackend::Memory => self.run_memory_until_error().await,
            }
        }

        // Print a one-line banner so operators can confirm which backend
        // the worker bound to. Especially useful when no env is set and
        // the backend silently defaults to Memory in a multi-process
        // deployment where Redis was intended.
        fn log_startup_backend(&self) {
            match &self.config.backend {
                JobBackend::Redis { .. } => tracing::info!(
                    target: "pocopine.log",
                    backend = "redis",
                    app = %self.config.app,
                    durable = true,
                    multi_process = true,
                    "pocopine worker backend selected"
                ),
                JobBackend::Memory => tracing::warn!(
                    target: "pocopine.log",
                    backend = "memory",
                    app = %self.config.app,
                    durable = false,
                    multi_process = false,
                    "pocopine worker backend selected; memory backend is process-local and lost on restart"
                ),
            }
        }

        async fn run_redis_until_error(&self) -> JobResult<()> {
            let mut conn = self.client.connection().await?;
            self.ensure_groups(&mut conn).await?;
            loop {
                let handled = self.run_redis_once(&mut conn).await?;
                if handled == 0 {
                    tokio::time::sleep(self.config.scheduler_interval).await;
                }
            }
        }

        async fn run_redis_once(&self, conn: &mut MultiplexedConnection) -> JobResult<usize> {
            let now_ms = redis_time_ms(&mut *conn).await?;
            self.enqueue_due_periodic_jobs(&mut *conn, now_ms).await?;
            self.promote_due_jobs(&mut *conn, now_ms).await?;
            self.reclaim_stale_jobs(&mut *conn).await?;
            self.read_ready_jobs(conn).await
        }

        async fn run_memory_until_error(&self) -> JobResult<()> {
            loop {
                let handled = self.run_memory_once().await?;
                if handled == 0 {
                    tokio::time::sleep(self.config.scheduler_interval).await;
                }
            }
        }

        async fn run_memory_once(&self) -> JobResult<usize> {
            self.enqueue_due_periodic_jobs_memory()?;
            self.promote_due_jobs_memory()?;

            let store = self.client.cached_memory_store()?;
            let envelopes =
                memory_pop_ready_envelopes(&store, &self.config.queues, self.config.batch_size)?;
            let handled = envelopes.len();
            for envelope in envelopes {
                self.run_envelope_memory(envelope).await?;
            }
            Ok(handled)
        }

        fn enqueue_due_periodic_jobs_memory(&self) -> JobResult<()> {
            let now_ms = epoch_ms(SystemTime::now())?;
            let store = self.client.cached_memory_store()?;
            for descriptor in self.descriptors.values() {
                let Some(schedule) = descriptor.periodic else {
                    continue;
                };
                if !self.config.queues.iter().any(|q| q == descriptor.queue) {
                    continue;
                }
                let last_key = self.client.periodic_last_key(descriptor.name);
                let last_fired_ms = memory_periodic_last_fired(&store, &last_key)?;
                for due_ms in due_periodic_slots(
                    schedule,
                    now_ms,
                    self.config.scheduler_interval,
                    last_fired_ms,
                    self.config.max_periodic_catch_up,
                )? {
                    let lock_key = self.client.periodic_lock_key(descriptor.name, due_ms);
                    let expires_at_ms = now_ms.saturating_add(periodic_lock_ttl_ms(schedule));
                    if !memory_try_periodic_lock(&store, lock_key, now_ms, expires_at_ms)? {
                        continue;
                    }

                    let envelope = JobEnvelope::new(
                        descriptor.name,
                        descriptor.queue,
                        descriptor.retry_policy.max_attempts,
                        &(),
                        None,
                    )?;
                    memory_enqueue_envelope(&store, envelope)?;
                    memory_set_periodic_last_fired(&store, last_key.clone(), due_ms)?;
                }
            }
            Ok(())
        }

        fn promote_due_jobs_memory(&self) -> JobResult<()> {
            let store = self.client.cached_memory_store()?;
            memory_promote_due_envelopes(&store, epoch_ms(SystemTime::now())?)
        }

        async fn ensure_groups(&self, conn: &mut MultiplexedConnection) -> JobResult<()> {
            for queue in &self.config.queues {
                let stream = self.client.queue_key(queue);
                let result: redis::RedisResult<()> = redis::cmd("XGROUP")
                    .arg("CREATE")
                    .arg(&stream)
                    .arg(&self.config.group)
                    .arg("0")
                    .arg("MKSTREAM")
                    .query_async(conn)
                    .await;
                if let Err(err) = result {
                    if !err.to_string().contains("BUSYGROUP") {
                        return Err(err.into());
                    }
                }
            }
            Ok(())
        }

        async fn enqueue_due_periodic_jobs(
            &self,
            conn: &mut MultiplexedConnection,
            now_ms: u64,
        ) -> JobResult<()> {
            for descriptor in self.descriptors.values() {
                let Some(schedule) = descriptor.periodic else {
                    continue;
                };
                if !self.config.queues.iter().any(|q| q == descriptor.queue) {
                    continue;
                }
                let last_key = self.client.periodic_last_key(descriptor.name);
                let last_fired_ms = redis_get_u64(conn, &last_key).await?;
                for due_ms in due_periodic_slots(
                    schedule,
                    now_ms,
                    self.config.scheduler_interval,
                    last_fired_ms,
                    self.config.max_periodic_catch_up,
                )? {
                    let lock_key = self.client.periodic_lock_key(descriptor.name, due_ms);
                    let locked: Option<String> = redis::cmd("SET")
                        .arg(lock_key)
                        .arg("1")
                        .arg("NX")
                        .arg("PX")
                        .arg(periodic_lock_ttl_ms(schedule))
                        .query_async(&mut *conn)
                        .await?;
                    if locked.is_none() {
                        continue;
                    }

                    let envelope = JobEnvelope::new(
                        descriptor.name,
                        descriptor.queue,
                        descriptor.retry_policy.max_attempts,
                        &(),
                        None,
                    )?;
                    xadd_envelope(
                        &mut *conn,
                        &self.client.queue_key(descriptor.queue),
                        &envelope,
                    )
                    .await?;
                    redis_set_u64(&mut *conn, &last_key, due_ms).await?;
                }
            }
            Ok(())
        }

        async fn promote_due_jobs(
            &self,
            conn: &mut MultiplexedConnection,
            now_ms: u64,
        ) -> JobResult<()> {
            let raw_jobs: Vec<String> = redis::cmd("ZRANGEBYSCORE")
                .arg(self.client.scheduled_key())
                .arg("-inf")
                .arg(now_ms)
                .arg("LIMIT")
                .arg(0)
                .arg(DEFAULT_MAX_PROMOTE)
                .query_async(conn)
                .await?;
            for raw in raw_jobs {
                let mut envelope: JobEnvelope = serde_json::from_str(&raw)?;
                envelope.scheduled_for_ms = None;
                let promoted = promote_scheduled_envelope(
                    conn,
                    self.client.scheduled_key(),
                    &self.client.queue_key(&envelope.queue),
                    &raw,
                    &envelope,
                )
                .await?;
                if !promoted {
                    continue;
                }
            }
            Ok(())
        }

        async fn reclaim_stale_jobs(&self, conn: &mut MultiplexedConnection) -> JobResult<()> {
            let idle_ms = self.config.visibility_timeout.as_millis() as u64;
            for queue in &self.config.queues {
                let stream = self.client.queue_key(queue);
                let reply: StreamAutoClaimReply = conn
                    .xautoclaim_options(
                        &stream,
                        &self.config.group,
                        &self.config.consumer,
                        idle_ms,
                        "0-0",
                        StreamAutoClaimOptions::default().count(self.config.batch_size),
                    )
                    .await?;
                for id in reply.claimed {
                    let envelope = match envelope_from_stream(&id.map) {
                        Ok(envelope) => envelope,
                        Err(err) => {
                            self.ack(conn, &stream, &id.id).await?;
                            return Err(err);
                        }
                    };
                    if envelope.attempt >= envelope.max_attempts {
                        self.move_to_dead_and_ack(
                            conn,
                            &stream,
                            &id.id,
                            &envelope,
                            "job reclaimed after max attempts",
                            None,
                        )
                        .await?;
                        continue;
                    }
                    let mut envelope = envelope;
                    envelope.attempt += 1;
                    self.run_envelope(conn, &stream, &id.id, envelope).await?;
                }
            }
            Ok(())
        }

        async fn read_ready_jobs(&self, conn: &mut MultiplexedConnection) -> JobResult<usize> {
            if self.config.queues.is_empty() {
                return Ok(0);
            }

            let streams: Vec<String> = self
                .config
                .queues
                .iter()
                .map(|queue| self.client.queue_key(queue))
                .collect();
            let ids: Vec<&str> = streams.iter().map(|_| ">").collect();
            // Special-case `block_ms == 0`: don't send `BLOCK` at all,
            // so XREADGROUP is a non-blocking poll. Redis treats
            // `BLOCK 0` as "block forever," which is rarely what
            // callers want and would deadlock `Worker::run_once` on
            // an idle stream.
            let mut options = StreamReadOptions::default()
                .group(&self.config.group, &self.config.consumer)
                .count(self.config.batch_size);
            if self.config.block_ms > 0 {
                options = options.block(self.config.block_ms);
            }
            let reply: StreamReadReply = conn.xread_options(&streams, &ids, &options).await?;

            let mut handled = 0;
            for key in reply.keys {
                for id in key.ids {
                    let envelope = match envelope_from_stream(&id.map) {
                        Ok(envelope) => envelope,
                        Err(err) => {
                            self.ack(conn, &key.key, &id.id).await?;
                            return Err(err);
                        }
                    };
                    self.run_envelope(conn, &key.key, &id.id, envelope).await?;
                    handled += 1;
                }
            }
            Ok(handled)
        }

        async fn run_envelope(
            &self,
            conn: &mut MultiplexedConnection,
            stream: &str,
            stream_id: &str,
            envelope: JobEnvelope,
        ) -> JobResult<()> {
            let Some(descriptor) = self.descriptors.get(envelope.job_name.as_str()) else {
                self.move_to_dead_and_ack(conn, stream, stream_id, &envelope, "unknown job", None)
                    .await?;
                return Ok(());
            };

            let started = Instant::now();
            log_job_started(&envelope, "redis");
            let payload = serde_json::to_vec(&envelope.payload)?;
            match run_handler_safely(descriptor.handler, payload).await {
                Ok(()) => {
                    log_job_completed(&envelope, "redis", elapsed_ms(started));
                    self.ack(conn, stream, stream_id).await
                }
                Err(err) => {
                    if envelope.attempt < envelope.max_attempts {
                        let mut retry = envelope.clone();
                        retry.attempt += 1;
                        let due_ms = redis_time_ms(&mut *conn)
                            .await?
                            .saturating_add(retry_delay_ms(retry.attempt, &retry.job_id));
                        retry.scheduled_for_ms = Some(due_ms);
                        schedule_retry_and_ack(
                            conn,
                            self.client.scheduled_key(),
                            stream,
                            &self.config.group,
                            stream_id,
                            &retry,
                            due_ms,
                        )
                        .await?;
                        log_job_retry_scheduled(
                            &envelope,
                            &retry,
                            due_ms,
                            "redis",
                            elapsed_ms(started),
                            &err,
                        );
                    } else {
                        self.move_to_dead_and_ack(
                            conn,
                            stream,
                            stream_id,
                            &envelope,
                            &err.to_string(),
                            Some(elapsed_ms(started)),
                        )
                        .await?;
                    }
                    Ok(())
                }
            }
        }

        async fn run_envelope_memory(&self, envelope: JobEnvelope) -> JobResult<()> {
            let Some(descriptor) = self.descriptors.get(envelope.job_name.as_str()) else {
                self.move_to_dead_memory(envelope, "unknown job", None)?;
                return Ok(());
            };

            let started = Instant::now();
            log_job_started(&envelope, "memory");
            let payload = serde_json::to_vec(&envelope.payload)?;
            match run_handler_safely(descriptor.handler, payload).await {
                Ok(()) => {
                    log_job_completed(&envelope, "memory", elapsed_ms(started));
                    Ok(())
                }
                Err(err) => {
                    if envelope.attempt < envelope.max_attempts {
                        let mut retry = envelope.clone();
                        retry.attempt += 1;
                        let due_ms = epoch_ms(SystemTime::now())?
                            .saturating_add(retry_delay_ms(retry.attempt, &retry.job_id));
                        retry.scheduled_for_ms = Some(due_ms);
                        let store = self.client.cached_memory_store()?;
                        memory_schedule_envelope(&store, retry.clone())?;
                        log_job_retry_scheduled(
                            &envelope,
                            &retry,
                            due_ms,
                            "memory",
                            elapsed_ms(started),
                            &err,
                        );
                    } else {
                        self.move_to_dead_memory(
                            envelope,
                            &err.to_string(),
                            Some(elapsed_ms(started)),
                        )?;
                    }
                    Ok(())
                }
            }
        }

        fn move_to_dead_memory(
            &self,
            envelope: JobEnvelope,
            error: &str,
            duration_ms: Option<u64>,
        ) -> JobResult<()> {
            let store = self.client.cached_memory_store()?;
            memory_move_to_dead(&store, envelope.clone(), error)?;
            log_job_dead_lettered(&envelope, "memory", error, duration_ms);
            Ok(())
        }

        async fn move_to_dead_and_ack(
            &self,
            conn: &mut MultiplexedConnection,
            stream: &str,
            stream_id: &str,
            envelope: &JobEnvelope,
            error: &str,
            duration_ms: Option<u64>,
        ) -> JobResult<()> {
            dead_letter_and_ack(
                conn,
                self.client.dead_key(),
                stream,
                &self.config.group,
                stream_id,
                envelope,
                error,
            )
            .await?;
            log_job_dead_lettered(envelope, "redis", error, duration_ms);
            Ok(())
        }

        async fn ack(
            &self,
            conn: &mut MultiplexedConnection,
            stream: &str,
            stream_id: &str,
        ) -> JobResult<()> {
            let _: i32 = redis::cmd("XACK")
                .arg(stream)
                .arg(&self.config.group)
                .arg(stream_id)
                .query_async(conn)
                .await?;
            Ok(())
        }
    }

    fn elapsed_ms(started: Instant) -> u64 {
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }

    fn log_job_started(envelope: &JobEnvelope, backend: &'static str) {
        tracing::info!(
            target: "pocopine.trace",
            backend = backend,
            job_id = %envelope.job_id,
            job_name = %envelope.job_name,
            queue = %envelope.queue,
            attempt = envelope.attempt,
            max_attempts = envelope.max_attempts,
            "job started"
        );
    }

    fn log_job_completed(envelope: &JobEnvelope, backend: &'static str, duration_ms: u64) {
        tracing::info!(
            target: "pocopine.trace",
            backend = backend,
            job_id = %envelope.job_id,
            job_name = %envelope.job_name,
            queue = %envelope.queue,
            attempt = envelope.attempt,
            max_attempts = envelope.max_attempts,
            duration_ms = duration_ms,
            "job completed"
        );
    }

    fn log_job_retry_scheduled(
        envelope: &JobEnvelope,
        retry: &JobEnvelope,
        due_ms: u64,
        backend: &'static str,
        duration_ms: u64,
        error: &JobError,
    ) {
        tracing::warn!(
            target: "pocopine.log",
            backend = backend,
            job_id = %envelope.job_id,
            job_name = %envelope.job_name,
            queue = %envelope.queue,
            attempt = envelope.attempt,
            next_attempt = retry.attempt,
            max_attempts = envelope.max_attempts,
            due_ms = due_ms,
            duration_ms = duration_ms,
            error = %error,
            "job retry scheduled"
        );
    }

    fn log_job_dead_lettered(
        envelope: &JobEnvelope,
        backend: &'static str,
        error: &str,
        duration_ms: Option<u64>,
    ) {
        match duration_ms {
            Some(duration_ms) => tracing::warn!(
                target: "pocopine.log",
                backend = backend,
                job_id = %envelope.job_id,
                job_name = %envelope.job_name,
                queue = %envelope.queue,
                attempt = envelope.attempt,
                max_attempts = envelope.max_attempts,
                duration_ms = duration_ms,
                error = error,
                "job moved to dead letter"
            ),
            None => tracing::warn!(
                target: "pocopine.log",
                backend = backend,
                job_id = %envelope.job_id,
                job_name = %envelope.job_name,
                queue = %envelope.queue,
                attempt = envelope.attempt,
                max_attempts = envelope.max_attempts,
                error = error,
                "job moved to dead letter"
            ),
        }
    }

    /// Snapshot of a dead-lettered job, returned by
    /// [`Worker::drain_dead_letter`].
    #[derive(Clone, Debug)]
    pub struct DeadLetter {
        /// Stable id assigned at enqueue time.
        pub job_id: String,
        /// Registered job name (`module::path::ident`).
        pub job_name: String,
        /// Queue the job lived on.
        pub queue: String,
        /// Final attempt count when the job was buried.
        pub attempt: u32,
        /// Configured retry budget.
        pub max_attempts: u32,
        /// Stringified terminal error (`Display` of `JobError`).
        pub error: String,
        /// Original enqueue time, milliseconds since the Unix epoch.
        pub created_at_ms: u64,
    }

    impl DeadLetter {
        fn from_envelope(envelope: JobEnvelope, error: String) -> Self {
            Self {
                job_id: envelope.job_id,
                job_name: envelope.job_name,
                queue: envelope.queue,
                attempt: envelope.attempt,
                max_attempts: envelope.max_attempts,
                error,
                created_at_ms: envelope.created_at_ms,
            }
        }
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct JobEnvelope {
        job_id: String,
        job_name: String,
        queue: String,
        payload: serde_json::Value,
        attempt: u32,
        max_attempts: u32,
        created_at_ms: u64,
        scheduled_for_ms: Option<u64>,
    }

    impl JobEnvelope {
        fn new<T>(
            job_name: &'static str,
            queue: &'static str,
            max_attempts: u32,
            payload: &T,
            scheduled_for_ms: Option<u64>,
        ) -> JobResult<Self>
        where
            T: Serialize,
        {
            Ok(Self {
                job_id: new_job_id()?,
                job_name: job_name.to_string(),
                queue: queue.to_string(),
                payload: serde_json::to_value(payload)?,
                attempt: 1,
                max_attempts: max_attempts.max(1),
                created_at_ms: epoch_ms(SystemTime::now())?,
                scheduled_for_ms,
            })
        }
    }

    fn memory_store(app: &str) -> JobResult<Arc<MemoryStore>> {
        let stores = MEMORY_STORES.get_or_init(|| Mutex::new(HashMap::new()));
        let mut stores = stores
            .lock()
            .map_err(|_| JobError::Env("memory job store registry was poisoned".into()))?;
        Ok(stores
            .entry(app.to_string())
            .or_insert_with(|| Arc::new(MemoryStore::default()))
            .clone())
    }

    fn memory_state(store: &MemoryStore) -> JobResult<MutexGuard<'_, MemoryState>> {
        store
            .state
            .lock()
            .map_err(|_| JobError::Env("memory job store was poisoned".into()))
    }

    fn memory_enqueue_envelope(store: &MemoryStore, envelope: JobEnvelope) -> JobResult<()> {
        memory_state(store)?.ready.push_back(envelope);
        Ok(())
    }

    fn memory_schedule_envelope(store: &MemoryStore, envelope: JobEnvelope) -> JobResult<()> {
        memory_state(store)?.scheduled.push(envelope);
        Ok(())
    }

    fn memory_promote_due_envelopes(store: &MemoryStore, now_ms: u64) -> JobResult<()> {
        let mut state = memory_state(store)?;
        let max_promote = DEFAULT_MAX_PROMOTE.max(0) as usize;
        let scheduled_count = state.scheduled.len();
        let mut remaining = Vec::with_capacity(scheduled_count);
        let mut promoted = VecDeque::new();

        for mut envelope in state.scheduled.drain(..) {
            let is_due = envelope.scheduled_for_ms.unwrap_or(0) <= now_ms;
            if is_due && promoted.len() < max_promote {
                envelope.scheduled_for_ms = None;
                promoted.push_back(envelope);
            } else {
                remaining.push(envelope);
            }
        }

        state.scheduled = remaining;
        state.ready.extend(promoted);
        Ok(())
    }

    fn memory_pop_ready_envelopes(
        store: &MemoryStore,
        queues: &[String],
        batch_size: usize,
    ) -> JobResult<Vec<JobEnvelope>> {
        if queues.is_empty() || batch_size == 0 {
            return Ok(Vec::new());
        }

        let allowed: BTreeSet<&str> = queues.iter().map(String::as_str).collect();
        let mut state = memory_state(store)?;
        let mut selected = Vec::new();
        let mut remaining = VecDeque::with_capacity(state.ready.len());

        while let Some(envelope) = state.ready.pop_front() {
            if selected.len() < batch_size && allowed.contains(envelope.queue.as_str()) {
                selected.push(envelope);
            } else {
                remaining.push_back(envelope);
            }
        }

        state.ready = remaining;
        Ok(selected)
    }

    fn memory_try_periodic_lock(
        store: &MemoryStore,
        lock_key: String,
        now_ms: u64,
        expires_at_ms: u64,
    ) -> JobResult<bool> {
        let mut state = memory_state(store)?;
        state.periodic_locks.retain(|_, expires| *expires > now_ms);
        if state.periodic_locks.contains_key(&lock_key) {
            return Ok(false);
        }
        state.periodic_locks.insert(lock_key, expires_at_ms);
        Ok(true)
    }

    fn memory_periodic_last_fired(store: &MemoryStore, key: &str) -> JobResult<Option<u64>> {
        Ok(memory_state(store)?.periodic_last_fired.get(key).copied())
    }

    fn memory_set_periodic_last_fired(
        store: &MemoryStore,
        key: String,
        due_ms: u64,
    ) -> JobResult<()> {
        memory_state(store)?.periodic_last_fired.insert(key, due_ms);
        Ok(())
    }

    fn memory_move_to_dead(
        store: &MemoryStore,
        envelope: JobEnvelope,
        error: &str,
    ) -> JobResult<()> {
        let mut state = memory_state(store)?;
        state.dead.push_back((envelope, error.to_string()));
        while state.dead.len() > DEFAULT_MEMORY_DEAD_LETTER_CAP {
            state.dead.pop_front();
        }
        Ok(())
    }

    fn memory_drain_dead_letter(store: &MemoryStore) -> JobResult<Vec<DeadLetter>> {
        let mut state = memory_state(store)?;
        Ok(state
            .dead
            .drain(..)
            .map(|(envelope, error)| DeadLetter::from_envelope(envelope, error))
            .collect())
    }

    async fn xadd_envelope(
        conn: &mut MultiplexedConnection,
        stream: &str,
        envelope: &JobEnvelope,
    ) -> JobResult<()> {
        let payload = serde_json::to_string(&envelope.payload)?;
        let _: String = redis::cmd("XADD")
            .arg(stream)
            .arg("*")
            .arg("job_id")
            .arg(&envelope.job_id)
            .arg("job_name")
            .arg(&envelope.job_name)
            .arg("queue")
            .arg(&envelope.queue)
            .arg("payload")
            .arg(payload)
            .arg("attempt")
            .arg(envelope.attempt)
            .arg("max_attempts")
            .arg(envelope.max_attempts)
            .arg("created_at_ms")
            .arg(envelope.created_at_ms)
            .arg("scheduled_for_ms")
            .arg(envelope.scheduled_for_ms.unwrap_or(0))
            .query_async(conn)
            .await?;
        Ok(())
    }

    async fn promote_scheduled_envelope(
        conn: &mut MultiplexedConnection,
        scheduled_key: &str,
        queue_key: &str,
        raw_scheduled_envelope: &str,
        envelope: &JobEnvelope,
    ) -> JobResult<bool> {
        let payload = serde_json::to_string(&envelope.payload)?;
        let value: Value = promote_scheduled_script()
            .key(scheduled_key)
            .key(queue_key)
            .arg(raw_scheduled_envelope)
            .arg(envelope.job_id.as_str())
            .arg(envelope.job_name.as_str())
            .arg(envelope.queue.as_str())
            .arg(payload)
            .arg(envelope.attempt)
            .arg(envelope.max_attempts)
            .arg(envelope.created_at_ms)
            .arg(0_u64)
            .invoke_async(conn)
            .await?;
        Ok(!matches!(value, Value::Nil | Value::Int(0)))
    }

    async fn schedule_retry_and_ack(
        conn: &mut MultiplexedConnection,
        scheduled_key: &str,
        stream: &str,
        group: &str,
        stream_id: &str,
        envelope: &JobEnvelope,
        due_ms: u64,
    ) -> JobResult<()> {
        let raw = serde_json::to_string(envelope)?;
        let _: i32 = schedule_retry_and_ack_script()
            .key(scheduled_key)
            .key(stream)
            .arg(due_ms)
            .arg(raw)
            .arg(group)
            .arg(stream_id)
            .invoke_async(conn)
            .await?;
        Ok(())
    }

    async fn dead_letter_and_ack(
        conn: &mut MultiplexedConnection,
        dead_key: &str,
        stream: &str,
        group: &str,
        stream_id: &str,
        envelope: &JobEnvelope,
        error: &str,
    ) -> JobResult<()> {
        let raw = serde_json::to_string(envelope)?;
        let failed_at_ms = redis_time_ms(&mut *conn).await?;
        let _: Value = dead_letter_and_ack_script()
            .key(dead_key)
            .key(stream)
            .arg(envelope.job_id.as_str())
            .arg(envelope.job_name.as_str())
            .arg(envelope.queue.as_str())
            .arg(envelope.attempt)
            .arg(envelope.max_attempts)
            .arg(error)
            .arg(raw)
            .arg(failed_at_ms)
            .arg(group)
            .arg(stream_id)
            .invoke_async(conn)
            .await?;
        Ok(())
    }

    /// Run a job handler future with panic-safety so a panicking
    /// handler is converted into a regular `JobError::Handler` and
    /// flows through the worker's normal retry/dead-letter path
    /// instead of unwinding the worker task.
    async fn run_handler_safely(handler: JobHandler, payload: Vec<u8>) -> JobResult<()> {
        match tokio::spawn(handler(payload)).await {
            Ok(result) => result,
            Err(join_err) => {
                let msg = match join_err.try_into_panic() {
                    Ok(panic) => {
                        if let Some(s) = panic.downcast_ref::<&'static str>() {
                            format!("handler panicked: {s}")
                        } else if let Some(s) = panic.downcast_ref::<String>() {
                            format!("handler panicked: {s}")
                        } else {
                            "handler panicked".to_string()
                        }
                    }
                    Err(_) => "handler task cancelled before completion".to_string(),
                };
                Err(JobError::Handler(msg))
            }
        }
    }

    async fn redis_time_ms(conn: &mut MultiplexedConnection) -> JobResult<u64> {
        let (seconds, micros): (u64, u64) = redis::cmd("TIME").query_async(conn).await?;
        Ok(seconds.saturating_mul(1_000).saturating_add(micros / 1_000))
    }

    async fn redis_get_u64(conn: &mut MultiplexedConnection, key: &str) -> JobResult<Option<u64>> {
        let raw: Option<String> = redis::cmd("GET").arg(key).query_async(conn).await?;
        raw.map(|value| {
            value.parse::<u64>().map_err(|err| {
                JobError::Env(format!("Redis key `{key}` should contain a u64: {err}"))
            })
        })
        .transpose()
    }

    async fn redis_set_u64(
        conn: &mut MultiplexedConnection,
        key: &str,
        value: u64,
    ) -> JobResult<()> {
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .query_async(conn)
            .await?;
        Ok(())
    }

    fn envelope_from_stream(map: &HashMap<String, Value>) -> JobResult<JobEnvelope> {
        let job_id = field::<String>(map, "job_id")?;
        let job_name = field::<String>(map, "job_name")?;
        let queue = field::<String>(map, "queue")?;
        let payload_raw = field::<String>(map, "payload")?;
        let payload = serde_json::from_str(&payload_raw)?;
        Ok(JobEnvelope {
            job_id,
            job_name,
            queue,
            payload,
            attempt: field::<u32>(map, "attempt")?,
            max_attempts: field::<u32>(map, "max_attempts")?,
            created_at_ms: field::<u64>(map, "created_at_ms")?,
            scheduled_for_ms: match field::<u64>(map, "scheduled_for_ms")? {
                0 => None,
                value => Some(value),
            },
        })
    }

    fn field<T>(map: &HashMap<String, Value>, key: &str) -> JobResult<T>
    where
        T: FromRedisValue,
    {
        let value = map
            .get(key)
            .ok_or_else(|| JobError::Env(format!("job envelope missing `{key}`")))?;
        Ok(T::from_redis_value(value)?)
    }

    fn registered_queues() -> Vec<String> {
        let queues: BTreeSet<String> = registered_jobs()
            .map(|descriptor| descriptor.queue.to_string())
            .collect();
        if queues.is_empty() {
            vec!["default".to_string()]
        } else {
            queues.into_iter().collect()
        }
    }

    fn job_backend_from_env() -> JobResult<JobBackend> {
        match std::env::var("POCOPINE_JOB_BACKEND") {
            Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "memory" => Ok(JobBackend::Memory),
                "redis" => redis_url_from_env().map(|url| JobBackend::Redis { url }),
                "" => Err(JobError::Env(
                    "POCOPINE_JOB_BACKEND was set but empty; use `memory` or `redis`".into(),
                )),
                other => Err(JobError::Env(format!(
                    "unsupported POCOPINE_JOB_BACKEND `{other}`; use `memory` or `redis`"
                ))),
            },
            Err(_) => match std::env::var("POCOPINE_REDIS_URL") {
                Ok(url) if !url.trim().is_empty() => Ok(JobBackend::Redis { url }),
                Ok(_) => Err(JobError::Env(
                    "POCOPINE_REDIS_URL was set but empty; unset it for memory jobs or set a Redis URL".into(),
                )),
                Err(_) => Ok(JobBackend::Memory),
            },
        }
    }

    fn redis_url_from_env() -> JobResult<String> {
        let url = std::env::var("POCOPINE_REDIS_URL").map_err(|_| {
            JobError::Env("POCOPINE_REDIS_URL must be set when POCOPINE_JOB_BACKEND=redis".into())
        })?;
        if url.trim().is_empty() {
            return Err(JobError::Env(
                "POCOPINE_REDIS_URL must not be empty when POCOPINE_JOB_BACKEND=redis".into(),
            ));
        }
        Ok(url)
    }

    fn millis_env(name: &str, default_ms: u64) -> JobResult<Duration> {
        match std::env::var(name) {
            Ok(raw) => parse_positive_millis(name, &raw),
            Err(_) => Ok(Duration::from_millis(default_ms)),
        }
    }

    fn parse_positive_millis(name: &str, raw: &str) -> JobResult<Duration> {
        let value = raw.trim().parse::<u64>().map_err(|err| {
            JobError::Env(format!(
                "{name} must be a positive integer number of milliseconds: {err}"
            ))
        })?;
        if value == 0 {
            return Err(JobError::Env(format!("{name} must be greater than 0")));
        }
        Ok(Duration::from_millis(value))
    }

    fn usize_env(name: &str, default_value: usize) -> JobResult<usize> {
        match std::env::var(name) {
            Ok(raw) => parse_positive_usize(name, &raw),
            Err(_) => Ok(default_value),
        }
    }

    fn parse_positive_usize(name: &str, raw: &str) -> JobResult<usize> {
        let value = raw
            .trim()
            .parse::<usize>()
            .map_err(|err| JobError::Env(format!("{name} must be a positive integer: {err}")))?;
        if value == 0 {
            return Err(JobError::Env(format!("{name} must be greater than 0")));
        }
        Ok(value)
    }

    fn due_periodic_slots(
        schedule: PeriodicSchedule,
        now_ms: u64,
        scheduler_interval: Duration,
        last_fired_ms: Option<u64>,
        max_catch_up: usize,
    ) -> JobResult<Vec<u64>> {
        match schedule {
            PeriodicSchedule::Every { interval_ms } => {
                if interval_ms == 0 {
                    return Ok(Vec::new());
                }
                let due_ms = (now_ms / interval_ms) * interval_ms;
                if last_fired_ms.is_some_and(|last| last >= due_ms) {
                    Ok(Vec::new())
                } else {
                    Ok(vec![due_ms])
                }
            }
            PeriodicSchedule::Cron { expr } => {
                let schedule = Schedule::from_str(expr).map_err(|err| {
                    JobError::Env(format!("invalid cron expression `{expr}`: {err}"))
                })?;
                let now = chrono::DateTime::<Utc>::from_timestamp_millis(now_ms as i64)
                    .ok_or_else(|| JobError::Time(format!("invalid timestamp ms: {now_ms}")))?;
                let window_start = match last_fired_ms {
                    Some(last_fired_ms) => {
                        chrono::DateTime::<Utc>::from_timestamp_millis(last_fired_ms as i64)
                            .ok_or_else(|| {
                                JobError::Time(format!(
                                    "invalid last-fired timestamp ms: {last_fired_ms}"
                                ))
                            })?
                    }
                    None => {
                        let interval = ChronoDuration::from_std(scheduler_interval)
                            .unwrap_or_else(|_| ChronoDuration::seconds(1));
                        now - interval
                    }
                };

                Ok(schedule
                    .after(&window_start)
                    .take(max_catch_up)
                    .filter_map(|due| {
                        let due_ms = due.timestamp_millis();
                        (due_ms >= 0 && due_ms as u64 <= now_ms).then_some(due_ms as u64)
                    })
                    .collect())
            }
        }
    }

    fn periodic_lock_ttl_ms(schedule: PeriodicSchedule) -> u64 {
        match schedule {
            PeriodicSchedule::Every { interval_ms } => interval_ms
                .saturating_mul(2)
                .clamp(60_000, 7 * 24 * 60 * 60 * 1_000),
            PeriodicSchedule::Cron { .. } => 7 * 24 * 60 * 60 * 1_000,
        }
    }

    fn retry_delay_ms(attempt: u32, job_id: &str) -> u64 {
        let exponent = attempt.saturating_sub(2).min(10);
        let base = DEFAULT_RETRY_BASE_DELAY_MS
            .saturating_mul(1_u64 << exponent)
            .min(DEFAULT_RETRY_MAX_DELAY_MS);
        let jitter_span = (base / 5).max(1);
        base.saturating_add(jitter_ms(job_id, attempt, jitter_span))
            .min(DEFAULT_RETRY_MAX_DELAY_MS)
    }

    fn jitter_ms(job_id: &str, attempt: u32, span: u64) -> u64 {
        if span == 0 {
            return 0;
        }
        let mut hasher = DefaultHasher::new();
        job_id.hash(&mut hasher);
        attempt.hash(&mut hasher);
        hasher.finish() % span
    }

    fn new_job_id() -> JobResult<String> {
        let now = epoch_ms(SystemTime::now())?;
        let next = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(format!("{now:x}:{}:{next:x}", process_identity()))
    }

    fn epoch_ms(time: SystemTime) -> JobResult<u64> {
        let duration = time
            .duration_since(UNIX_EPOCH)
            .map_err(|err| JobError::Time(err.to_string()))?;
        Ok(duration.as_millis() as u64)
    }

    fn duration_ms(duration: Duration) -> u64 {
        duration.as_millis().min(u128::from(u64::MAX)) as u64
    }

    fn default_consumer_name() -> String {
        format!("{DEFAULT_CONSUMER_PREFIX}-{}", process_identity())
    }

    fn process_identity() -> &'static str {
        PROCESS_IDENTITY.get_or_init(|| {
            let host = sanitize_identity_part(&host_name_hint());
            let pid = std::process::id();
            let started_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            format!("{host}_{pid:x}_{started_at:x}")
        })
    }

    fn host_name_hint() -> String {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| "host".to_string())
    }

    fn sanitize_identity_part(raw: &str) -> String {
        let sanitized: String = raw
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        let sanitized = sanitized.trim_matches('_');
        if sanitized.is_empty() {
            "host".to_string()
        } else {
            sanitized.to_string()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn retry_delay_uses_exponential_backoff_with_jitter_cap() {
            let second = retry_delay_ms(2, "job-a");
            let third = retry_delay_ms(3, "job-a");
            let tenth = retry_delay_ms(10, "job-a");

            assert!(second >= DEFAULT_RETRY_BASE_DELAY_MS);
            assert!(third >= second);
            assert!(tenth <= DEFAULT_RETRY_MAX_DELAY_MS);
        }

        #[test]
        fn job_id_includes_process_identity() {
            let id = new_job_id().unwrap();
            let parts: Vec<_> = id.split(':').collect();

            assert_eq!(parts.len(), 3);
            assert!(parts[1].contains(&format!("{:x}", std::process::id())));
        }

        #[test]
        fn default_consumer_name_is_process_unique() {
            let consumer = default_consumer_name();

            assert!(consumer.starts_with("worker-"));
            assert!(consumer.contains(&format!("{:x}", std::process::id())));
        }

        #[test]
        fn visibility_timeout_parser_rejects_zero_and_invalid_values() {
            assert_eq!(
                parse_positive_millis("POCOPINE_JOB_VISIBILITY_MS", "1500")
                    .unwrap()
                    .as_millis(),
                1500
            );
            assert!(parse_positive_millis("POCOPINE_JOB_VISIBILITY_MS", "0").is_err());
            assert!(parse_positive_millis("POCOPINE_JOB_VISIBILITY_MS", "banana").is_err());
        }

        #[test]
        fn positive_usize_env_parser_rejects_zero_and_invalid_values() {
            assert_eq!(
                parse_positive_usize("POCOPINE_JOB_PERIODIC_CATCH_UP_MAX", "32").unwrap(),
                32
            );
            assert!(parse_positive_usize("POCOPINE_JOB_PERIODIC_CATCH_UP_MAX", "0").is_err());
            assert!(parse_positive_usize("POCOPINE_JOB_PERIODIC_CATCH_UP_MAX", "banana").is_err());
        }

        #[test]
        fn cron_due_slots_catch_up_from_last_fired_time() {
            let last = chrono::DateTime::parse_from_rfc3339("2026-05-02T00:00:00Z")
                .unwrap()
                .timestamp_millis() as u64;
            let now = chrono::DateTime::parse_from_rfc3339("2026-05-02T00:00:15Z")
                .unwrap()
                .timestamp_millis() as u64;

            let slots = due_periodic_slots(
                PeriodicSchedule::Cron {
                    expr: "0/5 * * * * * *",
                },
                now,
                Duration::from_secs(1),
                Some(last),
                DEFAULT_MAX_PERIODIC_CATCH_UP,
            )
            .unwrap();

            assert_eq!(slots.len(), 3);
            assert_eq!(slots[0], last + 5_000);
            assert_eq!(slots[1], last + 10_000);
            assert_eq!(slots[2], last + 15_000);
        }

        #[test]
        fn cron_due_slots_respect_catch_up_cap() {
            let last = chrono::DateTime::parse_from_rfc3339("2026-05-02T00:00:00Z")
                .unwrap()
                .timestamp_millis() as u64;
            let now = chrono::DateTime::parse_from_rfc3339("2026-05-02T00:00:15Z")
                .unwrap()
                .timestamp_millis() as u64;

            let slots = due_periodic_slots(
                PeriodicSchedule::Cron {
                    expr: "0/5 * * * * * *",
                },
                now,
                Duration::from_secs(1),
                Some(last),
                2,
            )
            .unwrap();

            assert_eq!(slots.len(), 2);
            assert_eq!(slots[0], last + 5_000);
            assert_eq!(slots[1], last + 10_000);
        }

        #[test]
        fn every_due_slot_does_not_repeat_last_fired_slot() {
            let slots = due_periodic_slots(
                PeriodicSchedule::Every { interval_ms: 5_000 },
                12_000,
                Duration::from_secs(1),
                Some(10_000),
                DEFAULT_MAX_PERIODIC_CATCH_UP,
            )
            .unwrap();

            assert!(slots.is_empty());
        }

        #[test]
        fn redis_promotes_scheduled_jobs_when_redis_test_url_is_set() {
            let Ok(redis_url) = std::env::var("REDIS_TEST_URL") else {
                return;
            };

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                const QUEUE: &str = "scheduled-smoke";
                const JOB_NAME: &str = "redis_promotes_scheduled_jobs";

                let app = format!(
                    "pocopine-test-promote-{}-{}",
                    std::process::id(),
                    JOB_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                );
                let client = JobClient::new(redis_url.clone(), app.clone());
                let scheduled_key = client.scheduled_key();
                let queue_key = client.queue_key(QUEUE);
                let mut conn = client.connection().await.unwrap();
                let _: i64 = redis::cmd("DEL")
                    .arg(scheduled_key)
                    .arg(&queue_key)
                    .query_async(&mut conn)
                    .await
                    .unwrap();

                client
                    .schedule_json_at(
                        JOB_NAME,
                        QUEUE,
                        1,
                        &serde_json::json!({ "value": "ok" }),
                        UNIX_EPOCH,
                    )
                    .await
                    .unwrap();

                let worker = Worker::new(WorkerConfig {
                    backend: JobBackend::Redis { url: redis_url },
                    app,
                    queues: vec![QUEUE.to_string()],
                    group: "test".to_string(),
                    consumer: default_consumer_name(),
                    block_ms: 0,
                    visibility_timeout: Duration::from_secs(60),
                    scheduler_interval: Duration::from_millis(1),
                    max_periodic_catch_up: DEFAULT_MAX_PERIODIC_CATCH_UP,
                    batch_size: 10,
                })
                .unwrap();
                let now_ms = redis_time_ms(&mut conn).await.unwrap();

                worker.promote_due_jobs(&mut conn, now_ms).await.unwrap();

                let scheduled_len: i64 = redis::cmd("ZCARD")
                    .arg(scheduled_key)
                    .query_async(&mut conn)
                    .await
                    .unwrap();
                let stream_len: i64 = redis::cmd("XLEN")
                    .arg(&queue_key)
                    .query_async(&mut conn)
                    .await
                    .unwrap();
                assert_eq!(scheduled_len, 0);
                assert_eq!(stream_len, 1);

                worker.promote_due_jobs(&mut conn, now_ms).await.unwrap();
                let stream_len_after_second_promote: i64 = redis::cmd("XLEN")
                    .arg(&queue_key)
                    .query_async(&mut conn)
                    .await
                    .unwrap();
                assert_eq!(stream_len_after_second_promote, 1);

                let _: i64 = redis::cmd("DEL")
                    .arg(scheduled_key)
                    .arg(&queue_key)
                    .query_async(&mut conn)
                    .await
                    .unwrap();
            });
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use host::{
    registered_jobs, DeadLetter, JobBackend, JobClient, JobDescriptor, JobFuture, JobHandler,
    JobId, PeriodicSchedule, RetryPolicy, Worker, WorkerConfig,
};

#[cfg(not(target_arch = "wasm32"))]
pub use inventory;
#[cfg(not(target_arch = "wasm32"))]
pub use redis;
pub use serde_json;
