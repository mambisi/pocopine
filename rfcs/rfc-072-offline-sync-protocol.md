# RFC 072 - Offline sync protocol

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-03 |
| **Builds on** | [RFC 071](./rfc-071-event-spine-and-live-invalidation.md) |
| **Related** | [RFC 002](./rfc-002-app-stores-servers.md), [RFC 069](./rfc-069-observability.md), [RFC 073](./rfc-073-yrs-collaboration.md) |

## Implementation Status

Initial implementation has started in `pocopine-sync`:

- explicit extension crate, not a `pocopine` core feature,
- target-specific browser and host modules in one crate,
- `/__pocopine/sync/v1/open`, `/pull`, and `/push` protocol routes,
- `SyncServer`, `SyncStreamSource`, and `MemorySyncStream`,
- explicit `public_stream`, `guarded_stream`, and
  `guarded_stream_with` registration,
- stream guards run on `/open`, `/pull`, and `/push`,
- `SyncStreamSource::pull` / `push` receive `RequestContext` after the
  stream guard passes,
- browser `sync_plugin()`, `SyncClient`, and `CollectionState<T>`,
- browser `SyncClient::open()` calls `/open` before the first `/pull`,
- browser `SyncCollection::push()` applies optimistic rows and handles
  accepted, rejected, and conflict push outcomes,
- `SyncLocalStore`, `SyncLocalIdentity`, `MutationIdGenerator`, and
  `MemoryLocalStore` as the first local-store contract slice,
- `SyncCollection::open()` hydrates cached local rows, replays pending
  stored mutations, and persists pull responses through `SyncLocalStore`,
- `SyncCollection::push()` enqueues mutations before the network request
  and persists accepted/rejected/conflict outcomes,
- `MemorySyncStream` accepts upsert/delete/reset pushes with stable
  mutation-id dedupe,
- live wake-up integration through `pocopine-live` query-tag topics,
- runnable memory-backed example in `examples/sync`,
- Firefox wasm smoke coverage for `open -> pull -> render`.

Current model: `/open` is discovery/validation, not a session grant.
Every `/open`, `/pull`, and `/push` request runs the stream guard
independently, so skipping `/open` does not bypass access control.

Still future work: durable browser storage, cross-reload mutation replay
with the SQLite backend, conflict resolution UI, SQLx/database adapters,
CDC sources, and query-driven stream parameters. The local-store
implementation plan is documented in
[`docs/sync-local-store-plan.md`](../docs/sync-local-store-plan.md):
SQLite-first local storage, with SQLx kept as a later host/server
adapter.

## 1. Summary

Pocopine should treat sync as a protocol, not just a notification stream.
The sync layer coordinates snapshots, cursors, pulls, pushes, mutation
dedupe, gap recovery, and conflict policy for client-side data stores.

The first version is server-authoritative and database agnostic. It does
not require CRDTs. CRDTs remain the right tool for collaborative
documents, covered separately by RFC 073.

## 2. Problem Statement

Live invalidation answers "something changed; refresh this data." Offline
sync answers harder questions:

- Which subset of data should this client own locally?
- What cursor proves the client has seen all changes through a point in
  time?
- How does the client catch up after being offline?
- How are local mutations deduped when the network retries?
- What happens when the client edits old data?
- How does the server avoid leaking rows through deletes and tombstones?

These rules cannot be bolted onto a generic WebSocket event stream after
the fact. They need a stable protocol boundary.

## 3. Goals

- Define a future `pocopine-sync` crate and protocol.
- Define the frontend collection/query/mutation surface that consumes the
  protocol.
- Keep database adapters behind traits.
- Allow optional SQLx adapters for authors who want compile-time checked
  SQL without making SQLx a framework-wide dependency.
- Support initial snapshots, incremental pulls, mutation pushes, and
  cursor-based resume.
- Make sync streams explicit and guarded.
- Let components query normalized local data instead of forcing
  view-specific server endpoints.
- Provide safe defaults for conflict handling.
- Use `pocopine-live` only as an invalidation/wake-up path.
- Leave CRDT merge semantics to `pocopine-collab`.

## 4. Non-goals

- This RFC does not implement peer-to-peer sync.
- This RFC does not make every application local-first by default.
- This RFC does not expose arbitrary SQL filters to browsers.
- This RFC does not solve rich text or multi-user document editing.
- This RFC does not require a specific database engine.

## 5. Core Concepts

### 5.1 Stream

A stream is a named, authorized subset of application data:

```rust
pocopine::sync_stream! {
    posts_for_tenant {
        collection: posts;
        key: PostId;
        schema_version: 1;
        filter: |ctx, row| row.tenant_id == ctx.tenant_id();
        projection: PostSyncView;
    }
}
```

The client subscribes to stream names. It does not send table names, SQL
fragments, or raw database filters.

Streams are registered explicitly:

```rust
SyncServer::builder()
    .public_stream(public_posts_stream())
    .guarded_stream(user_posts_stream(), require_auth())
    .guarded_stream(admin_posts_stream(), require_role("admin"))
    .guarded_stream_with(tenant_posts_stream(), |ctx| async move {
        let user = ctx.require_user()?;
        ensure_tenant_access(user)?;
        Ok(())
    })
    .build();
```

There is no implicit public registration. Public streams are an explicit
author choice.

### 5.2 Cursor

A cursor is an opaque server-issued token. It may encode a backend offset
such as a Redis stream id, database LSN, sequence id, or framework
logical clock. Clients must not parse it.

If a cursor expires or is no longer available, the server returns `gap`
and the client must resnapshot the stream.

### 5.3 Change

The sync protocol has a stable change envelope:

```rust
pub struct SyncChange {
    pub stream: String,
    pub key: serde_json::Value,
    pub op: SyncOp,
    pub version: RowVersion,
    pub payload: Option<serde_json::Value>,
    pub cursor: SyncCursor,
}

pub enum SyncOp {
    Upsert,
    Delete,
    Reset,
}
```

Deletes carry keys and versions by default. Old row payloads require an
explicit opt-in and must pass the same read guard as normal rows.

## 6. Frontend Store Model

Pocopine sync should expose a frontend store model similar in spirit to
TanStack DB: typed collections, live queries over those collections, and
optimistic mutations that sync back to the server. Pocopine should not
copy TanStack DB's TypeScript implementation; it should adopt the author
experience that fits a Rust/Wasm framework.

### 6.1 Collections

A collection is a normalized local set of typed rows:

```rust
let posts = sync.collection::<PostSyncView>("posts_for_tenant");
```

Collections may be populated by:

- sync streams from this RFC,
- ordinary server-function/query fetches,
- local-only browser state,
- direct framework writes from live events or CDC adapters.

The collection keeps two stores:

- synced data: the last server-confirmed view,
- optimistic overlay: local mutations waiting for server confirmation.

Live queries read the merged view. Persistence and replay operate on the
synced store plus the pending mutation log, not on arbitrary component
state.

### 6.2 Live queries

Components should bind to local live queries:

```rust
let published = sync.live_query(|q| {
    q.from(posts)
        .filter(|post| post.status == PostStatus::Published)
        .order_by(|post| post.created_at.desc())
        .limit(50)
});
```

Live queries should support filtering, projection, ordering, limits, and
joins across local collections. The first implementation may recompute
affected queries conservatively. Later implementations can add
incremental query planning. The public contract is reactivity and
correctness, not a specific query-engine algorithm.

Every live query result row carries read-only sync metadata:

```rust
pub struct SyncRowMeta {
    pub synced: bool,
    pub origin: ChangeOrigin,
    pub key: serde_json::Value,
    pub collection: &'static str,
    pub version: Option<RowVersion>,
}
```

This metadata is queryable but is never persisted back to application
storage.

### 6.3 Query-driven sync

For large datasets, the observed frontend query should be able to drive
which server stream subset is loaded:

```rust
let products = sync.collection::<ProductView>("products");

let results = sync.live_query(|q| {
    q.from(products)
        .filter(|p| p.category == selected_category.get())
        .filter(|p| p.price < max_price.get())
        .limit(100)
});
```

The client does not send arbitrary executable filters. The framework maps
supported query predicates onto registered stream parameters. Unsupported
predicates run locally after the authorized stream subset is loaded.

This gives the desired "query the local store from components" workflow
without creating a raw database query API in the browser.

### 6.4 Optimistic mutations

Collections expose local insert, update, delete, and transaction APIs:

```rust
posts.update(post_id, |draft| {
    draft.title = new_title.clone();
});

sync.transaction(|tx| {
    tx.collection(posts).insert(new_post);
    tx.collection(activity).insert(new_activity);
});
```

Mutation lifecycle:

1. apply the mutation to the optimistic overlay,
2. render live queries from the merged view immediately,
3. send a `push` request with a stable mutation id,
4. wait for server acceptance and a sync cursor that includes the write,
5. replace optimistic state with the confirmed server row,
6. rollback or surface conflict if the server rejects it.

Multiple mutations in one transaction should be sent as one logical
operation. Mutations against the same key may be coalesced only when the
result preserves user intent and the server mutation contract allows it.

### 6.5 Direct server writes into collections

Server-originated changes from `pull`, live invalidation refreshes, or
CDC adapters must write directly into the synced store. They must not
create optimistic mutations.

Internal collection operations:

```rust
collection.apply_server_upsert(row, version, cursor);
collection.apply_server_delete(key, version, cursor);
collection.apply_server_reset(snapshot, cursor);
```

These operations update live queries immediately and reconcile any
pending optimistic overlay for the same key.

### 6.6 Local store adapters

The frontend store starts with a Pocopine-owned `SyncLocalStore`
contract and a memory implementation for tests:

```rust
pub trait SyncLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>>;
    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()>;
    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot>;
    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()>;
    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()>;
    fn enqueue_mutation(
        &self,
        stream: &SyncStreamName,
        mutation: ClientMutation<serde_json::Value>,
    ) -> SyncLocalFuture<'_, ()>;
    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()>;
    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>>;
}
```

The memory adapter is valid for tests and ephemeral views. The durable
browser implementation should be SQLite-first through SQLite WASM and
OPFS, behind a worker boundary. IndexedDB may still be useful as a
fallback or thin storage substrate, but it is not the primary query
engine for the TanStack DB-like direction.

SQLx is a separate host/server adapter. It should not sit underneath the
browser local store, because browser SQLite uses the SQLite WASM/OPFS
runtime shape rather than SQLx's native SQLite driver.

### 6.7 Framework integration

The framework should provide hooks/helpers rather than requiring
components to manually wire streams:

```rust
let posts = use_sync_collection::<PostSyncView>("posts_for_tenant");
let visible = use_live_query!(posts.where(|p| p.status == Published));
let save = use_sync_action!(posts.update);
```

The exact macro names can change during implementation. The RFC-level
contract is that collection loading, live query subscription, optimistic
mutation, error/conflict state, and pending/synced metadata are first
class frontend primitives.

## 7. Protocol Endpoints

### 7.1 Open

```text
POST /__pocopine/sync/v1/open
```

Request:

```json
{
  "streams": ["posts_for_tenant"],
  "client_id": "device_abc",
  "known_cursors": {}
}
```

Response:

```json
{
  "session_id": "sync_sess_123",
  "streams": [
    {
      "name": "posts_for_tenant",
      "snapshot_url": "/__pocopine/sync/v1/snapshot/snap_123",
      "cursor": "sync:v1:cursor_123"
    }
  ]
}
```

### 7.2 Pull

```text
POST /__pocopine/sync/v1/pull
```

The client sends stream cursors. The server returns ordered changes and a
new cursor. Pull responses may be empty.

### 7.3 Push

```text
POST /__pocopine/sync/v1/push
```

The client sends mutations:

```json
{
  "protocol": "pocopine.sync.v1",
  "stream": "posts_for_tenant",
  "mutations": [
    {
      "id": "device_abc:42",
      "key": "post_123",
      "base_version": "row:v7",
      "op": "upsert",
      "payload": {"title": "New title"}
    }
  ]
}
```

Mutation ids are idempotency keys. Replayed pushes must not apply twice.
The server returns accepted, rejected, or conflict states per mutation:

```json
{
  "protocol": "pocopine.sync.v1",
  "stream": "posts_for_tenant",
  "accepted": ["device_abc:42"],
  "rejected": [],
  "rows": [
    {
      "key": "post_123",
      "version": "row:v8",
      "value": {"title": "New title"},
      "pending": false,
      "conflict": false
    }
  ],
  "conflicts": [],
  "cursor": "sync:v1:cursor_124"
}
```

`rejected` is for validation, authorization, or business-rule failures
where retrying the same mutation as-is cannot succeed. `conflicts` is for
stale `base_version` or concurrent-write cases where the server can return
the current row and let the UI choose how to proceed.

## 8. Conflict Policy

The default policy is server-authoritative:

- if the base version still matches, apply the mutation in a transaction;
- if the base version is stale, reject with `conflict`;
- the client pulls current data and lets application UI decide what to do.

Applications may register custom resolvers:

```rust
resolve_conflict: |ctx, local, remote| async move {
    ConflictDecision::Apply(merge_post(local, remote)?)
}
```

The resolver runs on the server and must be explicit. Pocopine should not
silently merge arbitrary application records.

## 9. Change Sources

Phase A can use server functions and explicit application publication.
Later phases can add CDC adapters:

```rust
pub trait ChangeSource {
    async fn snapshot(&self, stream: StreamRequest) -> SyncResult<Snapshot>;
    async fn changes_since(&self, cursor: SyncCursor) -> SyncResult<ChangeBatch>;
}
```

Postgres logical replication and Supabase ETL are candidate sources for
Postgres. SQLite, MySQL, and application-event sources can be added
behind the same trait.

### 9.1 Future SQLx adapter

SQLx is a good opt-in adapter for teams that want compile-time checked
SQL. It should not be the core sync abstraction and should not be pulled
into apps that use another database layer.

The future crate should be separate:

```toml
[dependencies]
pocopine-sync = "..."
pocopine-sync-sqlx = { version = "...", features = ["postgres"] }
sqlx = { version = "0.8", features = [
  "runtime-tokio",
  "tls-rustls-ring-webpki",
  "postgres",
  "macros",
] }
```

Inline checked SQL should be the default authoring path for small streams:

```rust
pocopine::sync_stream! {
    posts_for_tenant {
        collection: posts;
        key: PostId;
        schema_version: 1;

        source: sqlx::postgres {
            snapshot: |pool, tenant_id| async move {
                sqlx::query_as!(
                    PostSyncView,
                    r#"
                    select id, title, description, updated_at
                    from posts
                    where tenant_id = $1
                    order by updated_at asc
                    "#,
                    tenant_id
                )
                .fetch_all(pool)
                .await
            };
        };

        filter: |ctx| ctx.tenant_id();
    }
}
```

File-backed SQL remains available for long snapshot/change queries:

```rust
sqlx::query_file_as!(
    PostSyncView,
    "sql/sync/posts_for_tenant_snapshot.sql",
    tenant_id
)
```

The adapter contract:

- SQL remains normal SQL, not a Pocopine ORM DSL.
- Inline `query!`/`query_as!` is preferred for simple streams.
- `query_file!`/`query_file_as!` is supported for large or shared SQL.
- Compile-time checking requires `DATABASE_URL` at build time or checked
  in SQLx offline metadata under `.sqlx`.
- CI for SQLx-enabled apps should run `cargo sqlx prepare --check`.
- Frontend query predicates must map to declared stream parameters. They
  must not produce unchecked SQL string concatenation.

SQLx checks query structure and database/Rust types for supported macros. It
does not prove tenant isolation, authorization, conflict correctness, or
delete privacy. Pocopine still owns those safety checks through stream
guards, server-side mutation policy, cursor validation, and tombstone
rules.

## 10. Interaction With `pocopine-live`

Sync does not need to keep a long-running bidirectional socket open for
the first version. The client can use `pocopine-live` as a wake-up path:

1. open the sync stream and load the snapshot,
2. listen for `stream.invalidated`,
3. call `pull` with the current cursor,
4. apply returned changes to the local store.

If the live stream drops, the next pull is still authoritative.

## 11. Security Model

- Every stream must be explicitly registered as public or guarded.
- Stream filters run on the server.
- Clients cannot invent new filters.
- Pushes execute through typed server mutations, not direct table writes.
- Tombstones reveal only keys unless explicitly configured.
- Cursor tokens must not grant access by themselves; access is checked on
  every open, pull, and push.
- `SyncStreamSource` receives `RequestContext` after stream-level access
  passes so the source can apply user/tenant filters before producing
  rows or accepting mutations.
- Frontend query predicates may narrow an authorized stream but must not
  expand it beyond the server-registered stream guard.
- SQLx compile-time checks, when enabled, are query/type checks only.
  They are not authorization proofs.

## 12. Observability

The sync crate must emit tracing events through the framework
observability model:

- `pocopine.trace` for session lifecycle and cursor progress.
- `pocopine.log` for gaps, auth denials, and adapter failures.
- `pocopine.metric` for snapshot bytes, pull latency, mutation retries,
  conflicts, and cursor-gap counts.

Mutation payloads and row payloads are not logged.

## 13. Phases

### Phase A - Manual source, HTTP protocol

- Add sync stream registration and guarded open/pull/push endpoints.
- Use application-published changes as the source.
- Add memory frontend collection store.
- Add basic live queries with conservative recomputation.

### Phase B - Live wake-up

- Emit `stream.invalidated` through RFC 071.
- Add browser helper that pulls on invalidation.
- Apply server changes directly to synced collections.

### Phase C - CDC adapters

- Add Postgres adapter using logical replication or Supabase ETL.
- Map database changes into stream changes.
- Add tests for delete privacy and cursor gaps.

### Phase D - Local store helpers

- Add `SyncLocalStore` and a memory implementation.
- Add stable device identity and durable mutation id generation.
- Support optimistic writes with mutation id replay.
- Add transaction state and conflict metadata exposed to components.

### Phase D2 - SQLite local store

- Add `pocopine-sync-sqlite`.
- Use SQLite WASM + OPFS behind a worker boundary in browsers.
- Persist stream cursors, rows, and pending mutations transactionally.
- Hydrate collections from local SQLite before network pull.

### Phase E - Advanced conflict handling

- Add custom resolvers and richer conflict UI hooks.
- Keep CRDT document sync in RFC 073.

### Phase F - Query-driven sync

- Map supported frontend predicates to stream parameters.
- Add request coalescing and subset expansion.
- Keep unsupported predicates as local-only filters.

### Phase G - Optional SQLx integration

- Add `pocopine-sync-sqlx` behind database features.
- Support inline `query!`/`query_as!` examples as the default docs path.
- Support `query_file!`/`query_file_as!` for large sync SQL.
- Document `DATABASE_URL`, `.sqlx`, and `cargo sqlx prepare --check`.
- Add compile-time checked snapshot and change-source examples for
  Postgres first.

## 14. Research References

- Local-first software paper: https://www.inkandswitch.com/essay/local-first/
- Automerge sync protocol: https://automerge.org/automerge/automerge/sync/index.html
- CouchDB replication protocol: https://docs.couchdb.org/en/stable/replication/protocol.html
- ElectricSQL sync engine and streams: https://electric-sql.com/product/sync
- PostgreSQL logical replication: https://www.postgresql.org/docs/current/logical-replication.html
- Supabase ETL: https://supabase.github.io/etl/
- TanStack DB overview: https://tanstack.com/db/latest/docs/overview
- TanStack DB live queries: https://tanstack.com/db/latest/docs/guides/live-queries
- TanStack DB mutations: https://tanstack.com/db/latest/docs/guides/mutations
- SQLite WASM persistence and OPFS: https://sqlite.org/wasm/doc/trunk/persistence.md
- SQLx repository and README: https://github.com/launchbadge/sqlx
- SQLx `query!` macro and offline mode: https://docs.rs/sqlx/latest/sqlx/macro.query.html
