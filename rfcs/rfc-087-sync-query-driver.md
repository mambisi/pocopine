# RFC 087 — `pocopine-sync-query` driver lifecycle

* **Status:** Draft
* **Author:** sync framework working group
* **Tracking branch:** `wip/sync-query-driver`
* **Supersedes:** the "background-task drivers" line item in `pocopine-sync-query` README's roadmap
* **Related:** [RFC 086 (pocopine-sync-query)](./rfc-086-sync-query.md), [RFC 071 (event spine + live invalidation)](./rfc-071-event-spine.md), [RFC 072 (offline sync protocol)](./rfc-072-offline-sync.md)

## Summary

RFC 086 shipped `pocopine-sync-query` as a routing engine + macro DSL: queries are typed, subscriptions are refcounted, mutations are predicate-routed into matching views, and the wire envelopes are built. What's **missing** is the actual sync loop — nothing today issues `/open` + `/pull` against the server, nothing subscribes to the live-wakeup channel, and nothing replays queued mutations after a disconnect. `client.observe(q)` returns a `QueryView` with empty canonical state; the only way data flows in is via the caller manually pushing mutation results through `Mutator::apply_remote`.

This RFC closes the gap with a **per-subscription background driver**. When a subscription is created, the `QueryClient` spawns a driver task that owns the `/open` handshake, the incremental `/pull` loop, live-wakeup invalidation routing, and offline mutation replay. When the last `QueryHandle` to a subscription drops, the driver's epoch is bumped and the task exits at its next yield point. The driver is a sync-query-native abstraction, not a retrofit of `pocopine-sync::SyncCollection<C, T>` — `SyncCollection` is CRUD-resource-centric and reusing it would re-create the architectural tension that drove the `pocopine-sync-crud` / `pocopine-sync-query` split in the first place.

## Motivation

Post-PR #146, the surface looks like a working framework but doesn't actually sync:

| Capability                                | Today                                                 | This RFC                                               |
| ----------------------------------------- | ----------------------------------------------------- | ------------------------------------------------------ |
| Routing engine                            | ✅ predicate-routed; correct under cancellation       | unchanged                                              |
| Macro DSL                                 | ✅ `#[query_resource]` + `#[query_param]`             | unchanged                                              |
| Wire builders                             | ✅ `build_open_request` / `build_pull_request` etc.   | consumed by the driver                                 |
| Initial population (`/open` + `/pull`)    | ❌ nothing calls them                                 | driver issues both on subscribe                        |
| Incremental sync                          | ❌ no periodic `/pull`                                | driver loops on cursor + heartbeat                     |
| Live invalidation                         | ❌ no listener attached                               | driver consumes `pocopine-live` events                 |
| Offline replay                            | ❌ failed pushes just error out                       | driver queues + retries on reconnect                   |
| Schema-version drift                      | ❌ stale state silently retained                      | driver resets state + re-opens on mismatch             |
| Subscription cancellation                 | ✅ Drop bumps refcount                                | Drop also bumps `SyncEpoch`; task exits at next yield  |

The user model post-RFC 087: declare a query, call `observe`, get a live view. No manual `apply_remote` glue for canonical data — only `Mutator::apply_remote` for client-initiated writes (which is the right place for that seam).

## Goals

* **Per-subscription driver task.** One sync-query-native lifecycle per `(TypeId, QueryKey)` — same identity as the registry.
* **Reuse pocopine-sync's wire layer.** `SyncStreamSubscription`, `SyncPullRequest`, `SyncPushRequest`, `validate_params`, `local_stream_key` — all unchanged, all consumed by the driver.
* **Reuse pocopine-live's invalidation channel.** Per-collection topic + client-side params filter is v1; per-`(name, params_hash)` topics is a v2 perf concern (not in scope).
* **Cancellation safety.** Tasks observe the same `SyncEpoch` as the subscription; epoch bump on last `QueryHandle::drop` causes the task to exit at its next `.await`.
* **`apply_remote` stays the push transport seam.** The driver does NOT take over `/push` — `Mutator::apply_remote` remains user-owned. The driver only owns `/open`, `/pull`, live wakeup, and offline replay (which re-invokes `apply_remote`).
* **Runtime-agnostic spawn shim.** `#[cfg(target_arch = "wasm32")]` uses `wasm_bindgen_futures::spawn_local`; native uses `tokio::spawn`. No public `Runtime` trait — keeps the user API the same on both targets.
* **Persistence-agnostic.** Driver works against in-memory `QueryState`; durable storage (IndexedDB, SQLite) is layered on later via the same `SyncLocalStore` interface used by `pocopine-sync`. Out of scope for this RFC.

## Non-goals

* **Persistent state across page reloads.** In-memory only this RFC. Persistence is a follow-up against a `SyncLocalStore`-shaped trait.
* **Per-`(name, params_hash)` live topics.** Server pushes on the per-collection topic; clients filter by their captured params. v2 perf optimization.
* **`#[query]` selector composition (Layer 2).** Composed views with read-tracking belong in a separate RFC. This RFC is just the single-query lifecycle.
* **Optimistic conflict resolution.** Server is authoritative; conflicts surface as failed `apply_remote` and flow through the existing rollback path (RFC 086 §3).
* **CRUD-Source → QuerySource adapter.** Filed as a follow-up; lets existing `CrudSource` impls be served by sync-query queries without rewriting.

## Design

### 1. Subscription lifecycle state machine

A `QuerySubscription<Row>` (from RFC 086) gains a `driver: Cell<Option<DriverHandle>>` field. The driver's state machine:

```
                  observe()
                     │
                     ▼
              ┌─────────────┐
              │  Spawning   │  (driver_handle.spawn → returns DriverHandle)
              └──────┬──────┘
                     ▼
              ┌─────────────┐
              │   Opening   │  (POST /open → cursor, schema_version, collection)
              └──────┬──────┘
                     ▼
              ┌─────────────┐
              │   Pulling   │  (POST /pull → snapshot/delta into canonical_rows)
              └──────┬──────┘
                     ▼
              ┌─────────────┐
              │    Idle     │  ◄─── live wakeup OR poll-interval timeout
              └──────┬──────┘
                     ▼
              ┌─────────────┐
              │  Replaying  │  (walk pending; re-invoke apply_remote for queued)
              └──────┬──────┘
                     │
                     └──── back to Idle
              
                 epoch.bump()
                     │
                     ▼
              ┌─────────────┐
              │  Cancelled  │  (any .await checks epoch.is_current; bails)
              └─────────────┘
```

State transitions are reactive in the sense that the QueryState's `version()` ticks on every canonical mutation — observers (UI) re-render. Driver state transitions don't themselves bump version unless they change canonical/pending data.

### 2. Runtime abstraction (cfg-split spawn shim)

```rust
// In crates/pocopine-sync-query/src/driver.rs:

#[cfg(target_arch = "wasm32")]
fn spawn_driver<F>(fut: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(fut);
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_driver<F>(fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    // Use the tokio handle the host runtime already installed. Panics
    // if there's no current tokio runtime — host tests + native apps
    // start one explicitly; the panic surfaces a real misconfiguration.
    tokio::spawn(fut);
}
```

The `Send` bound on native differs from wasm (wasm is single-threaded, doesn't need Send). The driver type itself avoids `!Send` types (Rc, RefCell) on the await boundary by carrying a `Weak<QuerySubscription<Row>>` and upgrading inside each tick.

Actually, **important detail**: `QuerySubscription` holds `Rc<RefCell<QueryState<Row>>>`. `Rc` is not `Send`. To make the future `Send` on native, the driver task runs on a `LocalSet` or holds the subscription via a sync-friendly handle. Two paths:

1. **Native: require `tokio::task::LocalSet`** in the host runtime; spawn via `tokio::task::spawn_local`. Same single-threaded execution model as wasm. Cleaner alignment.
2. **Add `Arc<Mutex<QueryState<Row>>>`** in the subscription. Bigger surgery, breaks the existing single-threaded RefCell pattern.

This RFC picks **(1)** — `tokio::task::spawn_local` requires the caller to have a `LocalSet` available. Native test code (and `pocopine-server`) already runs in a LocalSet for the same reason CRUD's `SyncCollection` does. Wasm uses `wasm_bindgen_futures::spawn_local` which is already local.

### 3. Driver loop

```rust
pub(crate) struct SubscriptionDriver<Row: Clone + 'static> {
    /// Weak ref so the driver doesn't keep the subscription alive after
    /// the last QueryHandle drops. Each tick re-upgrades; on failure the
    /// task exits cleanly.
    subscription: Weak<QuerySubscription<Row>>,
    /// Generation token; epoch.bump() on subscription drop signals exit.
    epoch: SyncEpoch,
    /// Sync endpoint base (default "/__pocopine/sync/v1", overridable).
    endpoint: String,
    /// Configured poll interval for incremental pulls (when no live
    /// wakeup is connected, or as a fallback heartbeat).
    poll_interval: Duration,
    /// Live wakeup channel; None if disabled by config.
    live_wakeup: Option<LiveWakeup<Row>>,
}

impl<Row: Clone + Serialize + DeserializeOwned + 'static> SubscriptionDriver<Row> {
    async fn run(self) {
        // Phase 1: /open
        let open_response = match self.open().await {
            Ok(r) => r,
            Err(_) => return self.mark_error_and_exit(),
        };
        self.apply_open(open_response);
        if !self.epoch.is_current() { return; }

        // Phase 2: initial /pull
        let pull_response = match self.pull().await {
            Ok(r) => r,
            Err(_) => return self.mark_error_and_exit(),
        };
        self.apply_pull(pull_response);
        if !self.epoch.is_current() { return; }

        // Phase 3: hook live wakeup (item 2)
        let mut live_rx = self.connect_live_wakeup();

        // Phase 4: main loop
        loop {
            tokio::select! {
                _ = self.poll_interval_timer() => {
                    self.tick_pull().await;
                }
                Some(event) = live_rx.next() => {
                    if self.live_event_matches_params(&event) {
                        self.tick_pull().await;
                    }
                }
            }
            if !self.epoch.is_current() { return; }
            // Phase 5: offline replay (item 3)
            self.replay_pending_if_offline().await;
        }
    }
}
```

The `tokio::select!` shape works for native; the wasm port uses `futures::select_biased!` or a small hand-rolled state machine because wasm doesn't have tokio. To keep the source unified, the driver uses `futures::select!` (provided by the `futures` crate, works on both targets).

### 4. `/open` and `/pull`

Both call the existing `wire::build_*_request` helpers from RFC 086, then issue the HTTP call via `pocopine::fetch`'s middleware chain (so tests can install a mock middleware). The response shape is the standard `SyncOpenResponse` / `SyncPullResponse<Value>` from `pocopine-sync` — already used by `SyncCollection` for the CRUD client.

`/pull` results land in `QueryState` via the routing engine's `route_canonical_changes` (already exists from RFC 086). Each row in the response is decoded as `Row` via `serde_json::from_value`; decode failures are logged with `tracing::warn!` and the row is skipped (per RFC 086's wire-shape mismatch handling).

### 5. Schema-version drift handling

The driver remembers the `application_schema_version` from the most recent successful `/open`. On every subsequent `/open` (e.g., on reconnect), if the response's schema_version differs from the remembered one:

1. Call `QueryState::reset()` — drops canonical_rows + pending + cursor.
2. Bump `application_schema_version` to the new value.
3. Bump the subscription's `version()` so observers re-render the empty state.
4. Continue with a fresh `/pull`.

This matches `SyncCollection`'s drift behavior and the RFC 086 §3.3 reset path.

### 6. Live wakeup hookup

Reuses the existing `pocopine-live::LiveClient`:

```rust
struct LiveWakeup<Row> {
    rx: LiveSubscription,    // from pocopine-live, per-collection topic
    captured_params: StreamParams,
}

fn live_event_matches_params(&self, event: &LiveEvent) -> bool {
    // v1: server publishes affected_keys + affected_fields for the
    // stream's collection. The driver compares the event's per-row
    // field values against the subscription's params. Mismatch → drop;
    // match → trigger /pull.
    //
    // The "minimal field set" the server publishes is the SAME field
    // set the resource declares with #[query_param] — bounded
    // vocabulary, predictable wire size.
    for (key, want) in &self.captured_params {
        match event.affected_fields.get(key) {
            None => continue,                  // server didn't include this field; allow
            Some(value) if value == want => continue,
            Some(_) => return false,           // mismatch → drop event
        }
    }
    true
}
```

If `affected_fields` is empty (server doesn't track field-level deltas), every event triggers a `/pull` — bandwidth-suboptimal but correct. Documented as the v1 fallback. v2 adds per-`(name, params_hash)` topics + server-side filtering.

### 7. Offline replay

Trigger: `Mutator::apply_remote` returns a transport-error `SyncError::Network(...)`. The driver marks the subscription's state as `syncing=false, error="offline"` and leaves the optimistic overlay in `pending`. The optimistic state stays visible to the UI.

On the next successful `/pull` (which fires when the network recovers + the next live-wakeup or poll interval), the driver walks `state.pending()` and re-invokes `apply_remote` for each overlay's captured `payload` (stored in `PendingOverlay::mutation.payload`). Successful replays clear the overlay via `dequeue_pending`; persistent failures stay queued.

**Server-side dedup contract**: each mutation carries a durable `mutation_id`. Servers MUST treat duplicate `mutation_id`s as idempotent — return the prior result if already applied. This is consistent with the wire contract in RFC 072 (offline sync protocol). Documented in the cookbook (follow-up); not enforced by this RFC.

Network detection: use `pocopine::fetch`'s `FetchError::Network` variant. `SyncError::Network` wraps it.

### 8. Cancellation via SyncEpoch

`QuerySubscription` already holds a `SyncEpoch`. The driver re-checks `epoch.is_current()` after every `.await` point. On stale: return immediately, no further state mutations. `Drop` on the last `QueryHandle` bumps the epoch; the task exits at its next yield.

The `Weak<QuerySubscription<Row>>` upgrade in each tick catches the case where the subscription is dropped while the driver is between awaits — `upgrade().is_none()` → exit.

### 9. Configuration

A new `QueryClientConfig` with sensible defaults:

```rust
#[derive(Clone, Debug)]
pub struct QueryClientConfig {
    /// Base endpoint for /open, /pull, /push. Defaults to
    /// "/__pocopine/sync/v1". Override for custom routing.
    pub endpoint: String,
    /// How often the driver tick polls for /pull when no live wakeup
    /// fires. Defaults to 30s. Set to None to disable polling (rely
    /// solely on live wakeup + manual refresh).
    pub poll_interval: Option<Duration>,
    /// Disable live wakeup subscription. Defaults to false.
    pub disable_live: bool,
    /// Send cookies / credentials with /open + /pull + /push. Defaults to true.
    pub with_credentials: bool,
}
```

`QueryClient::new()` uses defaults; `QueryClient::with_config(cfg)` overrides. Existing tests don't touch config (the defaults work).

### 10. Tests

* **Unit**: driver state machine transitions against a mock `pocopine::fetch::install_middleware`. Cover /open → /pull, schema drift, live wakeup match/mismatch, offline replay.
* **Integration (host)**: spin up `pocopine-server` with a small `SyncStreamSource` impl, subscribe a `QueryClient`, mutate, verify canonical rows arrive.
* **Integration (wasm)**: same shape via `wasm-bindgen-test`.
* **Cancellation**: subscribe, mutate, drop handle, verify driver task exits within one tick.
* **Offline**: install a mock middleware that returns `FetchError::Network`, verify pending overlay persists; flip middleware to success, verify replay drains pending.

## Alternatives considered

**A. Reuse `pocopine-sync::SyncCollection<C, T>`** instead of building a sync-query-native driver. SyncCollection has all the pieces (open/pull/push/live/replay) but is tightly coupled to the CRUD reactive model (`Handle<C>`, `CollectionSelector<C, T>`, single-state-per-resource). Retrofitting it for sync-query's predicate-routed multi-state model would recreate the architectural tension that drove the crate split. Rejected.

**B. One driver task per stream, not per subscription.** Two subscriptions to the same stream with different params would share a task. Sounds efficient, but params differ → /pull cursors differ → state differs. The task would still need per-subscription cursors and state. Adds complexity for no real savings. Rejected.

**C. Take over `/push` in the driver.** Replace `Mutator::apply_remote` with a built-in HTTP push the driver fires automatically. Simpler `Mutator` API but loses transport flexibility (custom auth headers, batching, alternative endpoints, RPC instead of HTTP). Rejected per the question to the user on the design call.

**D. Public `Runtime` trait** instead of cfg-split spawn. Would let apps plug in actix's runtime, smol, embassy, etc. Adds API surface; existing pocopine crates uniformly use the cfg split. Rejected for consistency.

**E. Per-`(name, params_hash)` live topics from day one.** More efficient bandwidth profile (no client-side filtering). Bigger server change (per-subscription topics in the broker), needs RFC 071 (event spine) updates. Deferred to a v2 follow-up.

## Implementation status

Single PR off `main` on branch `wip/sync-query-driver`:

1. **driver.rs** — driver type, state machine, spawn shim, /open + /pull loop.
2. **driver.rs (live)** — `LiveWakeup<Row>`, params filtering, `select!` on poll + live.
3. **driver.rs (offline)** — pending replay on reconnect.
4. **client.rs** — `QueryClient::observe` spawns driver on first-ref; `Drop` cancels via epoch.
5. **plugin.rs** — `QueryClientConfig` + `QueryClient::with_config`.
6. **Tests** — unit + integration.

LOC estimate: ~1100 driver, ~600 tests, ~50 in client.rs/plugin.rs wiring. Total ~1750 LOC.

## Migration / rollout

Additive — no user code changes required. Existing `client.observe(q)` calls keep working; subscriptions now spawn a driver instead of staying inert. Existing tests that don't install a fetch mock middleware will see the driver attempt `/open` and get a network error — those tests need a mock middleware or a test-mode flag.

A new `QueryClient::without_driver()` constructor lets tests opt out of the driver entirely (replaces `QueryClient::new()` for pure routing-engine tests that don't want network plumbing).

## Open questions

1. **Poll interval default.** 30s feels long; 5s feels noisy. Pick based on observed Linear/Slack/Replicache-app patterns; default 30s + document the knob.
2. **Live wakeup topic naming.** Reuse `pocopine-live`'s existing per-collection topic (`sync:stream:{name}`)? Confirm there's no conflict with CRUD's existing subscriber.
3. **Driver panic propagation.** If the driver task panics, the subscription stays in a broken state. Should we catch panics and surface them via `state.error = "driver panicked"`? Or let the panic propagate to the runtime?

## Related RFCs

* [RFC 086](./rfc-086-sync-query.md) — `pocopine-sync-query` crate (the routing engine this RFC drives).
* [RFC 071](./rfc-071-event-spine.md) — live invalidation channel (consumed by item 2).
* [RFC 072](./rfc-072-offline-sync.md) — offline sync protocol (server-side dedup contract).
* [RFC 080](./rfc-080-deploy-contract.md) — deploy contract (sync endpoints).
