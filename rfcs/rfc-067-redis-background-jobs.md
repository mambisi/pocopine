# RFC 067 - Background jobs with Redis and memory backends

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-02 |
| **Related** | [`rfc-002-app-stores-servers.md`](./rfc-002-app-stores-servers.md), [`rfc-066-server-function-auth.md`](./rfc-066-server-function-auth.md) |
| **Supersedes** | - |

## 1. Summary

Add native background jobs for server-side Pocopine apps:

```rust
#[pocopine::job(queue = "mail", retries = 3)]
pub async fn send_welcome_email(input: WelcomeEmail) -> JobResult<()> {
    /* host-side work */
}
```

The macro registers job metadata through `inventory`, generates typed
enqueue/schedule helpers, and keeps host-only worker code out of wasm
bundles. A new `pocopine-jobs` crate owns the runtime. Redis is the
durable multi-process backend; a process-local memory backend is available
for single-process production deployments and tests.

Periodic jobs ship in the same slice:

```rust
#[pocopine::job(queue = "maintenance", every = "15m")]
pub async fn refresh_indexes() -> JobResult<()> {
    /* host-side work */
}

#[pocopine::job(queue = "maintenance", cron = "0 0 2 * * * *")]
pub async fn nightly_cleanup() -> JobResult<()> {
    /* host-side work */
}
```

## 2. Design

- The runtime has two storage backends:
  - Redis for durable-enough queues shared by server and worker binaries.
  - Memory for process-local queues when enqueueing and worker execution live
    in the same process.
- Redis Streams are the ready queue:
  `pocopine:{app}:queue:{queue}`.
- Redis sorted sets hold delayed jobs:
  `pocopine:{app}:scheduled`.
- Redis locks coordinate periodic enqueue slots:
  `pocopine:{app}:periodic:{job}:{due_ms}`.
- Exhausted jobs move to:
  `pocopine:{app}:dead`.
- Worker coordination uses Streams consumer groups.
- Manually enqueued `#[job]` functions take exactly one owned payload
  argument and return `JobResult<()>`.
- Periodic `#[job(every = "...")]` and `#[job(cron = "...")]`
  functions take no payload argument and return `JobResult<()>`.
- Generated one-time helpers:
  `enqueue`, `enqueue_with`, `schedule_in`, `schedule_in_with`,
  `schedule_at`, and `schedule_at_with`.
- `every = "..."` supports `ms`, `s`, `m`, `h`, and `d` units.
- `cron = "..."` uses the `cron` crate expression format
  (`sec min hour day-of-month month day-of-week year`).
- The macro rejects zero intervals, invalid duration strings, invalid
  cron expressions, payload arguments on periodic jobs, and zero-arg
  manually enqueued jobs at compile time.
- `pocopine run` / `pocopine dev` can spawn a configured worker binary:

```toml
[package.metadata.pocopine]
bin = "server"
worker-bin = "worker"
```

`JobClient::from_env` and `Worker::from_env` select the backend with:

- `POCOPINE_JOB_BACKEND=memory` for the process-local memory backend.
- `POCOPINE_JOB_BACKEND=redis` plus `POCOPINE_REDIS_URL=...` for Redis.
- If `POCOPINE_JOB_BACKEND` is unset and `POCOPINE_REDIS_URL` is set, Redis
  is selected.
- If neither variable is set, memory is selected.
- `POCOPINE_JOB_VISIBILITY_MS` configures Redis pending-entry reclaim
  timeout and defaults to `60000`.
- `POCOPINE_JOB_PERIODIC_CATCH_UP_MAX` configures the maximum missed
  periodic slots a worker enqueues in one loop and defaults to `16`.
- `POCOPINE_JOB_CONSUMER` overrides the Redis Streams consumer name. If it
  is unset, Pocopine derives a process-unique name from host, pid, and a
  process token.

`pocopine dev` injects `POCOPINE_REDIS_URL=redis://127.0.0.1/` into
spawned server and worker binaries when the variable is not already set so
the default dev path exercises separate server/worker processes. It still
rejects `POCOPINE_JOB_BACKEND=memory` when a `worker-bin` is configured.
`pocopine run` and direct `cargo run` do not inject a production default.
When `pocopine run` sees a configured `worker-bin`, it rejects the
process-local memory backend and requires a Redis URL because the server
and worker are separate processes.

A configured `worker-bin` is a separate process. With the memory backend,
server-to-worker enqueueing across binaries is impossible because the queue
is not shared; use Redis for that shape. Memory is appropriate for embedded
workers or periodic/background work owned inside one process.

## 3. Non-goals

- No durable in-memory broker or cross-process memory sharing.
- No result storage API.
- No dashboard or job inspection UI.

## 4. Failure Model

Jobs are at-least-once. Handlers must be idempotent for effects that cannot
be safely repeated.

The worker acknowledges a stream entry only after the job succeeds, is
scheduled for retry, or is moved to the dead stream. Delayed jobs and
retry jobs are promoted by polling the sorted set. Stale pending jobs are
reclaimed via Redis stream claiming before each read loop.

Redis workers use Redis `TIME` for scheduler and retry due-time
comparisons. This keeps workers on different hosts from promoting jobs early
because their local clocks disagree. Redis-backed `schedule_in` also bases
relative delays on Redis `TIME`; `schedule_at` remains an explicit absolute
timestamp chosen by the caller.

Retries use exponential backoff with jitter and go through the sorted-set
scheduler instead of being immediately re-added to the ready stream. A
reclaimed stale job consumes another attempt before it is re-run; if it
has already reached the max attempt count it is moved to the dead stream.
The worker loop logs Redis/runtime errors, sleeps with capped backoff, and
reconnects instead of exiting on a transient Redis failure.

Redis state transitions that cross queue structures are Lua-scripted:
scheduled promotion removes from the sorted set and writes to the ready
stream atomically, and retry/dead-letter paths acknowledge the original
stream entry in the same script that records the next state.

`JobClient` caches a multiplexed Redis connection so burst enqueueing does
not open a TCP connection per job. The worker clears that cache after a
Redis loop error before reconnecting with backoff.

This RFC targets single-instance Redis or an operator-managed proxy that
preserves a primary endpoint across failover. Native Redis Cluster
`MOVED`/`ASK` handling and Sentinel discovery are not part of this slice.

The memory backend uses the same envelope, scheduled queue, retry backoff,
dead-letter, and periodic slot semantics inside a process-local store. It
does not use consumer groups or stale pending reclaim because a handler is
run directly by the worker task. If a handler hangs, the worker task is
occupied; if the process restarts, queued memory jobs are lost.

Job handlers run on a `tokio::spawn` task so a panicking handler is
captured by the runtime, converted into a `JobError::Handler`, and routed
through the same retry/dead-letter path as a normal `Err` return. This
applies to both backends.

The memory dead-letter buffer is capped (oldest entries dropped first) and
exposed via `Worker::drain_dead_letter() -> Vec<DeadLetter>` so embedded
operators can periodically scrape and persist failed jobs. The Redis
backend does not implement `drain_dead_letter`; the dead-letter stream
`pocopine:{app}:dead` is queryable directly.

`Worker::run` logs a one-line backend banner at startup so operators can
confirm whether the worker bound to Redis or to the process-local memory
backend, defending against the silent-default footgun in multi-process
deployments where Redis was intended.

`visibility_timeout` is part of the worker contract and is configured by
`POCOPINE_JOB_VISIBILITY_MS`: handlers should normally finish well under
that timeout or be idempotent enough to handle reclaim/retry. Periodic jobs
follow the same reclaim-attempt accounting as manual jobs; a later periodic
firing creates a fresh envelope.

Job IDs include timestamp, host/process identity, and process-local counter
so multiple workers do not mint the same id during the same millisecond and
retry jitter does not collapse across hosts. The separator is `:` because
the id is an internal metadata value, not a stable external format.

Periodic jobs are not executed directly by the scheduler loop. The worker
computes the due slot, acquires a backend-specific lock for that job/slot
(`SET NX PX` in Redis, a process-local lock map in memory), and enqueues the
job with a unit payload. From that point on the job uses the same retry and
dead-letter path as manually enqueued work.

Workers persist the last fired periodic slot per job. Cron jobs evaluate the
window `(last_fired_at_ms, now]`, capped per loop, so a slow scheduler
iteration catches up missed firings instead of dropping them permanently.
Operators can widen the per-loop cap with
`POCOPINE_JOB_PERIODIC_CATCH_UP_MAX` for explicit backfill scenarios.

## 5. Example Worker

```rust
#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> pocopine::JobResult<()> {
    use my_app as _;
    pocopine::Worker::from_env()?.run().await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
```
