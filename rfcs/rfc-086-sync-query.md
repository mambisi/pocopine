# RFC 086 — `pocopine-sync-query`

* **Status:** Implemented
* **Author:** sync framework working group
* **Tracking branch:** `wip/sync-query` (TBD)
* **Supersedes:** the shape-subscription portion of RFC 085 for production multi-tenant use; coexists with `pocopine-sync-crud`.
* **Related:** RFC 071 (event spine), RFC 072 (offline sync protocol), RFC 080 (deploy contract), RFC 085 (shape subscriptions; reference implementation lives on `wip/sync-shape-subs-batch-4`).

## Summary

Introduce `pocopine-sync-query` as a parallel client-side data layer to `pocopine-sync-crud`. Where CRUD is **resource-centric** ("one entity type, one logical view, one optimistic state"), Query is **subscription-centric** ("one query, one cache compartment, one reactive view; many queries per entity type"). The two crates share the underlying `pocopine-sync` wire protocol, durable store, and live-wakeup channel — they differ at the model layer above the protocol.

The Query crate is built around three primitives, drawn from the consensus design across Replicache/Zero, ElectricSQL, PowerSync, InstantDB, and TanStack Query:

1. **`Query<Row>`** — a declarative description of "what data do I want?", with its own canonical hash identity.
2. **`Mutator`** — a transactional function that produces row changes; the engine evaluates each change against every active query's predicate and routes it to the views that match.
3. **`QueryClient`** — a refcounted registry that owns one `QuerySubscription` per distinct `Query`, with its own state, queue, and lifecycle.

CRUD remains the recommended path for simple resource-shaped apps. Query is the recommended path for filtered, multi-tenant, dashboard-style apps. Both ship.

## Motivation

The shape-subscription work in RFC 085 (Batch 4) added a typed `params(...)` DSL to `pocopine-sync-crud`. The wire protocol is correct. The macro DSL is correct. The CLIENT model — where shape subscriptions and CRUD writes try to share one `CollectionState` keyed by `Resource` — produced a 16-pass codex review loop, with every fix exposing the next gap because the underlying abstraction is split:

| Concept            | CRUD invariant                  | Shape subscription reality              |
| ------------------ | ------------------------------- | --------------------------------------- |
| Identity           | `Resource` (entity type)        | `(stream, params)` (filtered view)      |
| State              | One per Resource                | One per `(stream, params)`              |
| Pending overlay    | Shared across all observers     | Scoped to one filtered view             |
| Optimistic apply   | Goes to "the" state             | Predicate-matched into multiple states  |
| Post-push pull     | Empty params                    | Subscription's params                   |
| Schema version     | One per Resource                | One per `(stream, params)` cache        |

The interim band-aids on `wip/sync-shape-subs-batch-4` keep both worlds working in isolation but never compose cleanly:

* `CollectionState::active_local_key` + reset-on-switch (breaks concurrent multi-shape observers).
* Bare-stream pending queue + composite-key snapshot (orphans queue on shape switch OR leaks cross-shape rows on overlay).
* `validate_push_params` as a separate trait method (the empty-CRUD-push case has no clean expression).
* Loose-on-empty server validation (security trade-off).
* Bare-snapshot hydrate on every shaped open (extra round-trip).
* `is_active_local_key` checks at every post-await point (whack-a-mole).

The honest read: CRUD was designed to be **safe and rigid by design**. Shape subscriptions need **flexibility**. Forcing them into one crate compromises CRUD's invariants without earning the flexibility.

This RFC proposes splitting them.

## Goals

* **Shape subscriptions as a first-class primitive.** Queries are the unit of identity, not a parameter on a Resource.
* **Mutators decoupled from queries.** Optimistic writes attach to rows via predicate evaluation, not via "active subscription" coupling.
* **Refcounted subscription registry.** Two components observing the same query share one `QuerySubscription`, one `/open`, one cursor, one materialized state.
* **Server-side query authority is optional.** The wire protocol stays open: server can advertise a fixed query catalog OR accept any well-shaped query and authorize per-call.
* **Wire protocol reuse.** `SyncStreamSubscription`, `validate_params`, `validate_push_params`, `local_stream_key` from RFC 085 stay as-is. No protocol changes.
* **Coexistence with CRUD.** Both crates depend on `pocopine-sync` independently. An app can use both: CRUD for `Settings`, Query for `Issues`.
* **Move the reference implementation.** The Batch 4 work on `wip/sync-shape-subs-batch-4` is documented as the reference for what NOT to retrofit into CRUD; the lessons inform the Query crate's design without the retrofit.

## Non-goals

* **No CRDT / merge model.** Optimistic state + last-writer-wins per row, same as CRUD.
* **No deprecation of CRUD.** CRUD remains the simpler path for resource-shaped apps. The README will recommend either based on app shape.
* **No SQL DSL.** The query DSL is the same bounded comparator vocabulary from RFC 085 (`eq`, `in`, `range`, `contains`) plus `order_by` and `limit`. No JOIN, no nested predicate trees, no SQL passthrough.
* **No server-side query planner.** Servers receive `request.params` as a typed map and apply their own filter logic.
* **No P2P sync.** Future work.

## Design

### 1. Query identity

A `Query<Row>` is the unit of identity. Two queries with the same canonical form share a `QueryKey` (FNV-1a 64-bit over the canonical JSON, same scheme as `local_stream_key`).

```rust
pub struct Query<Row> {
    stream: SyncStreamName,
    params: StreamParams,          // BTreeMap, canonical
    order_by: Option<OrderBy>,
    limit: Option<u32>,
    _marker: PhantomData<Row>,
}

impl<Row> Query<Row> {
    pub fn key(&self) -> QueryKey { /* canonical hash */ }
}
```

The macro generates per-resource builders (carried forward from RFC 085):

```rust
use issues::field;
let q: Query<Issue> = Issue::query()
    .eq(field::workspace_id, w1)
    .any_of(field::status, [Status::Open, Status::InProgress])?
    .order_by("created_at", Order::Desc)
    .limit(50)
    .build();
```

`field::*` markers + sealed comparator traits + comparator wrappers (`InSet`, `Range`, `Contains`) port over unchanged from `pocopine-sync-crud`.

### 2. `QueryClient` registry

```rust
pub struct QueryClient {
    sync: SyncClient,                                       // shared
    registry: RefCell<HashMap<QueryKey, Rc<dyn AnyQuerySubscription>>>,
    mutators: HashMap<TypeId, Box<dyn AnyMutator>>,
}

impl QueryClient {
    pub fn subscribe<R: Send + 'static>(&self, query: Query<R>) -> QueryHandle<R>;
    pub fn mutate<M: Mutator>(&self, payload: M::Payload) -> MutationFuture<M>;
}

struct QuerySubscription<Row> {
    query: Query<Row>,
    state: Rc<RefCell<QueryState<Row>>>,
    refcount: Cell<usize>,
    live_wakeup: Option<LiveSubscription>,
    epoch: SyncEpoch,
}

pub struct QueryHandle<Row> {
    subscription: Rc<QuerySubscription<Row>>,
}

impl<Row> Drop for QueryHandle<Row> {
    fn drop(&mut self) {
        // Decrement refcount; if zero, cancel background tasks via epoch.bump().
    }
}
```

Each `QuerySubscription` owns its own state, its own pending queue (durably keyed by `local_stream_key(stream, params)`), its own cursor, its own schema-version fence, and its own live wakeup. **No selector-shared state, no bare-stream queue, no active-local-key tracking.**

### 3. Mutators

A `Mutator` is a transactional function. The engine routes its output to every matching subscription.

```rust
pub trait Mutator: 'static {
    type Payload: Serialize + DeserializeOwned + 'static;
    type Row: Clone + Send + 'static;

    fn apply_local(&self, payload: &Self::Payload) -> Vec<RowChange<Self::Row>>;

    fn apply_remote(
        &self,
        ctx: RequestContext,
        payload: Self::Payload,
    ) -> impl Future<Output = SyncResult<Vec<RowChange<Self::Row>>>> + Send;
}

pub enum RowChange<R> {
    Upsert(R),
    Delete(RowKey),
}
```

When the user calls `client.mutate::<CreateIssue>(payload)`:

1. `apply_local(payload)` returns row changes.
2. Engine iterates active subscriptions for the relevant stream.
3. For each `(query, state)`, evaluates `query.matches(&row)` (generated predicate evaluator).
4. Matching subscriptions: upsert / delete into the state's pending overlay.
5. Non-matching subscriptions: skip.
6. Mutation goes on the wire to `/push`.
7. Server response (canonical row state) gets evaluated against subscriptions the same way.

This is the model that makes "create a W2 row while observing W1" correct without any "active subscription" question.

### 4. Predicate evaluator

The macro generates a typed `matches` method on `Query<Row>`:

```rust
// macro emission for Issues
impl Query<Issue> {
    pub fn matches(&self, row: &Issue) -> bool {
        // For each declared param, check the row's field against the
        // comparator. Generated from the typed declaration.
        if let Some(w) = self.params.get("workspace_id") {
            if &row.workspace_id != w { return false; }
        }
        // ... etc per declared comparator
        true
    }
}
```

Comparators evaluated client-side: `eq`, `in_set` (contains), `range` (within bounds), `contains` (substring). The macro reads the typed `params(...)` declaration and emits the evaluator. No reflection, no runtime DSL parsing.

### 5. Wire protocol

**No changes from RFC 085.** Reuses `SyncStreamSubscription { stream, params }` envelope verbatim. The `params` map includes `__order_by` and `__limit` as reserved keys when the query has ordering or pagination. Server's `validate_params` still validates the shape; new reserved keys are documented.

The QueryClient builds `SyncOpenRequest` / `SyncPullRequest` / `SyncPushRequest` directly. The Batch 4 work is the foundation it reads.

### 6. Server-side surface

`pocopine-sync-query` does NOT add new server-side traits. Source authors implement `SyncStreamSource` directly, OR use a forthcoming `QuerySource` adapter that:

* takes a typed `Row`, `Mutator`, and a list of supported queries;
* delegates to `pocopine-sync-crud`'s mutation-log + transaction-binding contracts for push handling;
* uses `request.params` to filter `pull` responses.

`QuerySource` can be built on top of `CrudSource` as an adapter — CRUD's safe/rigid push semantics are exactly what we want for transactional writes.

### 7. Local store

**No `SyncLocalStore` trait changes.** Each `QuerySubscription` keys all its local-store ops by `local_stream_key(stream, params)`. There is no bare/composite split because there are no "CRUD writes without subscription context" — every mutation flows through a subscription via predicate routing.

### 8. Live wakeup

Each `QuerySubscription` subscribes to the per-stream wakeup topic (`sync:stream:{name}`) and filters in the callback:

```rust
move |event| {
    if epoch.is_stale() { return; }
    // The wakeup carries affected row keys + minimal field set;
    // evaluate against this query's predicate.
    if event_matches_query(&event, &query) {
        start_pull_for_subscription(...);
    }
}
```

Per-`(stream, params_hash)` topics are a future optimization (opt-in at server registration), tracked as Phase 4 of the migration plan.

## Alternatives considered

### A. Retrofit subscription registry into CRUD

Add a registry under `CrudClientResource` that keys state by `(Resource, params)`. CRUD writes find their active subscription via thread-local or selector inspection.

Rejected: this is what the 16-pass codex loop tried to converge on. It compromises CRUD's design invariants (one Resource → one state) to support a use case CRUD wasn't built for. The result is a CRUD that's neither safe-and-rigid nor cleanly flexible.

### B. Drop CRUD entirely, only ship Query

Rejected: CRUD's safety-by-rigidity is the right answer for a large class of apps. TodoMVC, blog comments, settings pages don't need partial replication. A Query-only world would force complexity on every user, including those who don't need it.

### C. Make Query a feature flag on CRUD

Rejected: same as (A) with extra `#[cfg]`-flag complexity. The model split is real; hiding it behind a feature flag doesn't change that.

### D. SQL passthrough

Rejected: violates the "bounded comparator vocabulary" goal. SQL passthrough requires a query parser, predicate AST, server-side query planner, and a much larger client-side evaluator. Out of scope for a typed Rust framework.

## Reference implementation

The shape-subscription work on `wip/sync-shape-subs-batch-4` is the reference for:

* **Wire envelope** (`SyncStreamSubscription`, symmetric serialize, null-tolerance) — port as-is.
* **Macro DSL** (`#[resource(params(...))]`, comparator wrappers, sealed trait gate, type-safe field markers) — port to `pocopine-sync-query` macros, with two additions: `order_by` and `limit`.
* **Server-side validators** (`validate_params`, `validate_push_params`, `local_stream_key`) — port as-is.

The Batch 4 branch also documents what NOT to do:

* Don't share state across `(stream, params)` via selectors — observe via the registry.
* Don't split pending queue from snapshot — keep them together per-subscription.
* Don't add `active_local_key` or per-await staleness gates — each subscription owns its lifecycle.

## Migration plan

### Phase 1 — Land Batch 4 as the reference branch

`wip/sync-shape-subs-batch-4` ships as-is. Documents shape subscriptions as CRUD's "best-effort interim" with known limits in `docs/sync-shape-subscriptions.md`. No further band-aids.

### Phase 2 — Revert CRUD's shape-subscription integration

A separate PR on a new branch (`wip/revert-crud-shape-integration`) reverts the CRUD-side shape work back to clean main:

* `pocopine-sync-crud`: removes `validate_params` override on CRUD resources, removes the `params: StreamParams` field on `CrudClientResource`, removes the macro's `params(...)` emissions.
* `pocopine-sync`: keeps the wire envelope changes (`SyncStreamSubscription`, `validate_params` / `validate_push_params` on `SyncStreamSource`, `local_stream_key`). These are wire-level and useful to Query.
* `pocopine-sync`'s client-side: removes the bare-vs-composite split, `active_local_key`, `is_active_local_key`, `reconcile_local_key`. Restores the simple selector-keyed state model that CRUD's invariants need.

After this revert, `pocopine-sync-crud` is back to its pre-RFC-085 simplicity. The wire protocol's shape envelope stays. Apps that depended on the interim shape DSL on CRUD switch to `pocopine-sync-query` once Phase 3 lands.

### Phase 3 — Implement `pocopine-sync-query`

New crate. Builds on `pocopine-sync` directly. Includes:

* `Query<Row>`, `QueryClient`, `QuerySubscription`, `QueryHandle` types.
* `Mutator` trait + predicate-routing engine.
* New proc-macro crate `pocopine-sync-query-macros` for the trait-gated query DSL (`.eq` / `.any_of` / `.range` / `.contains` on `QueryBuilder<Row>`) + the predicate evaluator generator.
* `params::*` comparator wrappers (moved from `pocopine-sync-crud`).
* Optional `CrudSource → QuerySource` adapter for transactional writes.

### Phase 4 — Per-`(stream, params_hash)` live topics

Server-side opt-in for fanout streams. Lower client filtering cost. Not blocking.

### Phase 5 — Recommendation

After both crates have been used in production examples (`examples/blog` on CRUD; `examples/issue-tracker` on Query), the docs recommend the path each app should take. The decision tree in `sync.md` becomes:

* **CRUD** — one entity, one view, simple optimistic writes. TodoMVC, settings, blog.
* **Query** — multi-tenant, filtered views, dashboards. Linear-clones, SaaS apps.

If `pocopine-sync-query` proves to be the better default, the recommendation flips and CRUD becomes the "simpler choice for trivial apps" path.

## Open questions

1. **Order/limit on the wire.** Reserved keys (`__order_by`, `__limit`) inside `params`, or new fields on `SyncStreamSubscription`? Reserved keys are zero-protocol-change; new fields are cleaner. Decide before Phase 3.
2. **Multi-stream queries.** A query that spans multiple streams (`Issues` JOIN `Users`) is out of scope for v1. Path forward in v2: graph queries à la InstantDB.
3. **Server-side query catalog.** Should the server advertise its supported queries (ElectricSQL style)? Probably yes as an OPTIONAL feature, for systems that prefer server-driven shapes. Phase 4+.
4. **Predicate evaluator language.** Currently typed Rust macros. Could it be JSON-portable (so a non-Rust client can subscribe to the same query)? Out of scope for v1.
5. **`CrudSource → QuerySource` adapter scope.** How much CRUD machinery (mutation log, transaction binding, schema migration) carries to Query? Almost all of it, but the adapter shape needs detailed design.

## Status

Draft. Awaiting design-doc-level detail in `docs/sync-query-design.md` and a tracking branch for Phase 3 implementation.

## Related

* RFC 085 — Shape subscriptions. This RFC inherits its wire protocol and macro vocabulary, and supersedes its client-side model for shape-aware apps.
* `docs/sync-design.md` — full sync framework design, including the architectural-tension analysis that motivated this RFC.
* `docs/sync-shape-subscriptions.md` — the cookbook for the Batch 4 reference implementation. Will be superseded by `docs/sync-query-cookbook.md` once Phase 3 lands.
