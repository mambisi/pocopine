# Sync Local Store Plan

This is the next sync phase after the guarded stream protocol and
mutation push flow. The goal is to make Pocopine sync local-first without
turning framework core into a database toolkit.

The stable contract is a Pocopine sync store abstraction. SQLite is the
first serious durable implementation. SQLx is a later host/server adapter,
not the foundation of browser-local sync.

## Decision

Build the local store in this order:

1. Add a `SyncLocalStore` contract to `pocopine-sync`.
2. Add a memory implementation for tests and examples.
3. Add `pocopine-sync-sqlite` for browser SQLite through SQLite WASM and
   OPFS, behind a worker boundary.
4. Add `pocopine-sync-sqlx` later for host/server `SyncStreamSource`
   adapters and optional native SQLite use.

Do not route browser-local sync through SQLx. SQLx uses native database
drivers and is excellent for server Rust, but browser SQLite uses the
SQLite WASM/OPFS runtime shape.

Current implementation status: the `SyncLocalStore` contract,
`SyncLocalIdentity`, `MutationIdGenerator`, and `MemoryLocalStore` are in
place. The client runtime does not yet hydrate from a store or replay
stored mutations automatically.

## Crate Boundaries

`pocopine-sync` owns the protocol and store-neutral client semantics:

- sync wire types,
- stream open/pull/push flow,
- `CollectionState<T>`,
- stable client/device/session identity types,
- mutation id generation,
- `SyncLocalStore` trait,
- memory local store used by tests.

`pocopine-sync-sqlite` owns durable local storage:

- browser SQLite WASM + OPFS worker integration,
- Pocopine internal tables,
- transactional snapshot/change/mutation queue operations,
- hydration from local rows before network pull,
- pending mutation replay after reload.

`pocopine-sync-sqlx` is a later host/server adapter:

- SQLx-backed `SyncStreamSource` implementations,
- compile-time checked snapshot and change queries,
- Postgres/MySQL/SQLite host database support,
- optional native-app local SQLite support if the same trait boundary fits.

`pocopine-live` remains a wake-up channel only. It does not move sync
rows and does not become the local database.

## Local Store Responsibilities

The local store is responsible for durable client-side sync state:

- load cached stream rows before the network responds,
- persist snapshots atomically,
- apply incremental changes atomically,
- persist the latest stream cursor,
- enqueue mutations before sending them,
- mark mutations accepted, rejected, or conflicted,
- replay pending mutations after reload or reconnect,
- store stable device/client identity.

The store is not trusted. Server guards, stream filters, and mutation
policies still run on every `/open`, `/pull`, and `/push`.

## Identity And Mutation Ids

The next phase should add stable client identity:

```text
SyncDeviceId
SyncSessionId
MutationId = "{device_id}:{counter}"
```

`SyncDeviceId` is generated once and persisted by the local store.
`SyncSessionId` is process/page-load scoped. The persisted local identity
also stores the next mutation counter. Stores must advance that counter
durably before exposing a mutation id that can be sent to the server.

This keeps mutation ids stable across reloads and avoids the current
example-only `post_local_{n}` pattern becoming a production habit.

## Store Contract Sketch

The exact Rust shape can change during implementation, but the contract
should cover these operations:

```rust
pub trait SyncLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>>;
    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()>;

    fn hydrate_stream(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, LocalStreamSnapshot>;

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

The important invariant is transactional behavior. If a snapshot or push
result partially applies, the client can render stale or impossible state.

## Browser Flow

Mounting a collection becomes local-first:

```text
component mounts
  -> hydrate rows from SyncLocalStore
  -> render cached rows immediately
  -> open stream
  -> pull snapshot or incremental changes
  -> persist server changes transactionally
  -> update CollectionState from the confirmed local view
```

Pushing a mutation becomes durable-before-network:

```text
user edits
  -> allocate stable mutation id
  -> enqueue mutation in SyncLocalStore
  -> apply optimistic state
  -> POST /push
  -> persist accepted/rejected/conflict result
  -> reconcile CollectionState
  -> live wake-up or pull refreshes canonical rows
```

Reconnecting becomes anti-entropy:

```text
network returns
  -> load pending mutations
  -> replay pushes by mutation id
  -> pull every open stream using stored cursor
  -> resnapshot if the server reports a gap
```

## Initial SQLite Schema

The first SQLite backend should store Pocopine metadata separately from
application data:

```sql
create table __pocopine_meta (
  key text primary key,
  value text not null
);

create table __pocopine_streams (
  stream text primary key,
  collection text not null,
  cursor text,
  schema_version integer not null,
  updated_at_ms integer not null
);

create table __pocopine_rows (
  stream text not null,
  row_key text not null,
  version text,
  payload text not null,
  pending integer not null default 0,
  conflict integer not null default 0,
  updated_at_ms integer not null,
  primary key (stream, row_key)
);

create table __pocopine_mutations (
  stream text not null,
  mutation_id text primary key,
  row_key text,
  base_version text,
  op text not null,
  payload text,
  status text not null,
  error text,
  created_at_ms integer not null,
  updated_at_ms integer not null
);
```

This schema stores framework sync state. Later typed local query APIs may
materialize app-specific tables or views, but the first phase should keep
the persisted representation simple and correct.

## Implementation Phases

### Phase 1 - Store Contract

Files:

- `crates/pocopine-sync/src/lib.rs`
- `crates/pocopine-sync/src/local_store.rs`
- `crates/pocopine-sync/src/protocol.rs`
- `crates/pocopine-sync/src/state.rs`
- `docs/sync.md`

Deliverables:

- `SyncLocalStore` trait,
- stable `SyncDeviceId`, `SyncSessionId`, and mutation id generator,
- store-neutral result types for snapshots, changes, and push outcomes,
- documentation of the durability model.

Tests:

- device id is generated once and reused,
- mutation ids are monotonic for a device,
- accepted push clears the queued mutation,
- rejected push records rollback state,
- conflict push records conflict metadata.

Commit boundary:

```text
Define sync local-store contract
```

### Phase 2 - Memory Local Store

Files:

- `crates/pocopine-sync/src/local_memory.rs`
- `crates/pocopine-sync/src/client.rs`
- `crates/pocopine-sync/src/state.rs`

Deliverables:

- `MemoryLocalStore`,
- hydration before network pull,
- pending mutation replay through the existing push path,
- client behavior still works without durable browser storage.

Tests:

- hydrate cached rows before the first pull,
- snapshot replaces local rows,
- incremental changes patch local rows,
- queued mutation survives a recreated collection state,
- pull gap clears old rows and applies the new snapshot.

Commit boundary:

```text
Add memory-backed sync local store
```

### Phase 3 - Docs And Example

Files:

- `docs/sync.md`
- `examples/sync/README.md`
- `examples/sync/src/lib.rs`

Deliverables:

- tutorial updated for local-first flow,
- example shows cached state and pending replay using the memory store,
- docs clearly say memory is not production storage.

Tests:

- existing `pocopine-sync` tests,
- existing `sync-example` tests,
- browser smoke test if the example UI changes.

Commit boundary:

```text
Document local-first sync store flow
```

### Phase 4 - SQLite Backend

Files:

- `crates/pocopine-sync-sqlite/Cargo.toml`
- `crates/pocopine-sync-sqlite/src/lib.rs`
- `crates/pocopine-sync-sqlite/src/worker.rs`
- `crates/pocopine-sync-sqlite/src/schema.rs`
- `docs/sync.md`

Deliverables:

- new extension crate,
- browser SQLite WASM + OPFS worker boundary,
- schema migration bootstrap,
- transactional local-store implementation,
- fallback/error path when persistent storage is unavailable.

Tests:

- wasm browser test for open database and schema bootstrap,
- mutation queue persists across client recreation,
- snapshot/change application is atomic,
- private/incognito or storage-denied path reports a clear error.

Commit boundary:

```text
Add SQLite-backed sync local store
```

### Phase 5 - SQLx Backend

Files:

- `crates/pocopine-sync-sqlx/Cargo.toml`
- `crates/pocopine-sync-sqlx/src/lib.rs`
- `docs/sync.md`

Deliverables:

- host/server-only adapter crate,
- SQLx-backed `SyncStreamSource` helpers,
- examples for inline `query!` / `query_as!`,
- guidance for `.sqlx` offline metadata and CI.

Tests:

- host tests for snapshot and incremental source helpers,
- SQLx offline metadata example or documented CI check,
- authorization still proven outside SQLx query typing.

Commit boundary:

```text
Add SQLx sync source adapter
```

## CI Strategy

The store contract and memory backend should run in ordinary workspace
tests. SQLite browser persistence needs a narrower CI shape:

- keep pure protocol/store-state tests in `cargo test -p pocopine-sync`,
- add wasm browser tests under `pocopine-sync-sqlite`,
- run SQLite OPFS tests only where the browser supports persistent OPFS,
- keep a memory fallback test so CI failures distinguish storage support
  from sync semantic regressions.

Do not require Redis, Postgres, or SQLx to test the local-store contract.
Database adapter tests belong to their adapter crates.

## Non-goals For The Next PR

- No SQLx adapter in the local-store PR.
- No CDC integration.
- No CRDT document merge; that remains `pocopine-collab`.
- No arbitrary browser SQL API exposed to app code.
- No `pocopine` core feature that re-exports optional sync backends.
