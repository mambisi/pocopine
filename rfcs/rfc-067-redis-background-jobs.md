# RFC 067 - Redis-backed background jobs

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
enqueue/schedule helpers, and keeps Redis/worker code out of wasm
bundles. A new `pocopine-jobs` crate owns the Redis runtime.

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

`pocopine dev` injects `POCOPINE_REDIS_URL=redis://127.0.0.1/` into
spawned server and worker binaries when the variable is not already set.
`pocopine run` and direct `cargo run` do not inject a production default;
`JobClient::from_env` and `Worker::from_env` fail fast unless
`POCOPINE_REDIS_URL` is set.

## 3. Non-goals

- No in-memory production fallback.
- No result storage API.
- No dashboard or job inspection UI.

## 4. Failure Model

The worker acknowledges a stream entry only after the job succeeds, is
scheduled for retry, or is moved to the dead stream. Delayed jobs and
retry jobs are promoted by polling the sorted set. Stale pending jobs are
reclaimed via Redis stream claiming before each read loop.

Retries use exponential backoff with jitter and go through the sorted-set
scheduler instead of being immediately re-added to the ready stream. A
reclaimed stale job consumes another attempt before it is re-run; if it
has already reached the max attempt count it is moved to the dead stream.
The worker loop logs Redis/runtime errors, sleeps with capped backoff, and
reconnects instead of exiting on a transient Redis failure.

`visibility_timeout` is part of the worker contract: handlers should
normally finish well under that timeout or be idempotent enough to handle
reclaim/retry. Periodic jobs follow the same reclaim-attempt accounting as
manual jobs; a later periodic firing creates a fresh envelope.

Job IDs include timestamp, process id, and process-local counter so
multiple local workers do not mint the same id during the same
millisecond.

Periodic jobs are not executed directly by the scheduler loop. The worker
computes the due slot, acquires a Redis `SET NX PX` lock for that job/slot,
and enqueues the job with a unit payload. From that point on the job uses
the same stream, retry, and dead-letter path as manually enqueued work.

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
