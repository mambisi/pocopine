# Sync client API

What you use in wasm to subscribe to queries, render rows, and push
typed mutations.

```text
   QueryClient                       ← one per app, installed via plugin
     │
     ├── Query<Row>::bind(&qc, projector)   ─▶ component sugar
     │
     ├── observe(Query<Row>)        ─▶ QueryView<Row>     ← raw reactive read
     │                                     ├── rows()
     │                                     ├── version()
     │                                     ├── on_update(callback)
     │                                     └── into_signal()
     │
     ├── push(payload, change)      ─▶ raw write (untyped)
     │
     └── TypedMutation::push(&qc)   ─▶ macro-emitted write
                                          └── .optimistic(closure)
```

The tutorial in [`sync.md`](./sync.md) walks the end-to-end flow. This
doc is the reference for the client surface.

## Install the plugin

`QueryClient` is provided as an app plugin — install once at app
startup, and every `#[component]` in the tree can request it via
`self.plugin::<Rc<QueryClient>>()`.

```rust
use pocopine::prelude::*;
use pocopine_sync_query::query_client_plugin;

fn app(app: App) -> App {
    app
        .register::<crate::IssueList>()
        .plugin(query_client_plugin())              // defaults to /__pocopine/sync/v1
}
```

The plugin is the only public constructor — direct
`QueryClient::new` / `with_endpoint` / `with_config` were removed in
the polish PR. For tests that need a bare client without the plugin
lifecycle, use [`QueryClient::without_driver`](#querycientwithout_driver-tests-only).

### Configure endpoint + driver

```rust
use std::time::Duration;
use pocopine_sync_query::QueryClientConfig;

app.plugin(
    query_client_plugin()
        .endpoint("/my-api/sync/v1")
        .config(QueryClientConfig {
            endpoint: "/my-api/sync/v1".into(),
            poll_interval: Some(Duration::from_secs(15)),
            disable_live: false,
            with_credentials: true,
            local_store: None,           // or Some(Rc::new(my_local_store))
        })
);
```

### `QueryClient::without_driver` (tests only)

```rust
let qc = QueryClient::without_driver();
```

Routing engine only — no `/pull` cycles or SSE wake-ups. The right
test default when you're driving the routing engine manually via
`mutate(...)` and don't want spawned background tasks.

## The `Query<Row>` DSL

`#[query_resource]` emits `Issue::query()` pre-filled with the stream
name. Chain filters; finish with `.bind(...)` for a component or
`.observe(&client)` to keep the raw view:

```rust
use myapp_shared::issue::{Issue, issues};
use pocopine_sync_query::Order;

let view = Issue::query()
    .eq(issues::field::workspace_id, "W1")
    .eq(issues::field::status, "open")
    .any_of(issues::field::assignee, ["alice", "bob"])?
    .range(issues::field::priority, 1..=3)
    .contains(issues::field::title, "auth")?
    .order_by(issues::field::created_at, Order::Desc)
    .limit(50)
    .observe(&client);
```

### Method surface

| Method                            | Behaviour                                                       |
|-----------------------------------|-----------------------------------------------------------------|
| `.eq(field, value)`               | Exact match. Always available on `#[query_param]` fields.       |
| `.any_of(field, iter)`            | Set membership. `SyncResult` (empty set is rejected).           |
| `.range(field, range)`            | Closed/half-open range. Ordered types only.                     |
| `.contains(field, needle)`        | Case-insensitive substring. String fields only.                 |
| `.contains_exact(field, needle)`  | Case-sensitive substring.                                       |
| `.order_by(field, Order::…)`      | Typed sort. Field marker carries the wire key as a const.       |
| `.order_by_raw("field_name", …)`  | Escape hatch for synthesized / server-only sort columns.        |
| `.limit(n)`                       | Row cap. Default: backend's `max_snapshot_rows`.                |
| `.bind(&qc, projector)`           | Subscribe + copy rows into a component field.                   |
| `.observe(&client)`               | Subscribe; returns raw `QueryView<Row>`.                        |
| `.build()`                        | Finalise without subscribing. Returns `Query<Row>`.             |

All field markers are typed: `field::workspace_id` only accepts a
`String`-shaped value because `workspace_id: String` on the row.
Mismatches are build errors, not runtime "field not found".

### Stable identity

Two queries with the same stream + params + order + limit produce the
same `QueryKey`. `client.observe(...)` deduplicates against that key —
two calls for the same logical query share one underlying
subscription, refcounted independently.

## `.bind` — the component bridge

The recommended way to plug a query into a `#[component]`:

```rust
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct IssueList {
    #[prop] pub workspace_id: String,
    pub rows: Vec<Issue>,
}

#[handlers]
impl IssueList {
    pub fn on_mount(&mut self) {
        let qc = self.plugin::<Rc<QueryClient>>();
        Issue::query()
            .eq(issues::field::workspace_id, &self.workspace_id)
            .bind::<Self, _>(&qc, |s: &mut Self| &mut s.rows);
    }
}
```

Under the hood, `.bind(qc, projector)`:

1. Calls `.observe(qc)` to get a `QueryView<Row>`.
2. Bridges it into a `Signal<Vec<Row>>` via `.into_signal()` (the
   signal owns a drop-guard that holds the view + on_update token).
3. Runs `effect_scoped(|| { let snap = signal.get(); this.update(...) })`
   — the effect is tied to the surrounding component scope.
4. On component unmount, the effect drops, the signal drops, the
   token drops → on_update unregisters and the subscription
   refcount decrements.

Zero magic strings, zero scope/track/trigger bookkeeping, zero token
lifetimes to manage. The component field becomes a normal reactive
field; the `.poco` template binds via `pp-for` like any other.

## Raw `QueryView<Row>`

```rust
impl<Row: 'static> QueryView<Row> {
    pub fn rows(&self) -> Vec<Row>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn version(&self) -> u64;
    pub fn on_update<F: Fn() + 'static>(&self, callback: F) -> UpdateToken<Row>;
    pub fn into_signal(self) -> Signal<Vec<Row>>     // T1.2: drop-safe bridge
        where Row: Clone + Serialize;
}
```

Use when you need something `.bind` doesn't cover — a derived
aggregate, a selector body that doesn't write to a component field,
or a service module outside any component scope.

### `into_signal()`

```rust
let rows = Issue::query()
    .eq(issues::field::workspace_id, "W1")
    .observe(&qc)
    .into_signal();    // Signal<Vec<Issue>>

// Use it anywhere a Signal works:
pocopine_core::effect(move || {
    let count = rows.get().len();
    update_badge(count);
});
```

`into_signal()` owns the view + the on_update token via the signal's
drop-guard. When the last clone of the returned signal drops, the
subscription tears down — no explicit cleanup needed.

### `on_update` (manual)

```rust
let view = Issue::query().eq(field::ws, "W1").observe(&qc);
let _token = view.on_update(|| { /* re-render */ });
```

Returned `UpdateToken` unregisters on drop. Hold it as long as you
care about updates. For component use, prefer `.bind()` or
`.into_signal()` — they handle the lifetime automatically.

`on_update` fires on:

- `/pull` returns new canonical rows
- An optimistic overlay added or rolled back
- A live SSE wake-up triggers a re-pull
- A matching mutation routed through (from this client)

## Typed writes

`#[query_resource(name = "issues", draft = IssueDraft)]` emits typed
methods on `Issue`:

```rust
Issue::create(id, draft)                                -> TypedMutation
Issue::update(id, draft, expected_version: Option<…>)   -> TypedMutation
Issue::delete(id, expected_version: Option<…>)          -> TypedMutation
```

### One-time `From<(Id, Draft)>` impl

`Issue::create` and `Issue::update` use `Self::from((id, draft))` to
build the default optimistic overlay. Declare the conversion once
near the row struct (in shared code, alongside the row + draft
definitions):

```rust
impl From<(String, IssueDraft)> for Issue {
    fn from((id, draft): (String, IssueDraft)) -> Self {
        Self {
            id,
            version: String::new(),       // server-controlled
            created_at: String::new(),    // server-controlled
            workspace_id: draft.workspace_id,
            status: draft.status,
            title: draft.title,
        }
    }
}
```

Without this impl, `Issue::create(...)` fails to compile (clear bound
error). Server-controlled fields use `Default` (`String::new()`,
`None`, etc.); the rest comes from the draft.

### Pushing — the one-line form

```rust
let mutation_id = Issue::create(row_id, draft).push(&qc).await?;
```

`.push(&qc)`:

- Builds the optimistic Row via `Self::from((id.clone(), draft.clone()))`.
- Routes it through every matching `QueryView` (pending overlay).
- Generates a fresh UUIDv7-backed `MutationId` automatically.
- Resolves the push URL from `qc.endpoint()`.
- Reads the stream name from the macro-emitted `Issue::create(...)` builder.
- Inspects the server response: `accepted` → `Ok(id)`;
  `rejected` / `conflicts` / "id not in accepted" → `Err(...)`.

### Override / opt out

```rust
// Custom optimistic — predicts a server-computed field explicitly.
Issue::create(row_id, draft)
    .optimistic(|p| Issue { /* custom shape */ })
    .push(&qc).await?;

// No overlay — server confirms before the view updates.
Issue::create(row_id, draft).server_only().push(&qc).await?;
```

Use `.server_only()` when the optimistic shape is hard to predict
(server-assigned fields beyond id/version) or when a brief flicker
is better than showing a rolled-back row.

### Retry-safe writes — `push_with_id`

```rust
let id = my_durable_counter.next_mutation_id()?;
Issue::update(row_id, draft, expected_version)
    .push_with_id(&qc, id.clone())
    .await?;
```

Use when retries across reloads must collapse to the same logical
write at the server-side `MutationLog`. You own the id; the framework
just ships it. UUIDv7 from a client-persisted high-water mark is the
common pattern.

### What `.optimistic(...)` does

```mermaid
sequenceDiagram
    participant App as caller
    participant Client as QueryClient
    participant View as QueryView
    participant Server as SourceResource

    App->>Client: Issue::create(id, draft).optimistic(build).push(&qc)
    Client->>Client: build(&payload) → Issue
    Client->>View: route through pending overlay
    View-->>App: on_update fires (instant)

    Client->>Server: POST /sync/v1/push
    Server->>Server: extract_context
    Server->>Server: take_processing_payload
    Server->>Server: reserve_mutation
    Server->>Server: Source::create

    alt Accepted
        Server-->>Client: { accepted: [mutation_id] }
        Note over View: overlay stays; next /pull replaces with canonical
    else Rejected / conflict
        Server-->>Client: { rejected: [...] }
        Client->>View: RollbackGuard removes overlay
        View-->>App: on_update fires; caller gets SyncError
    else Transport error
        Server--xClient: network error
        Client->>View: RollbackGuard removes overlay
        View-->>App: on_update fires; caller gets SyncError
    end
```

With `.server_only()`, `.push(&qc)` skips the local routing engine
and just POSTs the wire envelope. The view updates on the next
`/pull`. Either branch surfaces server-side rejections as
`Err(SyncError::...)`.

### Lower-level `push` (advanced)

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

Use when you need to ship a payload shape `#[query_resource]` doesn't
model (e.g. a `MoveIssueBetweenColumns { ... }` payload that touches
the `Issue` collection but isn't a plain create/update). For typed
writes, prefer the macro-emitted `.push(&qc)`.

## `MutationId`

```rust
pub struct MutationId(/* opaque string */);

impl MutationId {
    pub fn new(value: impl Into<String>) -> SyncResult<Self>;
    pub fn uuid() -> Self;     // UUIDv7-backed, the default `.push(&qc)` uses
    pub fn as_str(&self) -> &str;
}
```

The client owns the id. For retry-safe writes the id MUST be stable
across reloads — `.push_with_id` is the entry point for that path.
`MutationId::uuid()` (used by `.push(&qc)`) generates a fresh
UUIDv7 per call; do NOT use it for hand-managed retries.

## Pending overlay vs canonical

```text
Canonical rows   ◀── /pull                  (server-authoritative)
   │   │   │
   │   │   │  merged on read
   ▼   ▼   ▼
Pending overlays ◀── .optimistic(closure)   (client-tentative)
   │
   │  supersedes canonical by row key
   ▼
QueryView::rows()
```

`rows()` returns the merged view. A pending overlay supersedes a
canonical row of the same key until the canonical version overrides
it on the next `/pull`. Rejected mutations remove the overlay and
restore the displaced canonical (if any).

## Live wake-up config

Live wake-up is on by default. The driver opens one SSE stream per
active `(stream, params_hash)` topic and triggers `/pull` on the
matching subscription. Subscriptions are partitioned by the macro's
`partition_for_topic`, so a workspace-W1 subscriber only wakes on W1
mutations.

Disable for offline-only flows or tests:

```rust
app.plugin(
    query_client_plugin().config(QueryClientConfig {
        disable_live: true,
        poll_interval: Some(Duration::from_secs(5)),  // poll only
        ..Default::default()
    })
);
```

The SSE endpoint is derived from the sync endpoint by the underlying
`pocopine-live` plugin — install `live_plugin()` alongside
`query_client_plugin()` if your app doesn't already.

## Cleanup

`.bind(...)` ties the subscription lifetime to the component scope
via `effect_scoped`. Unmounting drops the effect, which drops the
signal's drop-guard, which drops the on_update token AND the last
`QueryView` clone. The subscription's refcount hits zero, the
driver cancels, live topics unsubscribe — all automatic.

Raw `QueryView` requires either:

- Holding the `UpdateToken` from `on_update` for the desired lifetime.
- Wrapping in `into_signal()` and holding the signal.

Don't leak views into globals.

## See also

- Tutorial: [`sync.md`](./sync.md)
- Server contract: [`sync-server.md`](./sync-server.md)
- Selector layer: [`sync-query-selector-mechanism.md`](./sync-query-selector-mechanism.md)
