# RFC 085 - Shape subscriptions

| Field | Value |
|---|---|
| **Status** | Final |
| **Author** | pocopine team |
| **Created** | 2026-05-26 |
| **Related** | [`rfc-071-event-spine-and-live-invalidation.md`](./rfc-071-event-spine-and-live-invalidation.md), [`rfc-072-offline-sync-protocol.md`](./rfc-072-offline-sync-protocol.md), [Axis 2 in `docs/sync-local-first-gaps.md`](../docs/sync-local-first-gaps.md), [`docs/sync-shape-subscriptions.md`](../docs/sync-shape-subscriptions.md) (cookbook) |
| **Supersedes** | - |

## 1. Summary

Pocopine sync today identifies streams by an opaque `SyncStreamName` string. To express "issues in workspace W" or "issues assigned to alice with status in {open, in_progress}", an author must either mint one `SyncStreamName` per filter combination — exploding the server registry as workspaces and users are added — or pull every row server-side and filter locally, defeating the point of sync. Linear documented this scaling cliff publicly; every survey engine (ElectricSQL shapes, PowerSync sync rules, InstantDB InstaQL) treats the **(stream + parameters)** pair as the unit of subscription, not the stream alone.

This RFC closes Axis 2 of the local-first gap analysis by introducing parameterized subscriptions on the existing protocol envelope, a typed `params(...)` DSL on `#[resource]`, a query-builder layer with the comparator vocabulary `eq / in / range / contains`, and a subscription registry inside `SyncClient` that refcounts identical `(name, params)` pairs so two components observing the same query share one underlying task. The wire change is additive (`#[serde(default)]`, no protocol bump), the macro change is opt-in, and the contract surface stays shaped so a future P2P layer can apply params locally without server mediation.

```rust
#[resource(
    name = "issues",
    schema_version = 2,
    params(
        workspace_id: WorkspaceId,        // required eq
        assignee_id: Option<UserId>,      // optional eq
        status: params::InSet<Status>,    // in
        created_at: params::Range<DateTime<Utc>>,
        title: params::Contains,
    ),
)]
#[pocopine_sync_crud::async_trait]
impl CrudSource for IssuesSource { /* ... */ }

// Two equivalent client surfaces, sharing one underlying subscription:
let view = Issues::stream()
    .workspace_id(workspace)
    .status_in([Status::Open, Status::InProgress])
    .observe()?;

let view = Issues::query()
    .where_eq(field::workspace_id, workspace)
    .where_in(field::status, [Status::Open, Status::InProgress])
    .observe()?;
```

## 2. Motivation

Linear's [reverse-engineered sync engine](https://github.com/wzhudev/reverse-linear-sync-engine) documents the failure mode plainly: when streams are opaque, every multi-tenant filter combination must be its own stream registration, and the broadcast topic graph scales with `O(workspaces × users × filter_combinations)`. They rewrote the system after hitting the cliff. ElectricSQL's [shapes](https://electric.ax/docs/guides/shapes) and PowerSync's [sync rules](https://docs.powersync.com/usage/sync-rules) both make the parametric subscription the unit; InstantDB ships [InstaQL](https://instantdb.com/docs/instaql) as the same idea wrapped in a query DSL.

Pocopine's current `SyncStreamName` is a validated opaque string (`crates/pocopine-sync/src/protocol.rs:114`). The `SyncStreamSource` trait registers streams by that string; `pull_handler` and `push_handler` look the source up and dispatch. The `tenant_boundary.rs` regression test exercises the existing workaround — one stream per collection, with the source reading `x-tenant-id` from `RequestContext` and partitioning rows by tenant. That works for tenant isolation but does not generalize to client-driven filters ("assigned to me", "status in {…}"), nor does it dedupe materialization across components.

The Axis 2 gap-doc entry (`docs/sync-local-first-gaps.md`, line 27) rates this **P1**. After Axis 1 (schema versioning, merged via PRs #128, #130, #133) it is the next gating prerequisite for any honest multi-tenant app on pocopine.

## 3. Goals

- **Multi-tenant without registry explosion.** One `#[resource]` declaration, many parametric subscriptions, one server registration.
- **Typed parametric subscriptions.** Parameter names, types, and comparators are declared in the resource and validated at the macro layer; misuse is compile-time, not runtime.
- **Ergonomic query DSL.** `Issues::query().where_eq(...).where_in(...).observe()` reads naturally and shares cache keys with `Issues::stream()`.
- **Subscription dedup.** Components subscribing to identical `(name, params)` pairs share ONE underlying `SyncCollection` task, cursor, materialization, and live subscription.
- **Backwards-compatible wire change.** Old clients and old servers continue to interoperate (additive `#[serde(default)]` fields), mirroring the Axis 1 schema-versioning rollout.
- **P2P-shape preserved.** Params are inert metadata; a future peer can apply them locally to a replicated stream without server mediation.

## 4. Non-goals

- **Arbitrary SQL passthrough.** No `WHERE expr` strings, no SQL ASTs over the wire. The comparator vocabulary is fixed at `eq / Option<eq> / in / range / contains`.
- **Nested AND/OR predicate trees.** All params are AND-composed. `OR` semantics are out of scope; the design space is too easy to misuse and Linear's experience suggests the bounded vocabulary is enough.
- **Client-side joins.** "Orders + items" composition stays Axis 18; this RFC ships the substrate but not the join primitive.
- **P2P merge semantics.** Per-field LWW timestamps, Lamport vectors, and `state_vector / diff_since` APIs are Axes 5 + 20.
- **Per-`(name, params_hash)` topic routing on day one.** v1 routes live wake-ups on the per-collection topic and filters client-side; per-tuple topics are documented as a future opt-in (§5.6).

## 5. Design

### 5.1 Wire envelope

Add an additive `params: BTreeMap<String, Value>` field to the three request envelopes and the open-response stream entry. `BTreeMap` (sorted by key) makes serialization deterministic, which is load-bearing for the cache key (§5.5).

```rust
// crates/pocopine-sync/src/protocol.rs

/// Subscription to one stream, optionally narrowed by typed params.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncStreamSubscription {
    pub stream: SyncStreamName,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

pub struct SyncOpenRequest {
    pub protocol: String,
    #[serde(default)]
    pub client_id: Option<SyncDeviceId>,
    pub streams: Vec<SyncStreamSubscription>,   // was: Vec<SyncStreamName>
}

pub struct SyncOpenStream {
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub cursor: Option<SyncCursor>,
    #[serde(default)] pub schema_version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,        // echoed back; helps debug
}

pub struct SyncPullRequest {
    pub protocol: String,
    pub stream: SyncStreamName,
    #[serde(default)] pub params: BTreeMap<String, Value>,
    pub cursor: Option<SyncCursor>,
    pub limit: u32,
}

pub struct SyncPushRequest<M> {
    pub protocol: String,
    pub stream: SyncStreamName,
    #[serde(default)] pub params: BTreeMap<String, Value>,
    #[serde(default)] pub mutations: Vec<ClientMutation<M>>,
    #[serde(default, deserialize_with = "deserialize_schema_version_default_one")]
    pub schema_version: u32,
}
```

A custom `Deserialize` for `SyncStreamSubscription` accepts both the wrapped object and a bare string (old client emits `"posts"`, new server deserializes as `SyncStreamSubscription { stream: "posts", params: {} }`). Same trick as serde-untagged for typed enums; documented in the rollout section.

`SyncStreamName` itself stays a validated opaque string. Widening it to `{ name, params }` would break every log line, trace span, and downstream topic key — too invasive for too little gain.

### 5.2 Source trait surface

`SyncStreamSource::pull` and `push` already take the request envelope; they implicitly receive `request.params` via the field. A new method gates inbound validation:

```rust
pub trait SyncStreamSource: Send + Sync + 'static {
    // ...existing methods...

    /// Validate inbound params before authorization-checked dispatch.
    /// Default: accept anything (single-tenant sources).
    fn validate_params(
        &self,
        params: &BTreeMap<String, Value>,
    ) -> SyncResult<()> {
        let _ = params;
        Ok(())
    }
}
```

`open_handler` calls `validate_params` after `authorize`; an `Err` surfaces as `BadRequest` via `server_error`. Sources generated by `#[resource(params(...))]` get a strongly-typed override that deserializes each declared param to its native type and rejects unknown keys.

### 5.3 Macro DSL

The `#[resource]` attribute learns a `params(...)` clause. Each entry is `field_name: Type`, where `Type` is one of:

| Wrapper | Semantic | Wire shape |
|---|---|---|
| `T` (bare) | Required equality | `{ "field": "value" }` |
| `Option<T>` | Optional equality (omit on `None`) | `{ "field": "value" }` or absent |
| `params::InSet<T>` | Membership in a set (≥1 value) | `{ "field": { "in": ["v1", "v2"] } }` |
| `params::Range<T>` | Bounded range with optional inclusivity | `{ "field": { "from": "lo", "to": "hi", "inclusive": [true, false] } }` |
| `params::Contains` | Substring / case-sensitive flag | `{ "field": { "contains": "needle", "case_sensitive": false } }` |

```rust
// crates/pocopine-sync-crud/src/params.rs (new)
pub struct InSet<T>(pub Vec<T>);                       // non-empty checked at deserialize
pub struct Range<T> {
    pub from: Option<T>,
    pub to: Option<T>,
    pub inclusive: (bool, bool),
}
pub struct Contains {
    pub needle: String,
    pub case_sensitive: bool,
}
```

The macro generates:

- A typed `IssuesStreamParams` struct mirroring the declared shape.
- `impl IssuesStreamParams { fn serialize_params(&self) -> BTreeMap<String, Value> }` for the client→wire direction.
- `impl IssuesStreamParams { fn extract(params: &BTreeMap<String, Value>) -> SyncResult<Self> }` for the server-side typed extractor.
- A `field` module of const tokens (`field::workspace_id`, `field::status`, …) carrying each field's comparator type at the type level. This is what makes `Issues::query().where_in(field::workspace_id, [...])` fail to compile when `workspace_id` was declared as required `T`.
- The `validate_params` override on the generated `SyncStreamSource` impl that calls `IssuesStreamParams::extract` and converts an `Err` to `BadRequest`.
- Fluent builder methods on `Issues::stream()` — one per declared field, with method names derived from comparator (`.workspace_id(...)`, `.status_in([...])`, `.created_after(...)`, `.title_contains("...")`).

Compile-time guards reject:

- Unknown comparator wrappers (`params(x: SomethingElse<T>)`).
- Duplicate field names.
- A `field::name` token used with a comparator method that doesn't match its declared type.

### 5.4 Query DSL

`Resource::query()` is sugar over `Resource::stream()` with the same final-call semantics. Field references go through the macro-generated `field::*` tokens for type-safe dispatch:

```rust
let view = Issues::query()
    .where_eq(field::workspace_id, workspace)
    .where_in(field::status, [Status::Open, Status::InProgress])
    .where_range(field::created_at, ..now - Duration::weeks(2))
    .where_contains(field::title, "auth")
    .observe()?;
```

Both `Issues::stream()` and `Issues::query()` produce the same `BTreeMap<String, Value>` for the same logical params, and route through `SyncClient::subscribe(name, params)` (§5.5). Cache keys therefore match across surfaces: declaring a stream subscription and a query subscription with equivalent params yields ONE underlying task.

The DSL deliberately does not introduce expression composition, sub-queries, or runtime predicate evaluation. The intent is: every query at this layer is a flat conjunction of field predicates against the bounded vocabulary. Authors who need more reach for the typed `IssuesSource::pull` implementation server-side.

### 5.5 Subscription registry

```rust
// crates/pocopine-sync/src/client.rs
pub(crate) struct SubscriptionRegistry {
    inner: Arc<Mutex<HashMap<SubscriptionKey, SubscriptionHandle>>>,
}

#[derive(Clone, Hash, Eq, PartialEq)]
pub(crate) struct SubscriptionKey {
    pub stream: SyncStreamName,
    pub params_hash: [u8; 32],   // blake3(canonical_json(BTreeMap))
}

pub(crate) struct SubscriptionHandle {
    /// Refcounted Arc; when strong_count → 1 (the registry's), the
    /// underlying `SyncCollection` task is fenced via its epoch.
    inner: Arc<SubscriptionInner>,
}
```

The `params_hash` is derived from canonical JSON of the sorted `BTreeMap`. Sorting comes for free from `BTreeMap`'s insertion semantics; the canonical JSON pass keeps field-ordering and whitespace stable across builds. Collisions are not a concern at the cryptographic strength of blake3-256.

`SyncClient::subscribe(stream, params)` either returns the existing `SubscriptionHandle` (refcount ++) or spawns a new `SyncCollection` task and registers it (refcount = 1). When a handle drops and `Arc::strong_count` reaches the registry's lone reference, the task is fenced via its epoch and the entry is removed. A short grace window (e.g., 100ms) before final fencing lets navigation-style remounts revive without re-issuing `/open`.

Two consequences:

1. `LocalResourceView::observe_view` callers stop competing for storage cursors. Whichever component arrives first wins the initial pull; later subscribers ride the existing task's state.
2. The macro-generated `Resource::stream()`/`query()` must route through `subscribe` instead of constructing a bare `SyncCollection`. This is the breaking change inside the macro emissions — old code keeps compiling because the macro is updated alongside.

### 5.6 Live invalidation routing

`SyncServer::invalidate_stream` today publishes on `sync:stream:{name}` (`crates/pocopine-sync/src/server.rs::invalidate_stream`). The event payload is consumed by `open_live_wakeup` on the client and triggers `start_pull(reason: Live)` for every subscription on that stream.

This RFC keeps the topic at `sync:stream:{name}` in v1 and extends the wake-up payload to carry the **filterable field projection** of the affected row:

```rust
// pocopine-live event payload (new fields)
struct SyncLiveWakeup {
    stream: SyncStreamName,
    affected_keys: Vec<RowKey>,
    affected_fields: BTreeMap<RowKey, BTreeMap<String, Value>>,
    // Only fields declared in `params(...)` appear here.
}
```

Clients receive the wake-up and evaluate their captured `params` against `affected_fields` before triggering a pull. Mismatches are dropped silently. This is **approach (b)** from the gap-doc analysis: bandwidth tradeoff (every subscription on a stream wakes), but no server-side per-client state and no topic explosion.

v2 (future, opt-in at source registration): per-`(name, params_hash)` topics for high-fanout collections. The wire surface here is forward-compatible — a server can publish on both topics simultaneously, and clients can subscribe to whichever is offered.

Sources that do not (or cannot) emit field projections fall back to the legacy whole-stream wake-up. Documented as a regression in granularity, not correctness; the source's pull still returns the right rows.

### 5.7 Tombstone-on-filter-departure

When an issue transitions from `status=open` to `status=closed`, clients subscribed with `status_in=[open]` must drop the row from their view even though the row still exists in canonical state. This is implemented as a `SyncOp::Delete` from the subscription's perspective — `pull_handler` emits the synthetic delete when an incremental cursor crosses a row whose new shape no longer matches the subscription's params.

Concretely: the source's `pull(ctx, request)` implementation receives `request.params`; for incremental pulls it must compare each candidate change against the params and emit `SyncChange { op: Delete, ... }` for rows that left the subscription, regardless of whether they were actually deleted server-side. CRUD-generated sources do this automatically via `IssuesStreamParams::matches(&row)`; bespoke sources must implement the same semantics or document an inconsistency.

A subscription-departure delete carries `key: Some(...)`, `row: None`, and `cursor: <post-departure>`, mirroring a real delete on the wire.

### 5.8 Cache key and cursor scoping

A cursor is bound to a specific `(stream, params_hash)` tuple. Two subscriptions with different `params` to the same stream have independent cursors and independent local cache slices.

The durable local store (`crates/pocopine-sync/src/local_store.rs`) is keyed today by `stream: SyncStreamName` alone (`hydrate_stream`, `save_snapshot`, …). This RFC requires extending the storage key to `(stream, params_hash)`:

- SQLite: bump `SCHEMA_VERSION` 4→5; add `params_hash blob` column to `__pocopine_streams`, `__pocopine_rows`, `__pocopine_mutations`; existing rows observe NULL (legacy = no-params subscription).
- IndexedDB: extend the streams-store key by appending the params_hash hex.

`hydrate_stream`, `save_snapshot`, `apply_changes`, `enqueue_mutation`, `clear_stream` all gain a `params_hash: Option<&[u8; 32]>` parameter. `None` means "the no-params subscription," matching existing data. The Axis 1 schema-versioning batches established the v3→v4 pattern; this is the same migration shape.

## 6. Alternatives considered

**(a) Encode params in the stream name string.** `SyncStreamName::new("issues?workspace=W&status=open,in_progress")`. Rejected: validation rules would have to parse arbitrary URL-encoded payloads; log lines become unreadable; the opaque-string contract is destroyed; backwards-compat is harder.

**(b) Make `SyncStreamName` a struct.** `SyncStreamName { name: String, params: BTreeMap<...> }`. Rejected: too invasive — breaks every existing log line, trace span, and topic-name derivation; the token-budget validation (1024-byte limit) would need re-specification.

**(c) Generic SQL predicate AST.** `where: Expr::And([Expr::Eq(field, value), Expr::Or([...])])`. Rejected: design space too large; security review burden too high; Triplit's experience suggests authors don't actually need it; opinionated-by-default applies hardest here.

**(d) Per-tuple topics from day one.** Publish on `sync:stream:{name}:{params_hash}` instead of `sync:stream:{name}`. Rejected for v1: enumerating params at registration time isn't always possible (open enums, range filters); the bandwidth saving doesn't justify the server-state burden until proven needed. Documented as v2.

**(e) Client-side filter only, no server params.** Server still serves the full unfiltered stream; the client materializes everything and filters in memory. Rejected: leaks data across tenant boundaries; defeats sync for collections with >1000 rows; preserves the storage cliff.

## 7. Implementation Status

**Status: Final.** Shipped via a 4-batch stack, each with focused review.

- **Batch 1** — Wire envelope (`SyncStreamSubscription { stream, params }`, bare-string-compat deserializer, `params` field on `SyncOpenStream` / `SyncPullRequest` / `SyncPushRequest`); `SyncStreamSource::validate_params` default-impl method; client `SyncCollection` threads params through every spawned task. ~320 LOC, 9 regression tests. PR: [#141](https://github.com/mambisi/pocopine/pull/141).
- **Batch 2** — Macro `params(...)` DSL with comparator inference (`T` / `Option<T>` / `InSet<T>` / `Range<T>` / `Contains`); typed `StreamParams` struct + `serialize_params` + `extract`; fluent `Resource::stream()` builder; `pocopine_sync_crud::params` module with comparator wrappers. ~1055 LOC, 11 tests. PR: [#142](https://github.com/mambisi/pocopine/pull/142).
- **Batch 3** — Type-safe query DSL with sealed-trait `field::*` markers; `Resource::query()` with `where_eq` / `where_in` / `where_range` / `where_contains` methods that fail to compile when applied to the wrong comparator kind. ~517 LOC, 3 tests. PR: [#143](https://github.com/mambisi/pocopine/pull/143).
- **Batch 4** — Auto-wired `SyncStreamSource::validate_params` via the CRUD builder so structural param errors surface as `BadRequest` before `pull` / `push` runs; cookbook page; RFC flipped to Final; gap doc + roadmap updated. PR: [#144](https://github.com/mambisi/pocopine/pull/144).

**Deferred** (filed as separate follow-ups, not blocking RFC closure):

- **`SyncClient` subscription registry** for `(stream_name, params_hash)` dedup + Arc refcount + grace-window cleanup. The wire contract is correct without it; this is a client-side performance optimization that makes two `observe_view` calls with equivalent params share one underlying task. Will file as "Batch 2b: subscription registry."
- **Auto-emission of tombstone-on-filter-departure deletes** from CRUD sources. The wire contract supports it (sources emit `SyncOp::Delete` for rows leaving the subscription's predicate set); today source authors implement the predicate evaluator manually. The macro will eventually generate this from the declared `params(...)`.
- **Per-`(name, params_hash)` topics** as the v2 live-routing approach. v1 uses per-collection topics + client-side filter, which is correct but bandwidth-suboptimal for high-fanout collections.

## 8. Migration and rollout

The wire change is additive. All new fields default-empty via `#[serde(default)]`:

- **Old client → new server.** Open request carries `streams: ["issues"]` (bare strings). The custom `SyncStreamSubscription` deserializer accepts the bare-string form and produces empty params. Source's `validate_params` receives `{}`. If the source requires a param, it returns `InvalidValue` → `BadRequest`; otherwise it serves the unfiltered stream. Apps that have never declared `params(...)` continue working.
- **New client → old server.** New client emits `streams: [{stream: "issues", params: {...}}]`. Old server's serde drops the unknown field shape, sees an unexpected structure, returns a deserialization error. Operators upgrade the server first; client deploys after.
- **Mixed deploys.** Server is always upgraded before client. Pocopine doesn't currently support multi-version deploys behind one LB except during rolling cutovers; the documented order is: deploy server → wait for all pods to acknowledge → deploy client.

The local-store schema bump (4→5) follows the v3→v4 pattern Axis 1 introduced. Existing rows observe `params_hash = NULL` (the no-params subscription); new param-bearing subscriptions write fresh rows. No backfill needed.

Examples (`counter`, `todo`, `blog`, `keep`, `sync`) continue to compile because `params(...)` is opt-in. The `#[resource]` macro emits an empty `StreamParams` struct when no `params(...)` is declared; the client's `Resource::stream()` builder becomes parameterless.

## 9. Related RFCs and references

- [RFC 071 — Event spine and live invalidation](./rfc-071-event-spine-and-live-invalidation.md): the topic substrate this RFC extends with affected-fields projection.
- [RFC 072 — Offline sync protocol](./rfc-072-offline-sync-protocol.md): the protocol envelope this RFC additively extends.
- Axis 1 schema versioning (merged): PRs [#128](https://github.com/mambisi/pocopine/pull/128), [#130](https://github.com/mambisi/pocopine/pull/130), [#133](https://github.com/mambisi/pocopine/pull/133). Pattern source for additive wire change, `#[serde(default)]` rollout, multi-batch sequencing.
- Linear sync engine reverse-engineering: [wzhudev/reverse-linear-sync-engine](https://github.com/wzhudev/reverse-linear-sync-engine).
- ElectricSQL shapes: [electric.ax/docs/guides/shapes](https://electric.ax/docs/guides/shapes).
- PowerSync sync rules: [docs.powersync.com/usage/sync-rules](https://docs.powersync.com/usage/sync-rules).
- InstantDB InstaQL: [instantdb.com/docs/instaql](https://instantdb.com/docs/instaql).
- Gap analysis: [`docs/sync-local-first-gaps.md`](../docs/sync-local-first-gaps.md), Axis 2.
