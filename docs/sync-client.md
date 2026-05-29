# Sync client API

What you use in wasm to subscribe to queries, render rows, and push
typed mutations.

```
   QueryClient                  ← one per app
     │
     ├── observe(Query<Row>) ──▶ QueryView<Row>      ← reactive read
     │                              │
     │                              ├── rows()
     │                              ├── version()
     │                              └── on_update(callback)
     │
     ├── push(payload, change) ──▶ raw write (untyped)
     │
     └── push_typed(TypedMutation) ──▶ macro-emitted write
                                       │
                                       └── .optimistic(closure)
```

The tutorial in [`sync.md`](./sync.md) walks the end-to-end flow. This
doc is the reference for the client surface.

## `QueryClient`

```rust
impl QueryClient {
    pub fn new() -> Self                              // default config
    pub fn with_endpoint(endpoint: String) -> Self    // override /sync
    pub fn with_config(config: QueryClientConfig) -> Self
    pub fn without_driver() -> Self                   // tests: no SSE
}
```

`QueryClient::new()` enables the background driver: per-subscription
`/pull` cycles and live SSE wake-ups. `without_driver()` is for tests
that drive the routing engine manually.

Hold one `QueryClient` for the app's lifetime (typically in a top-level
provider). Subscriptions are refcounted — dropping the last
`QueryView`/`QueryHandle` for a query releases its slot.

## The `Query<Row>` DSL

`#[query_resource]` emits `Issue::query()` pre-filled with the stream
name. Chain filters and finish with `.observe(&client)`:

```rust
use myapp_shared::issue::{Issue, issues};
use pocopine_sync_query::Order;

let view = Issue::query()
    .eq(issues::field::workspace_id, "W1")
    .eq(issues::field::status, "open")
    .any_of(issues::field::assignee, ["alice", "bob"])?
    .range(issues::field::priority, 1..=3)
    .contains(issues::field::title, "auth")?
    .order_by("created_at", Order::Desc)
    .limit(50)
    .observe(&client);
```

### Method surface

| Method                          | Behavior                                                       |
|---------------------------------|----------------------------------------------------------------|
| `.eq(field, value)`             | Exact match. Always available on `#[query_param]` fields.      |
| `.any_of(field, iter)`          | Set membership. Returns `SyncResult` (empty set is rejected).  |
| `.range(field, range)`          | Closed/half-open range. Numeric/ordered types only.            |
| `.contains(field, needle)`      | Case-insensitive substring. String fields only.                |
| `.contains_exact(field, needle)`| Case-sensitive substring.                                      |
| `.order_by(field, Order::…)`    | Sort. Backend honours via `query.order_by()` in `Source::list`.|
| `.limit(n)`                     | Cap row count. Default: backend's `max_snapshot_rows`.         |
| `.observe(&client)`             | Finalise + subscribe. Returns `QueryView<Row>`.                |
| `.build()`                      | Finalise without subscribing. Returns `Query<Row>`.            |

The `field::*` markers are typed: `field::workspace_id` only accepts
`String`-shaped values; using an integer there is a build error. The
type comes from your `#[query_param]` field's Rust type.

### Stable identity

Two queries with the same stream + params + order + limit produce the
same `QueryKey`. `client.observe(...)` deduplicates against that key —
two `observe(...)` calls for the same logical query share one
underlying subscription, refcounted independently.

## `QueryView<Row>`

```rust
impl<Row: 'static> QueryView<Row> {
    pub fn rows(&self) -> Vec<Row>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn version(&self) -> u64;
    pub fn on_update<F: Fn() + 'static>(&self, callback: F) -> UpdateToken<Row>;
}
```

### Reactive listener

```rust
let view = Issue::query().eq(field::workspace_id, "W1").observe(&client);

let _token = view.on_update({
    let view = view.clone();
    move || {
        render(&view.rows());
    }
});
```

`on_update` fires on:
- `/pull` returns new canonical rows
- An optimistic overlay is added or rolled back
- A live SSE wake-up triggers a re-pull
- A matching mutation is routed through (typed write from this client)

The returned `UpdateToken` unregisters on drop. Hold it as long as you
care about updates.

### `rows()` vs `len()` vs `version()`

- `rows()` clones — call sparingly inside render.
- `len()` and `is_empty()` use a counter; cheap.
- `version()` is an opaque token; compare to a saved value to skip
  redundant work when the view hasn't changed.

## Typed writes

`#[query_resource(name = "issues", draft = IssueDraft)]` emits typed
methods on `Issue`:

```rust
Issue::create(id, draft)                                   -> TypedMutation
Issue::update(id, draft, expected_version: Option<…>)      -> TypedMutation
Issue::delete(id, expected_version: Option<…>)             -> TypedMutation
```

Returned builder:

```rust
impl<Row, Id, Draft> TypedMutation<Row, Id, Draft> {
    pub fn optimistic<F>(self, build: F) -> Self
        where F: FnOnce(&MutationPayload<Id, Draft>) -> Row + 'static;
    pub fn payload(&self) -> &MutationPayload<Id, Draft>;
    pub fn wire_row_key(&self) -> Option<&RowKey>;
}
```

### Pushing

```rust
use pocopine_sync::{MutationId, SyncStreamName};

let mutation = Issue::create(row_id.clone(), draft)
    .optimistic(|payload| Issue {
        id: row_id.clone(),
        version: String::new(),
        workspace_id: payload.draft().workspace_id.clone(),
        status: payload.draft().status.clone(),
        title: payload.draft().title.clone(),
    });

client.push_typed(
    SyncStreamName::new("issues")?,
    next_mutation_id(),
    mutation,
    "/sync/v1/push",
).await?;
```

### What `.optimistic(...)` does

```
Issue::create(id, draft).optimistic(build).push_typed(...)
        │
        ▼  client side
   1. build(&payload) → Issue
   2. route through every matching QueryView's pending overlay
        │
   3. POST /sync/v1/push
        │
        ▼  server side
   4. take_processing_payload → reserve_mutation → Source::create
        │
        ▼  client side
   5a. Accepted → overlay stays; next /pull replaces with canonical.
   5b. Rejected/conflict → RollbackGuard removes overlay;
       on_update fires; caller gets SyncError.
   5c. Transport error → same as rejected.
```

Without `.optimistic(...)`, `push_typed` skips the local routing engine
and just POSTs the wire envelope. The view updates on the next `/pull`.

### When NOT to use optimistic

- Writes whose canonical shape you can't predict (server-assigned
  fields beyond `id` + `version`).
- Writes where showing a brief flicker is better than showing a
  rolled-back row.

## `MutationId`

```rust
pub struct MutationId(/* opaque */);
```

The client owns the id. It MUST be stable across retries — if the user
reloads mid-flight, a replay with the same id collapses to one logical
write at the `MutationLog`. Use a durable counter or a UUIDv7 seeded
from a client-persisted high-water mark.

The framework's [pending-overlay store] persists the in-flight mutation
+ id across reloads; on next online tick it re-pushes with the same id.

## Pending overlay vs canonical

```
Canonical rows   ◀── /pull        (server-authoritative)
   ┃   ┃   ┃
   │   │   │  merged on read
   ▼   ▼   ▼
Pending overlays ◀── optimistic    (client-tentative)
   │
   ▼
QueryView::rows()
```

`rows()` returns the merged view. A pending overlay supersedes a
canonical row of the same key until the canonical version overrides it
on the next `/pull`. Rejected mutations remove the overlay and restore
the displaced canonical (if any).

## Lower-level `push` (advanced)

```rust
pub async fn push<P, Row>(
    &self,
    stream: SyncStreamName,
    mutation_id: MutationId,
    payload: P,
    change: RowChange<Row>,
    push_url: &str,
) -> SyncResult<()>
where P: Serialize, Row: Clone + Serialize + 'static;
```

`RowChange::Upsert(row)` or `RowChange::Delete(key)`. Use this when you
need to ship a payload shape `#[query_resource]` doesn't model
(e.g. a `MoveIssueBetweenColumns { ... }` payload that touches the
`Issue` collection but isn't a plain create/update). For typed writes,
prefer `push_typed`.

## Live wake-up config

```rust
let client = QueryClient::with_config(QueryClientConfig {
    sync_endpoint: "/sync/v1".to_string(),
    live_endpoint: Some("/live/v1/issues".to_string()),
    ..Default::default()
});
```

The driver opens one SSE stream per active `(stream, params_hash)` and
triggers `/pull` on the matching subscription when an event arrives.
Subscriptions are partitioned by the macro's `partition_for_topic`, so
a workspace-W1 subscriber only wakes on W1 mutations.

## Cleanup

`QueryView` holds one `Rc` to its subscription. Dropping the last view
+ all `UpdateToken`s releases the subscription, cancels its driver, and
unsubscribes from live topics. Hold views inside reactive components
that own their lifetime; don't leak them into globals.

## See also

- Tutorial: [`sync.md`](./sync.md)
- Server side: [`sync-server.md`](./sync-server.md)
- Selector layer: [`sync-query-selector-mechanism.md`](./sync-query-selector-mechanism.md)
