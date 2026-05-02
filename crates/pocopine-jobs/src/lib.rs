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
    use std::future::Future;
    use std::hash::{Hash, Hasher};
    use std::pin::Pin;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    const DEFAULT_CONSUMER: &str = "worker-1";
    const DEFAULT_BLOCK_MS: usize = 1_000;
    const DEFAULT_BATCH_SIZE: usize = 10;
    const DEFAULT_VISIBILITY_TIMEOUT_MS: u64 = 60_000;
    const DEFAULT_SCHEDULER_INTERVAL_MS: u64 = 1_000;
    const DEFAULT_MAX_PROMOTE: isize = 100;
    const DEFAULT_WORKER_ERROR_BACKOFF_MS: u64 = 1_000;
    const DEFAULT_WORKER_ERROR_BACKOFF_MAX_MS: u64 = 30_000;
    const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 1_000;
    const DEFAULT_RETRY_MAX_DELAY_MS: u64 = 60_000;

    static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);
    static MEMORY_STORES: OnceLock<Mutex<HashMap<String, Arc<MemoryStore>>>> = OnceLock::new();

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
        dead: Vec<(JobEnvelope, String)>,
        periodic_locks: HashMap<String, u64>,
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

    /// Client for enqueueing and scheduling background jobs.
    #[derive(Clone, Debug)]
    pub struct JobClient {
        backend: JobBackend,
        app: String,
    }

    impl JobClient {
        /// Build from `POCOPINE_JOB_BACKEND`, `POCOPINE_REDIS_URL`, and
        /// optional `POCOPINE_APP_NAME`.
        pub fn from_env() -> JobResult<Self> {
            Ok(Self {
                backend: job_backend_from_env()?,
                app: std::env::var("POCOPINE_APP_NAME")
                    .unwrap_or_else(|_| DEFAULT_APP_NAME.to_string()),
            })
        }

        /// Build a client with explicit Redis URL and app namespace.
        pub fn new(redis_url: impl Into<String>, app: impl Into<String>) -> Self {
            Self {
                backend: JobBackend::Redis {
                    url: redis_url.into(),
                },
                app: app.into(),
            }
        }

        /// Build a process-local memory client with an app namespace.
        pub fn memory(app: impl Into<String>) -> Self {
            Self {
                backend: JobBackend::Memory,
                app: app.into(),
            }
        }

        fn for_backend(backend: JobBackend, app: impl Into<String>) -> Self {
            Self {
                backend,
                app: app.into(),
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
                    let store = memory_store(&self.app)?;
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
                    let store = memory_store(&self.app)?;
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
            self.schedule_json_at(
                job_name,
                queue,
                max_attempts,
                payload,
                SystemTime::now() + delay,
            )
            .await
        }

        async fn connection(&self) -> JobResult<MultiplexedConnection> {
            let JobBackend::Redis { url } = &self.backend else {
                return Err(JobError::unsupported(
                    "the memory job backend does not have a Redis connection",
                ));
            };
            let client = redis::Client::open(url.as_str())?;
            Ok(client.get_multiplexed_async_connection().await?)
        }

        fn queue_key(&self, queue: &str) -> String {
            format!("pocopine:{}:queue:{queue}", self.app)
        }

        fn scheduled_key(&self) -> String {
            format!("pocopine:{}:scheduled", self.app)
        }

        fn dead_key(&self) -> String {
            format!("pocopine:{}:dead", self.app)
        }

        fn periodic_lock_key(&self, job_name: &str, due_ms: u64) -> String {
            format!("pocopine:{}:periodic:{job_name}:{due_ms}", self.app)
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
        /// Blocking read timeout.
        pub block_ms: usize,
        /// Jobs idle longer than this may be reclaimed.
        pub visibility_timeout: Duration,
        /// Scheduler polling interval.
        pub scheduler_interval: Duration,
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
                    .unwrap_or_else(|_| DEFAULT_CONSUMER.to_string()),
                block_ms: DEFAULT_BLOCK_MS,
                visibility_timeout: Duration::from_millis(DEFAULT_VISIBILITY_TIMEOUT_MS),
                scheduler_interval: Duration::from_millis(DEFAULT_SCHEDULER_INTERVAL_MS),
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

        /// Run until the process is stopped.
        pub async fn run(&self) -> JobResult<()> {
            let mut backoff = Duration::from_millis(DEFAULT_WORKER_ERROR_BACKOFF_MS);
            loop {
                match self.run_until_error().await {
                    Ok(()) => {
                        backoff = Duration::from_millis(DEFAULT_WORKER_ERROR_BACKOFF_MS);
                    }
                    Err(err) => {
                        eprintln!("pocopine worker error: {err}; retrying in {backoff:?}");
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
            self.enqueue_due_periodic_jobs(&mut *conn).await?;
            self.promote_due_jobs(&mut *conn).await?;
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

            let store = memory_store(&self.config.app)?;
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
            let store = memory_store(&self.config.app)?;
            for descriptor in self.descriptors.values() {
                let Some(schedule) = descriptor.periodic else {
                    continue;
                };
                if !self.config.queues.iter().any(|q| q == descriptor.queue) {
                    continue;
                }
                let Some(due_ms) =
                    due_periodic_slot(schedule, now_ms, self.config.scheduler_interval)?
                else {
                    continue;
                };
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
            }
            Ok(())
        }

        fn promote_due_jobs_memory(&self) -> JobResult<()> {
            let store = memory_store(&self.config.app)?;
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
        ) -> JobResult<()> {
            let now_ms = epoch_ms(SystemTime::now())?;
            for descriptor in self.descriptors.values() {
                let Some(schedule) = descriptor.periodic else {
                    continue;
                };
                if !self.config.queues.iter().any(|q| q == descriptor.queue) {
                    continue;
                }
                let Some(due_ms) =
                    due_periodic_slot(schedule, now_ms, self.config.scheduler_interval)?
                else {
                    continue;
                };
                let lock_key = self.client.periodic_lock_key(descriptor.name, due_ms);
                let locked: Option<String> = redis::cmd("SET")
                    .arg(lock_key)
                    .arg("1")
                    .arg("NX")
                    .arg("PX")
                    .arg(periodic_lock_ttl_ms(schedule))
                    .query_async(conn)
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
                xadd_envelope(conn, &self.client.queue_key(descriptor.queue), &envelope).await?;
            }
            Ok(())
        }

        async fn promote_due_jobs(&self, conn: &mut MultiplexedConnection) -> JobResult<()> {
            let now = epoch_ms(SystemTime::now())?;
            let raw_jobs: Vec<String> = redis::cmd("ZRANGEBYSCORE")
                .arg(self.client.scheduled_key())
                .arg("-inf")
                .arg(now)
                .arg("LIMIT")
                .arg(0)
                .arg(DEFAULT_MAX_PROMOTE)
                .query_async(conn)
                .await?;
            for raw in raw_jobs {
                let removed: i32 = redis::cmd("ZREM")
                    .arg(self.client.scheduled_key())
                    .arg(&raw)
                    .query_async(conn)
                    .await?;
                if removed == 0 {
                    continue;
                }
                let mut envelope: JobEnvelope = serde_json::from_str(&raw)?;
                envelope.scheduled_for_ms = None;
                xadd_envelope(conn, &self.client.queue_key(&envelope.queue), &envelope).await?;
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
                        self.move_to_dead(conn, &envelope, "job reclaimed after max attempts")
                            .await?;
                        self.ack(conn, &stream, &id.id).await?;
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
            let options = StreamReadOptions::default()
                .group(&self.config.group, &self.config.consumer)
                .count(self.config.batch_size)
                .block(self.config.block_ms);
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
                self.move_to_dead(conn, &envelope, "unknown job").await?;
                self.ack(conn, stream, stream_id).await?;
                return Ok(());
            };

            let payload = serde_json::to_vec(&envelope.payload)?;
            match (descriptor.handler)(payload).await {
                Ok(()) => self.ack(conn, stream, stream_id).await,
                Err(err) => {
                    if envelope.attempt < envelope.max_attempts {
                        let mut retry = envelope.clone();
                        retry.attempt += 1;
                        let due_ms = epoch_ms(SystemTime::now())?
                            .saturating_add(retry_delay_ms(retry.attempt, &retry.job_id));
                        retry.scheduled_for_ms = Some(due_ms);
                        schedule_envelope(conn, &self.client.scheduled_key(), &retry, due_ms)
                            .await?;
                    } else {
                        self.move_to_dead(conn, &envelope, &err.to_string()).await?;
                    }
                    self.ack(conn, stream, stream_id).await
                }
            }
        }

        async fn run_envelope_memory(&self, envelope: JobEnvelope) -> JobResult<()> {
            let Some(descriptor) = self.descriptors.get(envelope.job_name.as_str()) else {
                self.move_to_dead_memory(envelope, "unknown job")?;
                return Ok(());
            };

            let payload = serde_json::to_vec(&envelope.payload)?;
            match (descriptor.handler)(payload).await {
                Ok(()) => Ok(()),
                Err(err) => {
                    if envelope.attempt < envelope.max_attempts {
                        let mut retry = envelope.clone();
                        retry.attempt += 1;
                        let due_ms = epoch_ms(SystemTime::now())?
                            .saturating_add(retry_delay_ms(retry.attempt, &retry.job_id));
                        retry.scheduled_for_ms = Some(due_ms);
                        let store = memory_store(&self.config.app)?;
                        memory_schedule_envelope(&store, retry)?;
                    } else {
                        self.move_to_dead_memory(envelope, &err.to_string())?;
                    }
                    Ok(())
                }
            }
        }

        fn move_to_dead_memory(&self, envelope: JobEnvelope, error: &str) -> JobResult<()> {
            let store = memory_store(&self.config.app)?;
            memory_move_to_dead(&store, envelope, error)
        }

        async fn move_to_dead(
            &self,
            conn: &mut MultiplexedConnection,
            envelope: &JobEnvelope,
            error: &str,
        ) -> JobResult<()> {
            let raw = serde_json::to_string(envelope)?;
            let _: String = redis::cmd("XADD")
                .arg(self.client.dead_key())
                .arg("*")
                .arg("job_id")
                .arg(&envelope.job_id)
                .arg("job_name")
                .arg(&envelope.job_name)
                .arg("queue")
                .arg(&envelope.queue)
                .arg("attempt")
                .arg(envelope.attempt)
                .arg("max_attempts")
                .arg(envelope.max_attempts)
                .arg("error")
                .arg(error)
                .arg("envelope")
                .arg(raw)
                .query_async(conn)
                .await?;
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

    fn memory_move_to_dead(
        store: &MemoryStore,
        envelope: JobEnvelope,
        error: &str,
    ) -> JobResult<()> {
        memory_state(store)?
            .dead
            .push((envelope, error.to_string()));
        Ok(())
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

    async fn schedule_envelope(
        conn: &mut MultiplexedConnection,
        scheduled_key: &str,
        envelope: &JobEnvelope,
        due_ms: u64,
    ) -> JobResult<()> {
        let raw = serde_json::to_string(envelope)?;
        let _: () = redis::cmd("ZADD")
            .arg(scheduled_key)
            .arg(due_ms)
            .arg(raw)
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

    fn due_periodic_slot(
        schedule: PeriodicSchedule,
        now_ms: u64,
        scheduler_interval: Duration,
    ) -> JobResult<Option<u64>> {
        match schedule {
            PeriodicSchedule::Every { interval_ms } => {
                if interval_ms == 0 {
                    return Ok(None);
                }
                Ok(Some((now_ms / interval_ms) * interval_ms))
            }
            PeriodicSchedule::Cron { expr } => {
                let schedule = Schedule::from_str(expr).map_err(|err| {
                    JobError::Env(format!("invalid cron expression `{expr}`: {err}"))
                })?;
                let interval = ChronoDuration::from_std(scheduler_interval)
                    .unwrap_or_else(|_| ChronoDuration::seconds(1));
                let window_start = Utc::now() - interval;
                let Some(next) = schedule.after(&window_start).next() else {
                    return Ok(None);
                };
                let due_ms = next.timestamp_millis();
                if due_ms >= 0 && due_ms as u64 <= now_ms {
                    Ok(Some(due_ms as u64))
                } else {
                    Ok(None)
                }
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
        let pid = std::process::id();
        Ok(format!("{now:x}-{pid:x}-{next:x}"))
    }

    fn epoch_ms(time: SystemTime) -> JobResult<u64> {
        let duration = time
            .duration_since(UNIX_EPOCH)
            .map_err(|err| JobError::Time(err.to_string()))?;
        Ok(duration.as_millis() as u64)
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
            let parts: Vec<_> = id.split('-').collect();

            assert_eq!(parts.len(), 3);
            assert_eq!(parts[1], format!("{:x}", std::process::id()));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use host::{
    registered_jobs, JobBackend, JobClient, JobDescriptor, JobFuture, JobHandler, JobId,
    PeriodicSchedule, RetryPolicy, Worker, WorkerConfig,
};

#[cfg(not(target_arch = "wasm32"))]
pub use inventory;
#[cfg(not(target_arch = "wasm32"))]
pub use redis;
pub use serde_json;
