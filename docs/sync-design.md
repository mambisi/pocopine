# Pocopine Sync — Full Design Document

This document explains the sync framework end-to-end: the implementation as it ships in `wip/sync-shape-subs-batch-4`, the architectural tension that has emerged with shape subscriptions, the target design, and the migration path. Read it once before iterating on sync code so you understand the seams.

It supersedes scattered notes in:

* `sync.md` — user-facing tutorial; kept current with this doc.
* `sync-local-first-roadmap.md` — strategic positioning, mostly aspirational.
* `sync-local-first-gaps.md` — gap analysis by axis (RFC 085 = Axis 2).
* `sync-shape-subscriptions.md` — cookbook page for the shape DSL.
* `sync-local-first-architecture-review.md` — older architecture pass.
* `rfcs/rfc-085-shape-subscriptions.md` — design rationale for shape subs.

---

## 1. Executive summary

Pocopine sync is a cursor-based, optimistic, local-first sync framework. The current implementation works correctly for **unparameterized streams** (one logical stream per resource, one subscription per client) and provides a wire-level shape-subscription DSL for **parameterized streams** (e.g. `workspace_id = W1`).

The shape-subscription work (RFC 085 / Batch 4) introduced a single architectural seam that is responsible for every issue surfaced by the recent 16-pass code review cycle:

> **The CRUD write path does not know which subscription it belongs to.** Shape subscriptions identify themselves by `(stream, params)`. CRUD writes flow through an unparameterized `SyncCollection` and identify themselves by just `stream`. Reconciling these two on the client — in the durable cache, the pending queue, the in-memory view state, and the post-push reconciliation pull — produces a steady drip of cross-tenant data leaks, schema-drift edge cases, and shape-switch races.

The interim ships with a **bare-stream queue / composite-key snapshot split**: CRUD writes queue under the bare stream name, shaped subscriptions cache snapshots under `stream__params_<hash>`. This works at the durable layer but exposes inconsistencies at the in-memory and replay layers.

The **target design** unifies both via a **subscription registry**: every observed `(stream, params)` pair gets its own state, its own queue, and its own lifecycle. CRUD writes flow through the active subscription. The registry refcounts identical subscriptions across components. This is what Batch 2b in RFC 085 has always pointed at.

Estimated work to land the target design: medium-sized PR touching `SyncClient`, `CrudClientResource`, and the macro emissions. No protocol/wire changes; no `SyncLocalStore` trait changes.

---

## 2. Layer diagram

```text
┌─────────────────────────────────────────────────────────────────────┐
│  App code                                                           │
│  • Resource::create(...)   / save / remove                          │
│  • Resource::stream().where_eq(field::workspace_id, W).observe()    │
│  • Resource::query().where_in(field::status, [...]).observe_with(…) │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  pocopine-sync-crud-macros        (proc-macro)                      │
│  Generates per-resource:                                            │
│    • StreamParams typed struct + extract/serialize_params           │
│    • Resource::stream() / query()  (the shape DSL)                  │
│    • CrudClientResource: create/save/remove/conflict helpers        │
│    • field::* markers + sealed comparator traits (compile-fail      │
│      misuse of the query DSL)                                       │
│    • Auto-wired SyncStreamSource::validate_params                   │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  pocopine-sync-crud           (typed CRUD layer)                    │
│  • CrudSource trait (server)                                        │
│  • CrudClientResource (client; thin wrapper on SyncCollection)      │
│  • params::{InSet,Range,Contains}  (comparator wrappers)            │
│  • LocalResourceView<Id, Row>      (rebased typed view)             │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  pocopine-sync                (protocol + plugin)                   │
│  Server side:                                                       │
│    • /open  /pull  /push  routes                                    │
│    • SyncStreamSource trait (validate_params, pull, push, migrate)  │
│  Client side:                                                       │
│    • SyncCollection<C, T>  (the per-state runner)                   │
│    • CollectionState<T>   (canonical_rows / pending / cursor / etc) │
│    • SyncLocalStoreHandle (wraps any SyncLocalStore impl)           │
│    • Live wakeup via pocopine-live's query tags                     │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│  SyncLocalStore impls         (per-backend)                         │
│  • MemoryLocalStore   (in-tree, for tests)                          │
│  • pocopine-sync-sqlite (native SQLite + browser WASM+OPFS)         │
│  • pocopine-sync-indexdb (browser IndexedDB)                        │
│  • pocopine-sync-sqlx   (host SQLx adapter)                         │
└─────────────────────────────────────────────────────────────────────┘
```

Each layer is a thin contract on top of the one below. No layer reaches around the layer below it.

---

## 3. The subscription model

### 3.1 Wire identity

On the wire, a subscription is identified by `(stream, params)`:

```rust
// SyncOpenRequest.streams: Vec<SyncStreamSubscription>
struct SyncStreamSubscription {
    stream: SyncStreamName,       // server-registered name
    params: StreamParams,         // BTreeMap<String, Value> — sorted, canonical
}
```

When `params.is_empty()`, the subscription serializes as a bare string (`"posts"`) — backwards compatible with pre-RFC-085 clients. When non-empty, it serializes as `{"stream": "issues", "params": {"workspace_id": "W1"}}`.

The same `params` map travels on every `/open`, `/pull`, and `/push` envelope for that subscription.

### 3.2 Server-side identity

Server-side, a stream is the unit of registration:

```rust
SyncServer::builder()
    .public_stream(IssuesSource)        // exposes the "issues" stream
    .guarded_stream(PostsSource, auth)
    .build();
```

A single `SyncStreamSource` impl serves *every* `(stream, params)` subscription on that stream. It receives `request.params` per call and is responsible for filtering its read/write logic accordingly.

`validate_params` (and the new `validate_push_params`) are trait methods on `SyncStreamSource` that gate the shape:

* `/open` and `/pull` → strict `validate_params`. Empty params accepted only when the source explicitly declares no shape.
* `/push` → `validate_push_params`, which defaults to `validate_params` but CRUD resources override to accept empty params (the "CRUD writes don't carry shape" semantic).

### 3.3 Client-side identity (the heart of the tension)

**Today**, client-side identity is mixed:

| Concern                  | Keyed by              | Why                                                |
| ------------------------ | --------------------- | -------------------------------------------------- |
| Wire envelopes           | `(stream, params)`    | Server contract                                    |
| `CollectionState<T>` slot| `CollectionSelector`  | One state per selector regardless of params       |
| Durable snapshot         | `local_stream_key()`  | `stream` for empty params, `stream__params_<hash>` otherwise |
| Durable pending queue    | `stream` (bare)       | CRUD writes don't know subscription              |
| Durable conflict markers | `stream` (bare)       | Row-scoped, not subscription-scoped              |
| Live wakeup topic        | `stream` (bare)       | Per-stream broadcast; client filters in callback |

The split was introduced as a workaround. A clean design would key **everything except the wire envelope** by `(stream, params)`.

### 3.4 The interim's pragmatic choices

The interim wire envelope is correct. The interim's *client* model has these in-flight compromises:

* **`active_local_key` on `CollectionState`** — tracks which `(stream, params)` last populated the in-memory state. On switch, `reconcile_local_key` calls `reset_for_schema_invalidation` to drop the previous rows/cursor. Spawned tasks check `is_active_local_key` at every post-await point and abort when stale.

* **Bare queue with composite snapshot** — CRUD writes enqueue under the bare stream so they're discoverable by any subscription that opens later. Snapshots (rows + cursor + cached_schema_version) live under the composite to avoid cross-tenant contamination.

* **Loose-on-empty push validation** — `validate_push_params` allows empty params for CRUD writes that don't know their subscription. Custom shape-aware sources can override to be strict.

* **No CRUD source params filtering** — `CrudSource::list(ctx, limit)` takes no `params`, so even when the macro generates `validate_params`, the *read* path doesn't actually filter. Source authors must implement `SyncStreamSource` directly OR rely on `RequestContext`-scoped tenancy.

These work for the current API surface (CRUD writes + shaped read-only subscriptions) but each is a documented seam.

---

## 4. Where we are today (Batch 4 as shipped)

### 4.1 Server

* `SyncStreamSource::validate_params` defaults to "accept empty, reject non-empty" (safe default for sources without a declared shape).
* `SyncStreamSource::validate_push_params` defaults to `validate_params` (strict everywhere by default; CRUD-style sources override to accept the empty-CRUD-push case).
* `pull_handler` always validates params strictly.
* `push_handler` routes through `validate_push_params`.
* `open_handler` validates strictly + echoes accepted params on the response so the client can detect drift.

### 4.2 Wire envelopes

* `SyncStreamSubscription` serializes symmetrically: bare-string when `params.is_empty()`, object form when non-empty.
* All `params` fields (`SyncOpenStream`, `SyncPullRequest`, `SyncPushRequest`, `SyncStreamSubscription`) accept JSON `null` as equivalent to absent (handles TS/Python clients).
* `validate_open_response` compares the server-echoed `accepted.params` against the requested params; mismatches surface as `BadRequest`.

### 4.3 Macro DSL

* `#[resource(params(...))]` declares a typed shape. Empty `params()` is a parse-time error.
* Comparator wrappers (`InSet<T>`, `Range<T>`, `Contains`) must come from `pocopine_sync_crud::params::*` — bare `Range<T>` (which would collide with `std::ops::Range`) is rejected with a span-attributed error.
* Custom Deserialize on each wrapper re-runs the constructor invariants on the wire (empty in-set, fully-unbounded range, empty needle → BadRequest).
* `StreamParams::serialize_params` returns `SyncResult` (no panic on serde failure).
* `OptionalEq` extraction treats JSON `null` and key-absent both as `None`.
* The query DSL's `Resource::query().observe()` calls `StreamParams::extract` synchronously, so missing required `.where_*` clauses surface as a synchronous error instead of an async server BadRequest.
* `__SealedFieldMarker` is an `unsafe trait` — downstream code that tries to extend the sealed gate must write `unsafe impl`, a strong signal.
* All macro-emitted JSON paths route through `pocopine_sync_crud::__private::serde_json` so consumer crates inherit the dep transitively.

### 4.4 Client

* `SyncCollection::open()` synchronously calls `reconcile_local_key(local_key)` before spawning so shape switches don't flash with previous-shape rows in the first render.
* `start_open_then_pull` (the spawned open task) re-checks `is_active_local_key` after the hydrate await, the bare-snapshot await, and the `/open` await. Aborts when stale.
* `start_pull` adopts the active key when `None` (first-use case) and aborts when stale otherwise.
* Live wakeup callbacks capture their `local_key` and silently no-op when the active key has moved on.
* `send_push_and_reconcile` fences both the in-memory `apply_push` and the post-push reconciliation pull against the active key.

### 4.5 Local store

* `hydrate_stream` / `save_snapshot` / `apply_changes` are keyed by `local_stream_key(stream, params)`.
* `pending_mutations` / `enqueue_pending_mutation` / `mark_push_result` / `clear_conflict` / `purge_pending_for_row` are keyed by the **bare** stream.
* `clear_stream` wipes a single compartment. The composite wipe in `start_open_then_pull` also wipes the bare stream IF the bare cache's own `application_schema_version` is older than the advertised version. Bare queues that are at-or-newer-than-advertised are preserved across composite wipes.
* On `start_open_then_pull`, a shaped subscription hydrates BOTH the composite (for rows + cursor) AND the bare snapshot (for the queue + its own cached_schema_version). The bare queue's schema age governs whether to replay/wipe/defer.

### 4.6 Limitations documented in the cookbook

* CRUD source doesn't filter on params (`list_with_params` is needed).
* Shape-aware `SyncCollection::push(...)` is not exposed by the macro surface — only `.observe()`.
* Bare-queue mutations don't appear in shaped views' UI overlay (no per-mutation params metadata).
* Bare-queue replay defers when the bare cache's schema age is unknown (no prior unparameterized open).

---

## 5. The architectural tension

Every one of the 16 codex passes surfaced an instance of the same underlying problem:

### 5.1 The mismatch

```text
Shape subscription:  (stream, params)  ──> CollectionState slot, durable snapshot
                                     ──> live wakeup callback (captures params)
                                     ──> observe() flow
CRUD write:                stream     ──> bare queue, optimistic to SyncCollection state
                                     ──> create/save/remove flow
```

The CRUD `Resource::create(payload)` helper doesn't know — *cannot* know in today's API — which subscription is "active" in the user's UI. The user might be observing W1 while the create is for W1, OR they might be observing nothing (push-only component), OR they might be observing W1 but pushing a W2 row (cross-workspace write).

The macro picks one answer: **CRUD writes use an unparameterized `SyncCollection`** (empty `params`). The optimistic row goes into whatever `CollectionState` slot the selector points at — which is shared across observers of every shape because the selector is the same.

This works *as long as the user observes only one shape at a time* (or no shape). It breaks in:

1. **Concurrent multi-shape observation** — W1 and W2 views mounted simultaneously share a `CollectionState`. A pull for W1 overwrites W2's rows. (Mitigated by `active_local_key` + reset-on-switch, at the cost of breaking concurrent observers.)
2. **Workspace switcher UI** — quick W1 → W2 → W1 switches generate races between in-flight pulls and the new active key.
3. **Offline CRUD writes** — the queue is bare, but the observing view is shaped; the queue isn't visible in the shaped view until reconciliation.
4. **Schema drift across shapes** — the bare cache's schema version may disagree with a shaped composite's; replays could tag old-shape payloads with new-shape versions.
5. **Cross-tenant pushes** — a user observing W1 who creates a W2 row (via payload.workspace_id) sees the optimistic in W1 momentarily, then it disappears when the canonical pull rejects it for the W1 shape.

### 5.2 Why band-aids couldn't close it

Every codex pass either:

* Tightened an `active_local_key` gate at another await point.
* Split the bare/composite responsibility for another local-store op.
* Introduced a new trait method (`validate_push_params`) to split semantics.
* Added a schema-age check on the bare compartment.

Each fix was correct but local. The fixes don't compose into a single unified model because the underlying abstraction — "client identifies a subscription by something" — is split between `stream`, `(stream, params)`, and `CollectionState slot`.

The cycle terminates when there's one identity for all client-side state. That identity is `(stream, params, selector)` and it lives in a subscription registry.

---

## 6. The target design

### 6.1 Subscription Registry

A new top-level component on `SyncClient`:

```rust
struct SubscriptionRegistry {
    entries: RefCell<HashMap<SubscriptionKey, Rc<Subscription>>>,
}

struct SubscriptionKey {
    stream: SyncStreamName,
    params_hash: [u8; 32],   // blake3 over canonical JSON, or fnv1a if we keep it lightweight
    // selector is implicit: each Handle has one registry
}

struct Subscription {
    stream: SyncStreamName,
    params: StreamParams,
    state: Rc<RefCell<CollectionState<Value>>>,
    refcount: Cell<usize>,
    live_wakeup: Option<LiveSubscription>,
    epoch: SyncEpoch,
}
```

* Each `(stream, params)` pair has its own `Subscription`. Two components observing the same shape share one subscription (refcount).
* Each `Subscription` owns its own `CollectionState` — no more selector-shared state.
* When the last observer drops, the subscription is gc'd (and its background tasks via `epoch.bump()`).

### 6.2 What this unifies

| Concern                     | Today                              | Target                                  |
| --------------------------- | ---------------------------------- | --------------------------------------- |
| In-memory state             | One per selector (shared)          | One per `(stream, params)` (scoped)     |
| Durable snapshot key        | `local_stream_key()`               | Same (already correct)                  |
| Durable queue key           | Bare stream                        | `(stream, params)` per-subscription     |
| Conflict markers            | Bare stream                        | Per-subscription                        |
| Pending overlay in view     | Composite-only (bare invisible)    | Per-subscription, naturally correct     |
| `active_local_key` field    | Required                           | Removed (each state IS one shape)       |
| Shape-switch reset          | Required                           | Removed (new shape = new subscription)  |
| `is_active_local_key` gates | Required everywhere                | Removed                                 |
| Live wakeup filter          | Closure captures params + checks   | Subscription's own wakeup, no filter    |
| CRUD writes                 | Bare `SyncCollection`              | Routed through active subscription      |
| Post-push reconciliation    | Empty-params pull → fails on shape | Pull through subscription's own context |

Roughly **every band-aid added in the 16-pass loop goes away**.

### 6.3 CRUD writes against shape

The unresolved question: how does `Resource::create(payload)` find its subscription?

Three model choices, in increasing API impact:

**Model A — Implicit "last observed" subscription per resource.**
The registry tracks the most recent observe on each Resource. CRUD writes attach to that. Simple, works for typical UIs, breaks for unusual patterns (push-only components).

**Model B — Explicit subscription handle.**
`Resource::stream().workspace_id(W1).observe()` returns a handle. CRUD writes go through the handle: `handle.create(payload)`. The handle carries the subscription key. Clean but breaks the existing `Resource::create()` API.

**Model C — Hybrid via thread-local or scope-local context.**
The observe() returns a handle that the component holds. Inside that component's scope, `Resource::create()` finds the active subscription via scope context. Outside any observe scope, `Resource::create()` falls back to the unparameterized subscription. Preserves the existing API and works for typical UI flows.

**Recommendation: Model C.** It preserves the cookbook's `Resource::create(payload)` ergonomics while making CRUD writes shape-aware in observed contexts. Requires plumbing scope-local subscription context — minor.

### 6.4 CRUD source filtering on params

Separately from the registry work: the `CrudSource` trait needs a way to consume `request.params`. Without this, the macro's shape declaration is a wire contract but not a read filter.

Add a new trait method:

```rust
trait CrudSource: Send + Sync + 'static {
    type Id; type Row; type Draft;

    async fn list(&self, ctx: RequestContext, limit: usize) -> SyncResult<Vec<Self::Row>> {
        self.list_with_params(ctx, limit, &StreamParams::new()).await
    }

    async fn list_with_params(
        &self,
        ctx: RequestContext,
        limit: usize,
        params: &StreamParams,
    ) -> SyncResult<Vec<Self::Row>> {
        let _ = params;
        self.list(ctx, limit).await
    }

    // ... rest unchanged
}
```

Both methods default to each other; concrete sources override whichever fits. Existing `CrudSource` impls keep compiling. The macro's auto-generated `pull_snapshot` calls `list_with_params(ctx, limit, &request.params)`.

### 6.5 Live wakeup per-subscription topic

Today, all subscriptions to one stream share one wakeup topic (`sync:stream:{name}`). Every wakeup fires every subscription's callback; each callback filters internally.

Target: introduce a per-`(stream, params_hash)` topic for high-fanout streams. Opt-in at source registration:

```rust
SyncServer::builder()
    .public_stream(IssuesSource)
    .with_per_subscription_wakeups()
    .build();
```

Server emits wake-ups per `(stream, params_hash)` instead of per-stream when this is enabled. Client subscribes to the specific topic. This is a tail optimization — not blocking for the registry work.

---

## 7. What needs to change

Concrete delta from current `wip/sync-shape-subs-batch-4` to the target design.

### 7.1 `pocopine-sync`

* New `SubscriptionRegistry` on `SyncClient`. Owns the `HashMap<SubscriptionKey, Rc<Subscription>>`.
* `Subscription` struct with refcount, own state, own epoch.
* `SyncClient::subscribe(stream, params) -> Rc<Subscription>` — public entry point.
* `SyncClient::subscribe_or_get(stream, params)` — refcount-bumping idempotent variant.
* `Subscription::observe(callback)` — registers a reactive callback against the subscription's state.
* Remove `active_local_key` field + `reconcile_local_key` method + `is_active_local_key` from `CollectionState`.
* Remove the bare-queue / composite-snapshot split in `start_open_then_pull`. Each subscription's local-store ops use its own composite key everywhere.
* Live wakeup: each subscription owns its own. On drop, the wakeup unsubscribes.

### 7.2 `pocopine-sync-crud-macros`

* Generated `Resource::stream().observe()` / `Resource::query().observe()` route through `SyncClient::subscribe_or_get` and return a `View` backed by the subscription's state (not a selector-shared state).
* Generated `Resource::create/save/remove` route through the active subscription via scope-local context (Model C). Outside any observe scope, fall back to the unparameterized subscription.
* `CrudClientResource` removes its `collection: SyncCollection<C, T>` field; replace with `subscription: Rc<Subscription>`.

### 7.3 `pocopine-sync-crud`

* `CrudSource::list_with_params(ctx, limit, params)` default method as described above.
* Macro's `pull_snapshot` calls the new method.
* `LocalResourceView<Id, Row>` becomes a per-subscription view (one per shape).

### 7.4 `SyncLocalStore` impls

* No trait surface changes. All four backends (memory, sqlite, indexdb, sqlx) continue to work as-is — they already accept any `SyncStreamName`, and the composite key flows through naturally.
* No on-disk migration: empty-params subscriptions still use the bare stream name (backwards compat with existing data).

### 7.5 Documentation

* `sync.md` updated to describe the registry as the unit of subscription.
* `sync-shape-subscriptions.md` shortened — most of the "known limitations" section disappears.
* `rfcs/rfc-085-shape-subscriptions.md` marks Batch 2b as the registry RFC; new RFC for the subscription registry references back to 085.

---

## 8. Migration path

The interim `wip/sync-shape-subs-batch-4` work can ship as-is for unparameterized resources. Shape subscriptions ship as **stable wire contract, beta client model**. Apps that don't use the shape DSL are unaffected.

Recommended order:

### Phase 1 — Ship Batch 4 as-is

The current branch is wire-correct and works for the dominant use case (one shape observed at a time, unparameterized CRUD writes). The known limitations are documented in `sync-shape-subscriptions.md`. Apps wanting shape subscriptions get them with caveats; apps that don't are uncontended.

### Phase 2 — `CrudSource::list_with_params`

Standalone PR. Default-method addition; no API breakage. Apps that want shape-filtered reads override `list_with_params`. The macro starts threading `request.params` into the call.

This closes the "CRUD source doesn't filter on params" gap independently of the registry work.

### Phase 3 — Subscription registry (Batch 2b)

Major PR; touches `SyncClient` and the macro emissions. Old API stays compatible via a deprecation period:

* `SyncCollection<C, T>` API stays available; internally it routes through a registry-managed subscription.
* `Resource::stream()` / `query()` return registry-backed views instead of selector-shared states.
* `CrudClientResource` migrates to subscription-backed; the public methods (`create`, `save`, etc.) stay identical.

After Phase 3 ships, remove the interim band-aids:

* Drop `active_local_key` from `CollectionState`.
* Drop `is_active_local_key` checks in spawned tasks.
* Drop the bare-vs-composite split in local-store ops.
* Simplify `local_stream_key` (becomes purely the subscription's cache key with no special-casing).

### Phase 4 — Per-`(stream, params_hash)` live topics

Standalone optimization PR. Opt-in at server-side `SyncServerBuilder`. Low priority; the stream-wide broadcast is fine for typical fanout.

---

## 9. Decision log

### 9.1 Why we kept the interim instead of reverting

Reverting Batch 4 would have lost: the wire envelope (which is correct and forward-compatible), the macro DSL (which is correct), the strict validate_params gate (which is a real security improvement on `/open`/`/pull`), and the schema-versioning integration. Shipping the interim with documented seams is strictly better than reverting.

### 9.2 Why we didn't fix all 16 codex findings end-to-end

We did, mostly. The ones we deferred — CRUD source params filtering, shape-aware direct `SyncCollection::push`, per-mutation params in the bare queue — all require a model change (the registry, or `list_with_params`) that's bigger than the iteration budget for this batch. Documenting them is the honest call.

### 9.3 Why subscription registry now vs later

Each codex pass found one more local gap. The gaps aren't a quality issue with any individual fix; they're emergent from the abstraction split. Sixteen passes confirmed that more local fixes won't close it. The registry is the model change that does.

### 9.4 Why not `SyncStreamName` includes params

Considered and rejected during RFC 085 planning. The opaque-string validation on `SyncStreamName` is load-bearing for token budget, wire stability, and observability. Putting params inside the name would have:

* Broken every existing log line and trace.
* Pushed params parsing into every `SyncStreamSource` impl.
* Made backwards-compat with bare-string subscriptions impossible.

Keeping `SyncStreamName` opaque and adding `params` as a sibling is the right call.

### 9.5 Why FNV-1a instead of blake3 for `local_stream_key`

Cache disambiguation doesn't need cryptographic hashing. A collision shreds one client's local cache (no security impact, no data leak). FNV-1a is 20 lines, no dep, deterministic across runs. blake3 would be cleaner but isn't worth a new dependency for this purpose. Easy to upgrade later.

### 9.6 Why `unsafe trait __SealedFieldMarker`

In stable Rust, proc-macros emitting code in downstream crates force the seal supertrait to be reachable from outside the source crate. That makes a true cross-crate seal impossible. `unsafe trait` is the strongest available — downstream code that tries to extend the gate must write `unsafe impl`, which is a strong "you're violating the contract" signal that catches code review. The seal isn't memory-unsafe; the `unsafe` here is purely API-stability.

---

## 10. Reference: the 17-commit Batch 4 stack

Batch 4 shipped as four logical phases:

1. **Wire envelope + trait params** (eaa909f6, b1) — `SyncStreamSubscription` on the wire, `validate_params` trait method.
2. **Macro DSL + typed StreamParams** (4145dfff, b2) — `#[resource(params(...))]`, comparator wrappers, `Resource::stream()` builder.
3. **Query DSL + sealed comparator traits** (d2fc6bb4, b3) — `Resource::query()`, type-safe `field::*` markers, sealed comparator trait gate.
4. **Auto-validate_params + cookbook + RFC final** (b96f1a72, b4) — auto-wired validator, `docs/sync-shape-subscriptions.md`, RFC 085 → Final.

Plus 13 follow-up commits addressing 16 codex review passes. See `git log` for the chain.

---

## 11. What you should know going into the next iteration

1. **Don't add more `active_local_key` gates.** The pattern has saturated. Each new gate covers one more codex finding but leaves the next one open.

2. **Subscription registry is the correct unifying abstraction.** Until it lands, every shape-subscription feature carries the same architectural debt.

3. **The wire contract is settled.** Don't touch `SyncStreamSubscription`, `validate_params`, `validate_push_params`, or `local_stream_key`'s semantics. Those have been hardened.

4. **The macro is settled.** `#[resource(params(...))]` declares the shape; the comparator vocabulary is bounded and fixed; the sealed trait gate works.

5. **`CrudSource::list_with_params` is independent of the registry.** Ship it standalone. It closes the biggest gap in the cookbook (shape declarations that don't filter reads) without touching the client.

6. **Apps that don't use shape subscriptions are unaffected by all of this.** The Batch 4 work is purely additive at the API level.

7. **The 16-pass codex loop is not a quality signal.** It's a signal that the abstraction needs to move. The next architectural change (registry) is the one that ends the loop.
