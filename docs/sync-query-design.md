# `pocopine-sync-query` — Implementation Design

Concrete implementation design for the crate proposed in [RFC 086](../rfcs/rfc-086-sync-query.md). This document is the working spec for Phase 3 — the actual `pocopine-sync-query` build-out.

It assumes RFC 086 is read. The RFC explains *why*; this doc explains *what to build* and *how to lay it out*.

---

## 1. Crate layout

```
crates/
├── pocopine-sync/              # unchanged (wire protocol + plugin)
├── pocopine-sync-crud/         # unchanged (resource-centric API, kept simple)
├── pocopine-sync-crud-macros/  # unchanged
├── pocopine-sync-query/        # NEW
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── query.rs            # Query<Row>, QueryKey, OrderBy
│       ├── client.rs           # QueryClient, QuerySubscription, QueryHandle
│       ├── mutator.rs          # Mutator trait, RowChange, MutationFuture
│       ├── state.rs            # QueryState<Row>, per-query reactive state
│       ├── view.rs             # QueryView<Row>, the typed visible-rows wrapper
│       ├── params.rs           # comparator wrappers (moved from -crud)
│       ├── predicate.rs        # PredicateEvaluator trait + builtin impls
│       └── __private.rs        # macro-implementation surface (serde_json reexport)
├── pocopine-sync-query-macros/ # NEW (proc-macro)
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs              # #[query_resource(...)] macro
└── pocopine-sync-query-sqlite/ # OPTIONAL adapter (Phase 3.5)
```

Independent crate boundary. `pocopine-sync-query` depends on `pocopine-sync` directly, not on `pocopine-sync-crud`. The two are decoupled at the crate level, but apps frequently use both — Query for filtered reads, CRUD for typed-Draft writes, with `CrudResource::params_of(<resource>::row_to_params_typed)` bridging them so a CRUD-built source gets §C precise live wakeups for free. See [`sync-crud-query-composition.md`](./sync-crud-query-composition.md) for the canonical pattern.

The proc-macro crate splits the same way as CRUD's does (binary boundary). Naming convention: `pocopine-<thing>-macros` matches the existing pattern.

---

## 2. Public API surface

### 2.1 Defining a queryable resource

```rust
use pocopine_sync_query::{query_resource, Mutator, RowChange};

// `#[query_resource]` decorates the row struct directly. Every
// `#[query_param]` field auto-gets `.eq()` + `.any_of()`. Range and
// contains are inferred from the inner type's name (numeric / `String`
// / DateTime-y), with explicit `(range)` / `(contains)` opt-ins for
// newtypes the heuristic misses. `(required)` marks a tenant gate —
// the predicate fails if the query has no value for that field.
//
// The attribute MUST appear before `#[derive(...)]` so it strips the
// per-field annotations before downstream derives (serde, etc.) see
// them.
#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    pub id: IssueId,
    #[query_param(required)]  pub workspace_id: String,
    #[query_param]            pub assignee_id: Option<String>,
    #[query_param]            pub status: Status,
    #[query_param]            pub title: String,        // String → also range + contains
    #[query_param]            pub created_at: DateTime, // DateTime → also range
}
```

The macro generates:

* `impl Issue { fn query() -> QueryBuilder<Self> }`.
* `field::workspace_id`, `field::assignee_id`, ... markers (one per `#[query_param]` field).
* Comparator trait impls on each marker (sealed-trait gate).
* `pub fn matches(&Query<Issue>, &Issue) -> bool` inside the resource's
  module — the typed predicate evaluator, auto-wired into the
  builder's `matches_fn`.
* `NAME` and `SCHEMA_VERSION` constants.

### 2.2 Building a query

```rust
use issues::field;
let query = Issue::query()
    .eq(field::workspace_id, w1)
    .any_of(field::status, [Status::Open])?
    .contains(field::title, "auth")?
    .order_by("created_at", Order::Desc)
    .limit(50)
    .build();
```

`QueryBuilder` is the typed builder. `.build()` returns `Query<Issue>`. Predicate methods take `field::*` markers and route through the same sealed comparator traits as CRUD's query DSL.

### 2.3 Subscribing

```rust
let client: &QueryClient = pocopine::query_client();

let handle: QueryHandle<Issue> = client.subscribe(query);
// handle.rows()        → Vec<Issue>
// handle.cursor()      → Option<SyncCursor>
// handle.state()       → reactive Handle to the per-query state
// handle.on_update(|state| { ... })
// drop(handle)         → decrements refcount; cancels on last drop
```

Two components subscribing to logically-equal queries get refcounted shared subscriptions (one `/open` call, one cursor, one materialized state).

### 2.4 Defining a mutator

```rust
use pocopine_sync_query::{mutator, RowChange};

#[mutator(name = "create_issue")]
pub async fn create_issue(
    ctx: &MutatorContext,
    payload: CreateIssuePayload,
) -> SyncResult<Vec<RowChange<Issue>>> {
    // Server-side execution.
    let row = ctx.source.create(payload).await?;
    Ok(vec![RowChange::Upsert(row)])
}

// Macro generates the matching `apply_local(payload)` from the payload's
// structural shape, OR the author provides one explicitly:
impl create_issue::Local for create_issue::Mutator {
    fn apply_local(payload: &CreateIssuePayload) -> Vec<RowChange<Issue>> {
        // Build the optimistic Issue row from the payload.
        vec![RowChange::Upsert(Issue {
            id: payload.id.clone(),
            workspace_id: payload.workspace_id.clone(),
            ..
        })]
    }
}
```

The `Mutator` trait is what the `QueryClient` calls. Both `apply_local` (sync) and `apply_remote` (async, server-bound) are required.

### 2.5 Running a mutation

```rust
let payload = CreateIssuePayload { id: IssueId::new(), workspace_id: w1, ... };
client.mutate::<create_issue::Mutator>(payload).await?;

// Behind the scenes:
// 1. apply_local(payload) → vec![Upsert(Issue { workspace_id: W1, ... })]
// 2. Engine walks active QuerySubscriptions for stream="issues".
// 3. For each subscription's Query<Issue>, calls .matches(&new_row).
// 4. Matching subscriptions: optimistic upsert into their pending overlay.
// 5. Non-matching subscriptions: skip.
// 6. Wire push to /push with the mutation payload.
// 7. Server response → run matches() again against canonical rows.
// 8. Canonical rows land in matching subscriptions' canonical_rows.
```

No "active subscription" question; the row's predicate-match is the routing key.

---

## 3. Type definitions

### 3.1 `Query<Row>`

```rust
pub struct Query<Row> {
    stream: SyncStreamName,
    params: StreamParams,
    order_by: Option<OrderBy>,
    limit: Option<u32>,
    _row: PhantomData<Row>,
}

pub struct OrderBy {
    field: String,          // declared param-name or row field-name
    direction: Order,       // Asc | Desc
}

pub struct QueryKey([u8; 8]);  // FNV-1a 64-bit of canonical JSON
```

`Query<Row>` is `Clone`, `Eq`, `Hash` (via `QueryKey`). The `Row` type parameter is erased at the wire layer (everything goes through `Value`) but typed at the API.

`Query::matches(&self, row: &Row) -> bool` is the macro-generated typed predicate. The macro walks the row struct's `#[query_param]`-annotated fields and emits one comparator per annotation.

### 3.2 `QueryClient`

```rust
pub struct QueryClient {
    sync: SyncClient,                                          // shared underlying
    registry: RefCell<HashMap<QueryKey, Rc<dyn AnyQuerySubscription>>>,
    mutators: HashMap<TypeId, Box<dyn AnyMutator>>,
}

impl QueryClient {
    pub fn new(sync: SyncClient) -> Self;

    pub fn subscribe<R>(&self, query: Query<R>) -> QueryHandle<R>
    where R: Clone + DeserializeOwned + Serialize + 'static;

    pub fn mutate<M: Mutator>(&self, payload: M::Payload) -> MutationFuture<M>;

    pub fn register_mutator<M: Mutator>(&mut self);

    /// Drop the local cache for a stream (sign-out path).
    pub async fn clear_all(&self) -> SyncResult<()>;
}
```

`QueryClient` is built from a `SyncClient`. Apps that already have one (via `pocopine::sync_plugin`) reuse it; Query doesn't replace the sync runtime.

`subscribe` is idempotent on `QueryKey`. The handle increments the refcount on construct, decrements on drop. When refcount → 0, the underlying `QuerySubscription` is gc'd (background task cancelled via `SyncEpoch::bump`, durable cache untouched).

### 3.3 `QuerySubscription` (internal)

```rust
struct QuerySubscription<Row> {
    query: Query<Row>,
    state: Rc<RefCell<QueryState<Row>>>,
    refcount: Cell<usize>,
    live_wakeup: Option<LiveSubscription>,
    epoch: SyncEpoch,
}

impl<Row> QuerySubscription<Row> {
    fn open(&self, sync: &SyncClient) -> SyncResult<()>;
    fn pull(&self, sync: &SyncClient, reason: SyncReason) -> SyncResult<()>;
    fn apply_optimistic(&self, change: &RowChange<Row>);  // predicate-gated
    fn apply_canonical(&self, change: &RowChange<Row>);   // predicate-gated
}
```

The subscription owns its state, its live wakeup (per-stream, filters in callback), and its own `SyncEpoch` (signed out only when the parent `QueryClient` signs out).

Local-store keys: `local_stream_key(&query.stream, &query.params)`. Same helper used in Batch 4; no changes needed.

### 3.4 `Mutator` trait

```rust
pub trait Mutator: 'static {
    type Payload: Serialize + DeserializeOwned + Send + 'static;
    type Row: Clone + Send + 'static;

    const NAME: &'static str;       // wire identity (mutation_id namespace)
    const STREAM: &'static str;     // which stream's queries this affects

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>>;

    fn apply_remote(
        ctx: RequestContext,
        payload: Self::Payload,
    ) -> impl Future<Output = SyncResult<Vec<RowChange<Self::Row>>>> + Send;
}

pub enum RowChange<R> {
    Upsert(R),
    Delete(RowKey),
}

pub struct MutationFuture<M: Mutator> {
    inner: Pin<Box<dyn Future<Output = SyncResult<MutationOutcome<M::Row>>> + 'static>>,
}

pub enum MutationOutcome<Row> {
    Accepted(Vec<RowChange<Row>>),
    Rejected { reason: String },
    Conflict { server_rows: Vec<Row> },
}
```

`Mutator` is a trait (one impl per mutation type), not a single function. The `#[mutator]` proc-macro generates the impl from a free function.

`apply_local` is sync because it runs inside `RefCell::borrow_mut` of the registry's state-application path. `apply_remote` is async because it hits the server.

### 3.5 `QueryState<Row>`

```rust
pub struct QueryState<Row> {
    canonical_rows: BTreeMap<RowKey, Row>,
    pending_overlays: BTreeMap<MutationId, PendingMutation<Row>>,
    cursor: Option<SyncCursor>,
    application_schema_version: Option<u32>,
    loading: bool,
    syncing: bool,
    stale: bool,
    error: String,
    last_reason: SyncReason,
}
```

Per-query state. Similar in shape to `pocopine-sync::CollectionState` but indexed by `RowKey` (not `Vec<SyncRow>`) because the predicate evaluator may insert / remove rows by identity rather than by position.

Rebase order:
1. Start with `canonical_rows`.
2. Replay pending overlays in mutation-id order.
3. Rows that no longer match the query's predicate are filtered out.
4. Result: the visible row set.

### 3.6 `QueryHandle<Row>` and `QueryView<Row>`

```rust
pub struct QueryHandle<Row> {
    subscription: Rc<QuerySubscription<Row>>,
}

impl<Row> QueryHandle<Row> {
    pub fn rows(&self) -> Vec<Row>;
    pub fn cursor(&self) -> Option<SyncCursor>;
    pub fn loading(&self) -> bool;
    pub fn error(&self) -> Option<String>;
    pub fn on_update<F>(&self, callback: F) -> SubscriptionToken
    where F: Fn(&QueryState<Row>) + 'static;
}

// Optional typed view wrapper (à la LocalResourceView):
pub struct QueryView<Row> {
    handle: QueryHandle<Row>,
}

impl<Row> QueryView<Row> {
    pub fn iter(&self) -> impl Iterator<Item = &Row>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn find(&self, key: &RowKey) -> Option<&Row>;
}
```

`QueryHandle` is the lifecycle owner. `QueryView` is a convenience wrapper over the rendered rows; thin.

---

## 4. Predicate evaluator

The macro generates one `matches` method per declared resource. Each comparator type evaluates against the matching row field.

### 4.1 Macro emission sketch

```rust
// Input (user code):
#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: IssueId,
    #[query_param(required)]  pub workspace_id: String,
    #[query_param]            pub status: Status,
    #[query_param]            pub created_at: DateTime,
    // ... other fields, queryable or not
}

// Generated:
impl Query<Issue> {
    pub fn matches(&self, row: &Issue) -> bool {
        // workspace_id: required eq
        if let Some(want) = self.params.get("workspace_id") {
            let want: String = match serde_json::from_value(want.clone()) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if row.workspace_id != want { return false; }
        }
        // status: InSet
        if let Some(set) = self.params.get("status") {
            let set: params::InSet<Status> = match serde_json::from_value(set.clone()) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if !set.values().iter().any(|s| s == &row.status) { return false; }
        }
        // created_at: Range
        if let Some(range) = self.params.get("created_at") {
            let range: params::Range<DateTime> = match serde_json::from_value(range.clone()) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if !range_contains(&range, &row.created_at) { return false; }
        }
        true
    }
}
```

The generated code is verbose but mechanical. Macros expand once at compile time; the runtime cost is the field comparisons themselves.

### 4.2 Predicate evaluation cost

Each `matches()` call is O(declared params). For a typical 3–5 param query, that's a handful of comparisons per row. The engine evaluates against every subscription's predicate on every mutation, so total cost is O(mutations × active subscriptions × params per subscription). For realistic apps (10s of subscriptions, 100s of mutations per session), this is negligible.

If a high-fanout app needs faster evaluation, the predicate evaluator can be compiled to a more efficient form (table-driven, vectorized) in a future pass. Out of scope for v1.

### 4.3 What about ordering and limits?

`order_by` and `limit` are applied at the rendered-view layer, not in `matches`. `matches` answers "does this row belong in this query?"; the render step sorts + truncates the matching rows.

Server-side: `/pull` responses are expected to return rows already filtered + ordered + limited per the request's params. Client-side: applies the same ordering to pending overlays before merging into the rendered view.

---

## 5. Routing engine

The heart of `QueryClient::mutate`. Here's the algorithm:

```rust
impl QueryClient {
    fn route_changes<Row>(&self, stream: &str, changes: &[RowChange<Row>])
    where
        Row: Clone + 'static,
    {
        let registry = self.registry.borrow();
        for (key, sub) in registry.iter() {
            // Subscriptions only care about their own stream.
            if sub.stream() != stream { continue; }

            // Try to coerce the subscription to Row's type. (One stream, one Row.)
            let Some(sub) = sub.as_typed::<Row>() else { continue; };

            for change in changes {
                match change {
                    RowChange::Upsert(row) => {
                        if sub.query.matches(row) {
                            sub.state.borrow_mut().upsert(row.clone());
                        } else {
                            sub.state.borrow_mut().remove(&row_key_of(row));
                        }
                    }
                    RowChange::Delete(key) => {
                        sub.state.borrow_mut().remove(key);
                    }
                }
            }
        }
    }
}
```

Called twice per mutation:

1. **After `apply_local`** — routes optimistic changes into matching subscriptions' pending overlays.
2. **After `apply_remote`** (server response) — routes canonical changes into matching subscriptions' canonical row sets, dequeues the optimistic.

Predicate routing also fires on:

* `/pull` responses (each returned row evaluated against any other-than-target subscription that may benefit).
* Live wakeup events (the event carries affected row keys + minimal fields; engine evaluates which subscriptions need to pull).

---

## 6. Wire protocol bindings

The Query crate constructs `pocopine-sync` request envelopes directly:

```rust
fn build_open_request<Row>(query: &Query<Row>) -> SyncOpenRequest {
    SyncOpenRequest::new([SyncStreamSubscription {
        stream: query.stream.clone(),
        params: query.params.clone(),
    }])
}

fn build_pull_request<Row>(query: &Query<Row>, cursor: Option<SyncCursor>) -> SyncPullRequest {
    SyncPullRequest::new(query.stream.clone())
        .params(query.params.clone())
        .cursor(cursor)
}

fn build_push_request<M: Mutator>(
    mutation: ClientMutation<M::Payload>,
    schema_version: u32,
) -> SyncPushRequest<M::Payload> {
    SyncPushRequest::new(SyncStreamName::new(M::STREAM).unwrap(), [mutation])
        .params(StreamParams::new())   // mutator pushes carry no params
        .with_schema_version(schema_version)
}
```

`/push` deliberately carries empty params — mutators are query-agnostic on the wire; the routing happens client-side via predicate evaluation. The server's `validate_push_params` accepts the empty case (the same trait method CRUD overrides; Query inherits the default).

---

## 7. Local store integration

No `SyncLocalStore` trait changes. Each `QuerySubscription` keys its local-store ops by `local_stream_key(&query.stream, &query.params)`.

The hydrate path for a subscription:

```rust
async fn hydrate(&self, store: &SyncLocalStoreHandle) -> SyncResult<()> {
    let local_key = local_stream_key(&self.query.stream, &self.query.params);
    let snapshot = store.hydrate_stream(&local_key).await?;
    // Restore canonical rows + cursor + pending overlays from the snapshot.
    // No bare-stream queue; no cross-query merge.
    self.state.borrow_mut().apply_local_snapshot(snapshot);
    Ok(())
}
```

The save path:

```rust
async fn save_snapshot(&self, store: &SyncLocalStoreHandle, rows: Vec<SyncRow<Value>>, cursor: Option<SyncCursor>) -> SyncResult<()> {
    let local_key = local_stream_key(&self.query.stream, &self.query.params);
    store.save_snapshot(
        LocalSnapshotBatch::new(local_key, /* collection */, rows, cursor)
            .with_application_schema_version(Some(self.advertised_version()))
    ).await
}
```

No bare queue, no composite/bare split, no `clear_stream(&bare_stream)` cross-cuts. Each query is its own world.

For mutations:

```rust
async fn enqueue_pending(&self, store: &SyncLocalStoreHandle, mutation: ClientMutation<Value>) -> SyncResult<()> {
    let local_key = local_stream_key(&self.query.stream, &self.query.params);
    store.enqueue_pending_mutation(&local_key, LocalPendingMutation::new(mutation)).await
}
```

The queue is per-subscription. When the user mutates, the engine enqueues the same mutation into every matching subscription's queue (predicate-routed). On replay, each subscription replays its own queue with its own params on the wire.

---

## 8. Schema versioning

Each subscription tracks its own `application_schema_version` (per-query). The schema-drift detection at `/open` time compares the advertised version against the subscription's cached version. On mismatch, the subscription wipes its own compartment — no cross-query implications.

Mutators get their `schema_version` from the `Mutator::STREAM_SCHEMA_VERSION` const (generated by the macro from the resource declaration). The push envelope carries that version; server routes through `migrate_payload` as usual.

---

## 9. CRUD ↔ Query interop

Apps using both crates pick per-resource:

```rust
// Settings — simple, CRUD-shaped.
let settings = SettingsClient::observe()?;
SettingsClient::save(form_data).await?;

// Issues — multi-tenant, Query-shaped.
let issues_in_w1 = query_client.subscribe(
    Issue::query().eq(issues::field::workspace_id, w1).build()
);
query_client.mutate::<create_issue::Mutator>(payload).await?;
```

Both crates can call `pocopine::sync_plugin()` to get the shared `SyncClient`. They don't share state at the model layer — each resource is owned by one or the other.

For sources that want CRUD's transactional push handling (mutation log, dedup, transaction binding) but Query's read model: implement `SyncStreamSource` directly and delegate push to a `CrudResource` constructed internally. The future `QuerySource` adapter formalizes this pattern.

---

## 10. Testing strategy

### 10.1 Unit tests

* Per-comparator: `matches()` correctness on each comparator type.
* Predicate-routing engine: optimistic / canonical changes routed to right subscriptions.
* Refcount lifecycle: subscriptions gc'd on last handle drop.
* Schema-drift: each subscription's wipe is independent.

### 10.2 Integration tests

`crates/pocopine-sync-query/tests/`:

* `shape_subscriptions.rs` — port the Batch 4 tests; verify wire envelope, validate_params, push semantics.
* `multi_subscription.rs` — two QueryHandles to different shapes on the same selector; verify state isolation.
* `optimistic_routing.rs` — mutator → predicate evaluation → state apply across multiple subscriptions.
* `offline_replay.rs` — queue under one subscription, reload, verify replay with correct params.
* `workspace_switcher.rs` — rapid subscribe / drop / re-subscribe with different params; no cross-contamination.

### 10.3 Compile-fail trybuild

`crates/pocopine-sync-query-macros/tests/ui/`:

* `query_in_on_eq_only_field.rs` — calling `.any_of` on a field declared as bare `T`.
* `query_unknown_field.rs` — calling `.eq(unknown_field, _)`.
* `query_empty_params.rs` — `#[query_resource(params())]` parse error.

Ports the existing Batch 4 trybuild patterns.

### 10.4 Example apps

* `examples/issue-tracker/` — Linear-clone on `pocopine-sync-query`, multiple workspaces, multiple shaped views per workspace. The canonical demo.
* `examples/blog/` (existing) — STAYS on CRUD. Demonstrates the simple path.

---

## 11. Migration tooling

Apps that adopted the Batch 4 shape DSL on CRUD need a clear migration path. We provide:

1. **Cookbook page** — `docs/migrating-crud-shapes-to-query.md` covers the typical patterns: `Resource::query().observe()` → `query_client.subscribe(Resource::query()....build())`; `Resource::create(payload).await?` → `query_client.mutate::<create_mutator::Mutator>(payload).await?`.
2. **Deprecation timing** — when Phase 2 (CRUD shape revert) lands, the macro's `params(...)` attribute on `#[resource(...)]` emits a deprecation warning pointing at `pocopine-sync-query`. Removed in a later release.
3. **Adapter (optional)** — a `pocopine-sync-crud-shape-compat` crate that provides the Batch 4 API as a thin wrapper over `pocopine-sync-query`. Not built unless real users need it.

---

## 12. Open questions (deferred to implementation)

* **`Mutator` macro ergonomics.** `#[mutator]` on a free function vs `impl Mutator for ...` on a struct? Decide during Phase 3.
* **`apply_local` derivation.** Auto-derive from `apply_remote` for trivial cases (create row, delete by key)? Or always require explicit impl?
* **Subscription token semantics.** Drop-on-decrement vs explicit `handle.cancel()`? Replicache style is RAII; matches Rust.
* **Reactive integration.** How does `on_update` plumb into pocopine's component reactivity? Use the existing `Handle<C>::update` mechanism, or a new observer pattern?
* **Live wakeup payload.** Server includes affected row key + minimal fields so client can predicate-evaluate without a round-trip; what's the wire shape? Likely `LiveEvent::QueryInvalidated { stream, affected_keys: Vec<RowKey>, affected_fields: Map<RowKey, Map<String, Value>> }` — defined in `pocopine-live` already, just needs Query to consume it.

These are implementation details; they don't block the design.

---

## 13. Phase 3 work breakdown

A reasonable PR sequence for Phase 3:

### PR 1 — `pocopine-sync-query` scaffolding

* Create the crate, Cargo.toml, lib.rs skeleton.
* Define `Query<Row>`, `QueryKey`, `OrderBy`, `Order`.
* Port `params::{InSet, Range, Contains}` from `pocopine-sync-crud` (move, not copy; Phase 2 revert removes from CRUD).
* No client yet, just types + builders.
* Unit tests on `Query::key()` determinism, `params` serialization.

### PR 2 — `pocopine-sync-query-macros` skeleton

* `#[query_resource]` parses the declaration.
* Emits `Issue::query() -> QueryBuilder<Issue>` with typed setters.
* Emits `field::*` markers + sealed comparator trait impls (carried from CRUD macros).
* No predicate evaluator yet.

### PR 3 — `Query::matches` predicate evaluator

* Macro emits the typed `matches(&self, row: &Row) -> bool`.
* Unit tests covering each comparator.
* Trybuild compile-fail tests for misuse.

### PR 4 — `QueryClient` + `QuerySubscription` runtime

* Implement the registry, refcount lifecycle, subscribe / drop semantics.
* Hydrate, /open, /pull flows.
* Schema-drift detection per subscription.
* Integration tests covering single-subscription / multi-subscription lifecycle.

### PR 5 — `Mutator` trait + routing engine

* `Mutator` trait + `#[mutator]` macro.
* `QueryClient::mutate` + predicate-routing.
* Optimistic apply + canonical reconciliation.
* Tests covering mutator → multi-subscription routing.

### PR 6 — Offline queue + live wakeup integration

* Per-subscription pending queue (durable).
* Replay on hydrate.
* Live wakeup via per-stream topic with predicate filter in callback.
* Tests for offline-then-online, multi-tab via BroadcastChannel, sign-out flow.

### PR 7 — Documentation + example app

* `docs/sync-query-cookbook.md` — the user-facing guide.
* `docs/migrating-crud-shapes-to-query.md` — migration from Batch 4 CRUD-shapes.
* `examples/issue-tracker/` — full multi-workspace demo.

### PR 8 — Phase 2 revert in CRUD

Separately: a revert PR that drops the shape-subscription integration from `pocopine-sync-crud`. Keeps the wire envelope changes in `pocopine-sync`. CRUD goes back to its pre-RFC-085 simplicity.

After all 8 PRs land, the recommendation flow in `sync.md` updates to point users at the right crate based on app shape.

---

## 14. References

* RFC 086 — design rationale + alternatives + migration plan.
* `docs/sync-design.md` — full sync framework design.
* `docs/sync-shape-subscriptions.md` — Batch 4 reference implementation cookbook (will be deprecated).
* Replicache architecture documentation — mutator + query separation.
* Zero (rocicorp) — CVR protocol, materialized views.
* ElectricSQL shapes — shape declaration + sync protocol.
* TanStack Query — QueryClient + refcounted subscriptions pattern.
* InstantDB InstaQL — typed query DSL with reactive auto-update.
