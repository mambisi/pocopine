---
title: "Background jobs"
description: "The background-job runtime: Redis Streams, the scheduler, periodic firings, reclaim, and the memory backend."
---

# Background jobs — architecture

This page covers what `pocopine-jobs` does at the protocol level: what the broker stores, which invariants the algorithm preserves, and how to verify them against a live Redis.

## Mental model

A `#[pocopine::job]` function compiles to two artifacts:

1. **A static descriptor** registered through `inventory` at link
   time. `Worker::new()` walks the registry to build a name →
   handler dispatch map.
2. **Typed enqueue/schedule helpers** (`JobClient::enqueue_json`,
   `schedule_json_in`, `schedule_json_at`) that produce a JSON
   envelope and write it to the chosen backend.

The runtime has two backends behind the same enum:

```
JobBackend::Redis { url }   // durable, multi-process
JobBackend::Memory          // process-local, in-memory
```

Both share the same envelope shape, retry math, and dead-letter
contract. The Redis backend is the interesting one — the rest of this
doc covers it. The memory backend is documented at the end.

### High-level topology

```mermaid
flowchart LR
    App["app code<br/>(server fn / request handler)"]
    Client["JobClient"]
    Worker["Worker"]
    Redis[("Redis<br/>broker")]
    Handler["#[pocopine::job]<br/>handler fn"]

    App -->|"enqueue_json / schedule_json_in"| Client
    Client -->|"XADD / ZADD"| Redis
    Worker -->|"TIME / XREADGROUP / XAUTOCLAIM /<br/>EVALSHA promote, retry, dead"| Redis
    Worker --> Handler
    Handler -.->|"Ok / Err / panic"| Worker
```

The client and worker run as separate processes — the `web` and `worker` processes from the deploy contract. They communicate only through Redis; there's no in-process queue or RPC.

### Job state machine

```mermaid
stateDiagram-v2
    [*] --> Ready: enqueue
    [*] --> Scheduled: schedule_in / schedule_at
    Scheduled --> Ready: promote (due_ms reached)
    Ready --> Running: XREADGROUP delivers
    Running --> Done: Ok + XACK
    Running --> Scheduled: Err, attempt < max (retry)
    Running --> Dead: Err, attempt >= max
    Running --> Stale: idle > visibility_timeout
    Stale --> Running: XAUTOCLAIM (attempt += 1)
    Stale --> Dead: attempt >= max after bump
    Done --> [*]
    Dead --> [*]
```

Every transition is exercised by an integration test in
`crates/pocopine/tests/jobs_redis.rs`.

## Data model (Redis keys)

All keys are namespaced by the configured `app` value. Curly braces
in `{app}` are intentional — they double as a Redis Cluster hash tag
so every pocopine key for one app lands on one slot, which makes the
Lua scripts viable on cluster topologies.

| Key | Type | What's in it |
|---|---|---|
| `pocopine:{app}:queue:{queue}` | Stream | Ready queue. Entries here are eligible for `XREADGROUP`. |
| `pocopine:{app}:scheduled` | Sorted Set | Future-due envelopes. Score is `due_ms` (broker time, ms since epoch); member is the JSON envelope. |
| `pocopine:{app}:dead` | Stream | Dead-letter — jobs that exhausted their retry budget, with the failure message and full envelope captured as fields. |
| `pocopine:{app}:periodic:{job}:{due_ms}` | String + TTL | One-shot lock per (job, slot). `SET NX PX <ttl>` so only one worker enqueues each periodic firing. Auto-expires before the next slot. |
| `pocopine:{app}:periodic:last:{job}` | String (`u64`) | Last fired slot for the periodic job, used for cron catch-up after worker downtime. |

The envelope itself:

```json
{
  "job_id":           "1a2b3c:hostname_pid_started:42",
  "job_name":         "blog::record_post_view",
  "queue":            "blog",
  "payload":          { /* serde_json::Value */ },
  "attempt":          1,
  "max_attempts":     4,
  "created_at_ms":    1714680000000,
  "scheduled_for_ms": null
}
```

`job_id` is `{ms:x}:{host}_{pid:x}_{started_at:x}:{counter:x}` so two
workers on different hosts cannot collide even at the same wall-clock
millisecond, and two restarts of the same process can't collide either
(the `started_at` nanoseconds differ).

## The worker loop

```mermaid
flowchart TD
    Start(["run_redis_once"]) --> T["① now_ms = Redis TIME"]
    T --> P["② enqueue_due_periodic_jobs"]
    P --> M["③ promote_due_jobs"]
    M --> R["④ reclaim_stale_jobs"]
    R --> X["⑤ read_ready_jobs<br/>(XREADGROUP)"]
    X --> Z(["return handled count"])
    Z -->|"handled == 0"| Sleep["sleep scheduler_interval"]
    Sleep --> Start
    Z -->|"handled > 0"| Start
```

```rust
async fn run_redis_once(&self, conn: &mut MultiplexedConnection) -> JobResult<usize> {
    let now_ms = redis_time_ms(&mut *conn).await?;             // ①
    self.enqueue_due_periodic_jobs(&mut *conn, now_ms).await?; // ②
    self.promote_due_jobs(&mut *conn, now_ms).await?;          // ③
    self.reclaim_stale_jobs(&mut *conn).await?;                 // ④
    self.read_ready_jobs(conn).await                            // ⑤
}
```

Order matters:

- **① TIME comes from Redis, not `SystemTime::now()`.** All due-time
  comparisons (`now_ms` vs scheduled score, vs cron next-fire, etc.)
  use the broker's clock so worker clock skew doesn't cause premature
  promotion or missed firings.
- **② Periodic before promote** so a periodic firing this tick can be
  consumed in the same iteration (no extra round-trip).
- **③ Promote before reclaim** so retries scheduled in earlier
  iterations land in the stream before reclaim/read look at it.
- **④ Reclaim before read** so abandoned entries from dead workers are
  picked up before the new-entry path. Reclaimed entries run
  inline in step ④; step ⑤ only returns *new* entries.
- **⑤ Read new** — `XREADGROUP ... >` skips PEL entirely.

The outer `Worker::run()` loop wraps `run_redis_once` with capped
exponential-backoff reconnect on transient errors and logs a one-line
backend banner through `tracing` so operators can see at startup which
backend is bound.

## Lifecycle 1 — happy path

```mermaid
sequenceDiagram
    autonumber
    participant App as App code
    participant Cli as JobClient
    participant R as Redis
    participant W as Worker
    participant H as Handler

    App->>Cli: enqueue_json(job_name, queue, payload)
    Cli->>R: XADD pocopine:{app}:queue:Q *<br/>job_id, attempt=1, payload, …
    R-->>Cli: stream-id

    loop run loop
        W->>R: TIME
        R-->>W: now_ms
        W->>R: XREADGROUP GROUP g c COUNT N STREAMS Q >
        R-->>W: stream-id, envelope
        W->>H: tokio::spawn(handler(payload))
        H-->>W: Ok(())
        W->>R: XACK Q g stream-id
        R-->>W: 1
    end
```

Why `run_handler_safely`? It runs the handler future under
`tokio::spawn` so a panic becomes `JoinError::is_panic()` →
`JobError::Handler(...)`. Without that wrapper, a panic would unwind
through `run_once` and crash the worker; with the wrapper it routes
through the same retry/dead-letter path as a normal `Err`.

The stream entry is **not** deleted by `XACK`. `XACK` only removes
the entry from the consumer group's PEL ("Pending Entries List").
The stream itself is append-only; pocopine doesn't `XTRIM` or `XDEL`.
For high-throughput apps, configure Redis-side stream-trim (e.g.
`XADD ... MAXLEN ~ 10000`) — that's a follow-up.

### Verifying

```sh
# Before consume
XLEN pocopine:my-app:queue:emails              # 1
XPENDING pocopine:my-app:queue:emails group    # 0 pending

# After XREADGROUP, before XACK
XPENDING pocopine:my-app:queue:emails group    # 1, owner=consumer-name

# After XACK
XPENDING pocopine:my-app:queue:emails group    # 0
XLEN pocopine:my-app:queue:emails              # still 1 — the entry is in the log
```

## Lifecycle 2 — failure → retry → success or dead-letter

```mermaid
sequenceDiagram
    autonumber
    participant W as Worker
    participant R as Redis
    participant H as Handler

    Note over W: attempt 1, fails
    W->>H: tokio::spawn(handler)
    H-->>W: Err(_)
    W->>R: TIME
    R-->>W: now_ms
    Note over W: due_ms = now + retry_delay(2)
    W->>R: EVALSHA SCHEDULE_RETRY_AND_ACK<br/>XACK + ZADD scheduled
    R-->>W: 1

    Note over W,R: ...retry_delay later, next loop iter...

    W->>R: ZRANGEBYSCORE scheduled -inf now_ms
    R-->>W: [raw envelope (attempt=2)]
    W->>R: EVALSHA PROMOTE_SCHEDULED<br/>ZREM + XADD ready
    R-->>W: stream-id (or 0 if a peer won the race)
    W->>R: XREADGROUP ... >
    R-->>W: stream-id, envelope (attempt=2)
    W->>H: tokio::spawn(handler)

    alt handler succeeds
        H-->>W: Ok(())
        W->>R: XACK Q g stream-id
    else attempt < max_attempts
        H-->>W: Err(_)
        Note over W: schedule attempt=3 …
    else attempt >= max_attempts
        H-->>W: Err(_)
        W->>R: EVALSHA DEAD_LETTER_AND_ACK<br/>XACK + XADD dead
    end
```

When `run_handler_safely` returns `Err(_)`:

```rust
if envelope.attempt < envelope.max_attempts {
    retry.attempt += 1;
    let due_ms = redis_time_ms(&mut *conn).await?
        .saturating_add(retry_delay_ms(retry.attempt, &retry.job_id));
    retry.scheduled_for_ms = Some(due_ms);
    schedule_retry_and_ack(conn, scheduled_key, stream, group,
                           stream_id, &retry, due_ms).await?;
} else {
    self.move_to_dead_and_ack(conn, stream, stream_id, &envelope,
                              &err.to_string(), Some(elapsed_ms(started))).await?;
}
```

`retry_delay_ms` is exponential with cap and hash-jittered span:

```
attempt    base       jitter span    delay range
─────────  ─────────  ─────────────  ─────────────
2          1000 ms    ≤ 200 ms       1.0 – 1.2 s
3          2000 ms    ≤ 400 ms       2.0 – 2.4 s
4          4000 ms    ≤ 800 ms       4.0 – 4.8 s
5          8000 ms    ≤ 1600 ms      8.0 – 9.6 s
…          …          …              capped at 60 s
```

The jitter is hashed from `(job_id, attempt)` so the same envelope
gets the same delay on every replay, but different jobs spread out.
Job-id hashing means cross-host collisions in the id were a real
concern — the `host_pid_started` portion of the id makes it negligible.

### `SCHEDULE_RETRY_AND_ACK_SCRIPT`

```lua
local acked = redis.call('XACK', KEYS[2], ARGV[3], ARGV[4])
-- KEYS[2] = stream, ARGV[3] = group, ARGV[4] = stream_id
if acked == 0 then return 0 end                -- already acked? skip
redis.call('ZADD', KEYS[1], ARGV[1], ARGV[2])
-- KEYS[1] = scheduled, ARGV[1] = due_ms (score), ARGV[2] = raw envelope JSON
return acked
```

**Why Lua.** If the worker crashes between `XACK` and `ZADD` we'd
have an acked entry with no retry — a silent loss. Redis runs Lua
scripts to completion without interleaving, so either both writes
land or neither does (broker-crash + AOF recovery still preserves the
all-or-nothing property at fsync boundaries).

**Why XACK first.** If we did ZADD first and the script aborted before
XACK, the entry would be in PEL *and* in scheduled — eventually
reclaim would re-run the same envelope a second time. Acking first +
checking the result avoids that duplication; the worst case becomes
"never run again," which is fine because `attempt` is preserved in the
new envelope and reclaim doesn't fire on already-acked entries.

The retry then waits in the sorted set until the next promote tick:

```rust
async fn promote_due_jobs(conn, now_ms):
    let raw_jobs: Vec<String> =
        ZRANGEBYSCORE scheduled -inf now_ms LIMIT 0 100
    for raw in raw_jobs:
        let envelope = serde_json::from_str(&raw)  // decode to get queue name
        promote_scheduled_envelope(conn, scheduled_key, queue_key, raw, envelope)
```

### `PROMOTE_SCHEDULED_SCRIPT`

```lua
local removed = redis.call('ZREM', KEYS[1], ARGV[1])
if removed == 0 then return 0 end
return redis.call('XADD', KEYS[2], '*',
                  'job_id', ARGV[2],
                  'job_name', ARGV[3],
                  'queue', ARGV[4],
                  'payload', ARGV[5],
                  'attempt', ARGV[6],
                  'max_attempts', ARGV[7],
                  'created_at_ms', ARGV[8],
                  'scheduled_for_ms', ARGV[9])
```

**Why Lua.** Two workers can both `ZRANGEBYSCORE` and see the same
envelope simultaneously. The `ZREM` inside the script is the claim:
the first worker gets `removed = 1` and proceeds to `XADD`, the
second gets `removed = 0` and bails. Without atomicity here, a worker
crash between ZREM and XADD silently drops the envelope.

> **Regression we caught:** an earlier version did the `ZREM` in Rust
> *before* calling the script. The script's own `ZREM` then always
> returned `0` and skipped `XADD` — every promoted retry was silently
> dropped. The integration test
> `schedule_in_promotes_consumes_and_acks` is the regression check
> for this. See `crates/pocopine/tests/jobs_redis.rs`.

### `DEAD_LETTER_AND_ACK_SCRIPT`

```lua
local acked = redis.call('XACK', KEYS[2], ARGV[9], ARGV[10])
if acked == 0 then return 0 end
return redis.call('XADD', KEYS[1], '*',
                  'job_id', ARGV[1], …, 'envelope', ARGV[7], 'failed_at_ms', ARGV[8])
```

Same shape as the retry script. Atomically: ack the original stream
entry, then write a new entry to the dead-letter stream with the
final attempt count, error message, and full envelope embedded as a
field so an operator can inspect what was attempted.

### `EVALSHA` caching

All three scripts are wrapped in `redis::Script` statics
(`promote_scheduled_script`, `schedule_retry_and_ack_script`,
`dead_letter_and_ack_script`) so the redis crate `LOADs` the script
on first call and uses `EVALSHA` thereafter. Without this wrapper,
the worker would ship the full Lua source on every promote/retry/
dead-letter call.

### Verifying retry → dead-letter

Enqueue with `retries = 3` (so `max_attempts = 4`), handler always
returns `Err`:

```
T+0   XLEN queue        = 1, attempt = 1
T+0   handler runs, fails
T+0   ZCARD scheduled   = 1, score ≈ T+1s
T+1s  promote runs
T+1s  XLEN queue        = 1 (new entry, attempt = 2 in the fields)
T+1s  ZCARD scheduled   = 0
T+1s  handler runs, fails
T+~3s ... (attempt=3, then 4) ...
T+~15s XLEN dead        = 1
T+~15s ZCARD scheduled  = 0
```

`crates/pocopine/tests/jobs_redis.rs::permanently_failing_handler_lands_in_dead_letter_stream`
asserts the final state.

## Lifecycle 3 — reclaim (worker died, or handler exceeded visibility)

```mermaid
sequenceDiagram
    autonumber
    participant A as Worker A (will die)
    participant R as Redis
    participant B as Worker B

    A->>R: XREADGROUP ... >
    R-->>A: stream-id, envelope (attempt=1)
    Note over A: handler hangs<br/>or process is killed<br/>before XACK

    Note over R: stream-id remains in PEL,<br/>owner = A's consumer name

    Note over B: ...visibility_timeout (60s)...

    B->>R: XAUTOCLAIM Q g B-consumer<br/>min_idle=60000 0-0 COUNT N
    R-->>B: [stream-id, envelope]<br/>ownership transferred to B

    Note over B: envelope.attempt += 1
    B->>B: run_envelope (attempt=2)
    Note over B: routes through normal Ok / retry / dead path
```

If a worker reads an entry and never `XACK`s — crashed, OOM-killed,
network-partitioned, or just took longer than `visibility_timeout` —
the entry sits in PEL forever from that consumer's perspective.
`XAUTOCLAIM` is the recovery mechanism:

```
XREADGROUP ... >        # only delivers NEW entries; PEL is untouched
XAUTOCLAIM stream group new_consumer min_idle_ms 0-0 COUNT N
                                             ↑
                                        visibility_timeout
```

`XAUTOCLAIM` walks the PEL and reassigns ownership of any entry idle
≥ `min_idle_ms` to the calling consumer (not the original owner).
The reassigned entries are returned and pocopine processes them
through `run_envelope` with `attempt += 1` so chronically-stuck jobs
eventually exhaust the retry budget and dead-letter.

**At-least-once is the contract.** If your handler runs longer than
`visibility_timeout` (default 60s, configurable via
`POCOPINE_JOB_VISIBILITY_MS`), another worker will reclaim and
re-execute the same job. Handlers must therefore be idempotent
*or* finish well under the timeout. This is documented in
RFC-067 §4 and in the doc comment on `WorkerConfig.visibility_timeout`.

### Reclaim attempt accounting

```rust
async fn reclaim_stale_jobs(conn):
    let reply = conn.xautoclaim_options(
        stream, group, consumer, idle_ms, "0-0",
        StreamAutoClaimOptions::count(batch_size)).await?;
    for id in reply.claimed:
        let envelope = envelope_from_stream(id.map)?
        if envelope.attempt >= envelope.max_attempts:
            move_to_dead_and_ack(...)              // budget already exhausted
        else:
            envelope.attempt += 1
            run_envelope(conn, stream, id.id, envelope)  // normal Ok/retry/dead path
```

The `attempt += 1` is critical: a handler that hangs forever would
otherwise loop reclaim → re-run → reclaim indefinitely. Bumping
attempt on each reclaim consumes the retry budget like a normal
failure.

### Verifying reclaim

In one shell, enqueue and have a worker pick up an entry but never
ack it (e.g. handler that sleeps 70s with `visibility_timeout = 60s`).
Watch `XPENDING`:

```sh
redis-cli XPENDING pocopine:my-app:queue:slow group
# 1) "1"                                                # 1 entry pending
# 2) "stream-id-of-entry"
# 3) "stream-id-of-entry"
# 4) 1) 1) "consumer-A"
#       2) "1"
```

After 60s + the next worker's reclaim loop, the entry's owner flips
to whatever consumer ran reclaim. If the handler still doesn't
complete, eventually `attempt == max_attempts` and the entry moves to
the dead stream.

## Lifecycle 4 — periodic jobs

```mermaid
sequenceDiagram
    autonumber
    participant A as Worker A
    participant B as Worker B
    participant R as Redis

    Note over A,B: tick T+0: both compute the same due_ms

    A->>R: GET pocopine:{app}:periodic:last:job
    R-->>A: nil (never fired)
    A->>R: SET pocopine:{app}:periodic:job:T 1 NX PX <ttl>
    R-->>A: OK (locked)
    A->>R: XADD ready stream (envelope, payload=())
    A->>R: SET pocopine:{app}:periodic:last:job T

    B->>R: GET pocopine:{app}:periodic:last:job
    R-->>B: T (already fired)
    Note over B: due_periodic_slots returns []<br/>(skip this slot)

    Note over A,B: ...next due slot T'...

    B->>R: GET periodic:last:job
    R-->>B: T
    Note over B: T' > T → due
    B->>R: SET periodic:job:T' 1 NX PX <ttl>
    R-->>B: OK (B wins this time)
    B->>R: XADD ready stream
    B->>R: SET periodic:last:job T'
```

`#[pocopine::job(every = "15m")]` and `#[pocopine::job(cron = "...")]`
register a periodic descriptor. Periodic jobs are not executed by the
scheduler loop directly — the scheduler computes due slots and
enqueues them like normal jobs.

```rust
async fn enqueue_due_periodic_jobs(conn, now_ms):
    for descriptor in periodic descriptors with matching queue:
        last_fired_ms = GET pocopine:{app}:periodic:last:{job}
        for due_ms in due_periodic_slots(schedule, now_ms,
                                          scheduler_interval, last_fired_ms,
                                          max_periodic_catch_up):
            // Lock the slot. SET NX PX is atomic on the broker.
            locked: Option<String> = SET pocopine:{app}:periodic:{job}:{due_ms}
                                         1 NX PX <ttl>
            if locked.is_none():
                continue                  // another worker won this slot
            XADD ready_stream (envelope payload = ())
            SET pocopine:{app}:periodic:last:{job} {due_ms}
```

Two failure modes the lock defends against:

1. **Multi-worker race in the same slot.** Workers A and B both
   compute `due_ms = T`. Only one wins `SET NX PX` and enqueues; the
   other gets `nil` and skips.
2. **Catch-up after downtime.** If a worker was offline through 5
   firings, `due_periodic_slots` returns up to
   `max_periodic_catch_up` past slots (default 16). Each one needs
   its own lock so multiple workers coming back online don't all
   replay the backlog. The TTL is the inter-firing interval × 2 (or
   7 days for cron) so old slots can't accidentally re-fire after
   state is forgotten.

### `due_periodic_slots`

```rust
fn due_periodic_slots(schedule, now_ms, scheduler_interval,
                      last_fired_ms, max_catch_up):
    match schedule {
        Every { interval_ms } => {
            let due_ms = (now_ms / interval_ms) * interval_ms;
            if last_fired_ms.is_some_and(|last| last >= due_ms) {
                vec![]                  // already fired this slot
            } else {
                vec![due_ms]
            }
        }
        Cron { expr } => {
            let window_start = match last_fired_ms {
                Some(last) => DateTime::from_timestamp_millis(last),
                None       => now - scheduler_interval,
            };
            Schedule::from_str(expr)
                .after(&window_start)
                .take(max_catch_up)         // cap on catch-up
                .filter_map(|d| (d.timestamp_millis() as u64 <= now_ms)
                                 .then_some(d.timestamp_millis() as u64))
                .collect()
        }
    }
```

For `every`, only one slot is returned per tick — the most recent
boundary that hasn't fired. Cron fans out missed firings.

### Verifying periodic firings

```sh
# With #[pocopine::job(every = "5s")]:
redis-cli --scan --pattern 'pocopine:my-app:periodic:*'

# pocopine:my-app:periodic:my::job::name:1714680000000   (slot lock, TTL ~10s)
# pocopine:my-app:periodic:last:my::job::name             (last fired)
```

The slot lock keys come and go with TTL; the `:last:` key is updated
in lockstep with each successful enqueue.

## Atomicity guarantees, summarized

| Transition | Mechanism | What breaks without it |
|---|---|---|
| Schedule retry + ack original | `SCHEDULE_RETRY_AND_ACK_SCRIPT` | Crash between XACK and ZADD = job lost. |
| Promote scheduled → ready | `PROMOTE_SCHEDULED_SCRIPT` | Two workers double-promote, or crash between ZREM and XADD = lost. |
| Dead-letter + ack original | `DEAD_LETTER_AND_ACK_SCRIPT` | Crash between XACK and dead XADD = no record of the failure. |
| Periodic slot dedup | `SET NX PX` | All workers enqueue every slot independently → N×duplicate firings. |
| Promote claim race | ZREM-as-claim inside the Lua script | Two workers both `XADD` the same envelope = duplicate run. |
| Reclaim ownership transfer | `XAUTOCLAIM` (atomic broker-side) | Multiple workers think they own the same entry = duplicate run. |

## Configuration knobs

| Field / env var | Default | What it controls |
|---|---|---|
| `WorkerConfig.visibility_timeout` / `POCOPINE_JOB_VISIBILITY_MS` | 60 000 ms | `XAUTOCLAIM` `min_idle_ms`. Handlers must finish well under this or be idempotent under reclaim. |
| `WorkerConfig.scheduler_interval` | 1 000 ms | Sleep when `run_redis_once` returns 0 work. Bounds the worst-case latency between "scheduled time reached" and "promoted to stream." (code only; no env var) |
| `WorkerConfig.block_ms` | 1 000 ms | `XREADGROUP BLOCK` timeout. **Special case:** `0` means non-blocking poll, *not* "block forever" — the worker omits `BLOCK` from the command so callers using `run_once` against an idle stream don't deadlock. (code only; no env var) |
| `WorkerConfig.batch_size` | 10 | `XREADGROUP COUNT` and reclaim/promote batch size. (code only; no env var) |
| `WorkerConfig.max_periodic_catch_up` / `POCOPINE_JOB_PERIODIC_CATCH_UP_MAX` | 16 | Cap on missed periodic firings backfilled per loop iteration. |
| `POCOPINE_JOB_BACKEND` | (auto) | `memory` or `redis`. If unset and `POCOPINE_REDIS_URL` is set → redis; otherwise → memory. |
| `POCOPINE_REDIS_URL` | — | Standard `redis://[user[:pass]]@host:port/[db]` connection string. |

## Failure modes & invariants

**No silent drops.** Every entry that enters the ready stream or the
scheduled set leaves only via:

- successful execution + `XACK`, or
- exhaust attempts → `XADD` to dead + `XACK`, or
- crash + `XAUTOCLAIM` reclaim (back to one of the above)

**Idempotent acks.** `XACK` returns 0 for already-acked entries. The
Lua scripts check this and short-circuit the next-state write, so a
re-run of the same script doesn't double-write to scheduled or dead.

**At-least-once.** Documented and enforced by the visibility-timeout
contract.

**At-most-one-per-slot for periodic.** Defended by `SET NX PX` and
`pocopine:{app}:periodic:last:{job}`.

**Retry budget honored.** `attempt < max_attempts` is checked in
`run_envelope` for the normal path and in `reclaim_stale_jobs` for
the timeout path. Both routes converge on the same dead-letter
script.

**Worker crashes are recoverable.** The worker's connection cache is
cleared on errors, then `run()` retries with capped exponential
backoff. PEL entries from the dead worker process are reclaimed by
any other worker (or by the same worker after restart) via
`XAUTOCLAIM`.

## Out of scope (RFC-067 §4)

- **Native Redis Cluster `MOVED`/`ASK` handling.** Pocopine targets a
  single-instance Redis or an operator-managed proxy that preserves
  a primary endpoint across failover.
- **Sentinel auto-discovery.** Same — operators provide a stable URL.
- **In-memory durability.** The memory backend is process-local;
  state is lost on restart.

## Memory backend

Same envelope shape, retry math, periodic semantics, and dead-letter
flow — implemented inside a `Mutex<MemoryState>` keyed by `app`
namespace inside a process-global registry.

```rust
struct MemoryState {
    ready: VecDeque<JobEnvelope>,                    // ready queue
    scheduled: Vec<JobEnvelope>,                     // future-due
    dead: VecDeque<(JobEnvelope, String)>,           // capped dead-letter
    periodic_locks: HashMap<String, u64>,            // slot → expires_at
    periodic_last_fired: HashMap<String, u64>,       // job → last fired
}
```

Differences from the Redis backend:

- **No consumer groups.** A handler runs directly inside the worker
  task. There's no PEL and no reclaim path.
- **Panic safety still routes through dead-letter.**
  `run_handler_safely` uses `tokio::spawn` + `JoinError::is_panic()`
  for both backends, so a panicking handler is captured and routed
  through retry/dead-letter rather than unwinding the worker.
- **Dead-letter is capped** at `DEFAULT_MEMORY_DEAD_LETTER_CAP = 1024`
  entries (oldest evicted on overflow) and exposed via
  `Worker::drain_dead_letter() -> Vec<DeadLetter>`. Operators using
  the embedded worker should drain periodically and persist the
  results.
- **Process-local.** Two separately-launched processes both pick the
  memory backend by default if neither env is set, and never share
  state. `Worker::run` logs a `tracing::warn` at startup naming the
  backend, so the misconfiguration is visible in the startup logs.

When to pick which:

- **Redis** for any deployment where server and worker are separate
  processes, or where the queue must survive restarts, or where you
  want to scale workers horizontally.
- **Memory** for embedded workers, single-process production
  deployments where losing in-flight jobs on restart is acceptable,
  and tests.

## Verifying a running deployment

The shortest path is `redis-cli` against the broker. With a
testcontainer:

```sh
docker ps                                         # find the container id
docker exec -it <id> redis-cli

> KEYS pocopine:*
> XLEN pocopine:my-app:queue:emails
> XPENDING pocopine:my-app:queue:emails group-name
> ZRANGE pocopine:my-app:scheduled 0 -1 WITHSCORES
> XRANGE pocopine:my-app:dead - +
```

Live tracing:

```sh
> MONITOR        # prints every command the worker issues
```

The `EVALSHA` script lookups, `XADD`/`XREADGROUP`/`XACK`/`ZADD`/
`ZREM`/`XAUTOCLAIM` stream is exactly what `pocopine-jobs` is doing
on the wire. If you see anything in `MONITOR` that doesn't match one
of the lifecycle diagrams above, that's worth a bug report.

## Code locations

All line numbers are approximate. If the code and this page disagree,
the code is authoritative — open a PR.

| Concern | Where |
|---|---|
| Lua script sources | `crates/pocopine-jobs/src/lib.rs:114-175` |
| `EVALSHA` wrapper functions | `~:180-193` |
| `run_redis_once` (loop body) | `~:771-777` |
| `enqueue_due_periodic_jobs` (Redis) | `~:866-917` |
| `promote_due_jobs` | `~:919-948` |
| `reclaim_stale_jobs` | `~:951-990` |
| `read_ready_jobs` (incl. `block_ms == 0` special case) | `~:993-1033` |
| `run_envelope` (Redis) | `~:1035-1096` |
| `run_envelope_memory` | `~:1098-1139` |
| `run_handler_safely` (panic capture) | `~:1586-1605` |
| `due_periodic_slots` | `~:1748-1798` |
| `retry_delay_ms` (backoff math) | `~:1810-1818` |
| `new_job_id` (job-id construction) | `~:1830-1834` |
| `MemoryState` struct | `~:227-233` |
| Integration tests | `crates/pocopine/tests/jobs_redis.rs` |
| Unit tests (math, parsers, ids) | `crates/pocopine-jobs/src/lib.rs` `mod tests` (~:1897) |
