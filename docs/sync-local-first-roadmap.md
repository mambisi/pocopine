# Sync Local-First Roadmap

Pocopine sync should be a database-agnostic local-first sync engine, not
a sync database product.

The framework should own the protocol, mutation lifecycle, local
durability, optimistic state, rebase, conflict model, live wake-up
integration, and generated resource ergonomics. Apps and adapter crates
should own the actual database queries, schema, authorization filters, and
backend-specific change tracking.

## Current Foundation

Pocopine already has the pieces needed to build on:

- `pocopine-sync` owns `/open`, `/pull`, `/push`, stream registration,
  cursor types, `CollectionState<T>`, mutation ids, pending replay, and
  the `SyncLocalStore` trait.
- `CollectionState<T>` keeps canonical server rows separate from the
  rendered merged view, then rebases pending local mutations over
  canonical rows after hydration, pulls, push outcomes, and replay.
- Pending local mutations can persist an optional local-only optimistic row
  alongside the wire mutation, so non-row CRUD payloads can still rebuild
  their rendered overlay after reload.
- `pocopine-live` wakes clients after server-side changes. It remains a
  wake-up channel, not a row transport.
- `pocopine-sync-sqlite` exists. It provides a durable local store with a
  shared schema, native SQLite implementation, and browser SQLite WASM +
  OPFS implementation through the same `SyncLocalStore` API.
- `pocopine-sync-crud` defines the first resource contract:
  `ResourceId`, CRUD mutation payloads, write policies, transaction
  binding, `CrudSource`, bounded snapshot pulls, exact mutation replay
  dedupe, a typed `LocalResourceView` over rebased sync state, generated
  resource methods, and the first conservative conflict helpers.

That means we are past "can the browser persist sync state?" The next
work is making persisted sync state behave like a complete local-first
resource model.

## What Is Still Missing

The first two stepping stones below are now implemented enough for the
framework contract. They stay in this section because follow-up work still
needs richer subscriptions, examples, and broader browser coverage.

### 1. Local View Rebase

The client needs a deterministic local view pipeline:

```text
hydrate canonical rows from local store
  -> hydrate pending mutations
  -> apply canonical server pull/change
  -> replay pending mutations over canonical rows
  -> render the rebased view
```

The core `CollectionState<T>` rebase slice is now in place: server pulls
update canonical rows, pending overlays replay in deterministic queue
order, rejected writes roll back to the latest canonical rows, and
row-shaped pending upserts can be reconstructed from local-store
hydration before replay.

The first typed view slice is now in place: `LocalResourceView<Id, Row>`
turns `CollectionState<Row>` into typed visible rows, canonical
`base_version` values, hidden pending deletes, pending/conflict flags, and
collection metadata. Generated CRUD methods now build on that view
instead of exposing protocol structs directly. They can create, save,
remove, queue offline, require online confirmation, use canonical
`base_version` defaults, and surface typed accepted/rejected/conflict
outcomes.

Deliverables:

- a row-level pending overlay model,
- deterministic replay order for queued mutations,
- rollback helpers for rejected mutations,
- conflict marking that keeps user-visible data available,
- tests for pull-while-push races and reload-with-pending-mutations.

### 2. Generated CRUD Resource API

Users should not hand-build sync envelopes for normal CRUD.

The macro layer now generates simple methods:

```rust
customers.create(id, draft).await?;
customers.save(id, draft).await?;
customers.remove(id).await?;
```

Defaults:

- queue offline,
- reserve a durable mutation id,
- use the latest canonical row version as `base_version`,
- apply an explicit optimistic row when supplied,
- push in the background when the browser runtime is online,
- rebase after canonical pulls,
- surface accepted/rejected/conflict states through typed helpers.

Advanced call sites use explicit escape hatches:

```rust
customers
    .save_with_options(
        id,
        draft,
        customers::SaveOptions::new()
            .base_version(version)
            .require_online(),
    )
    .await?;
```

The generated API should hide protocol plumbing, not database access.

### 3. Atomic Server Writes And Idempotency

Production server writes must be atomic:

```text
begin transaction
  -> authorize and validate
  -> reserve accepted mutation id
  -> check base_version and write row
  -> commit accepted writes
  -> rollback conflicts, rejections, and errors
publish live wake-up after commit
```

The first transaction-backed server slice is now in place. Resources can
opt into `.transactional(runner, log)`, where `CrudTransactionRunner`
owns begin/commit/rollback, `TransactionalCrudSource<Tx>` applies the
CRUD write with the active transaction handle, and
`TransactionalCrudMutationLog<Tx, Row>` reserves and reads accepted
mutation ids with the same handle.

The older `.mutation_log(...)` terminator remains available for simple
adapters and tests, but it cannot force the source write and
accepted-mutation insert to share one transaction. Production resources
should use the transaction-backed path and enforce a unique accepted-log
key over the same authorization scope as the source query. The reservation
insert is the retry/concurrency boundary; conflicts and rejections roll
back that reservation.

What is still left is backend-specific packaging, not the core contract:

- richer SQLx source examples for full CRUD resources,
- integration tests for backend-specific crash/retry boundaries where possible,
- more docs for app-level idempotency keys in payments,
  inventory, uniqueness-sensitive, or other side-effecting domains.

The first SQLx packaging slice now exists in `pocopine-sync-sqlx`. It
ships a generic SQLx transaction runner, backend-feature constructors for
Postgres/MySQL/SQLite, and a durable accepted-mutation log helper with
SQLite-backed tests. Source SQL remains app-owned and backend-specific.

### 4. Backend-Agnostic Change Sources

Pocopine should not assume one database changefeed.

The core needs traits for:

- snapshot reads,
- incremental reads after a cursor,
- gap detection and resnapshot,
- server-side row authorization,
- write application with atomic base-version checks.

Different backends can implement those contracts differently:

- app-owned `CrudSource` for direct CRUD resources,
- SQLx helper adapters for compile-time checked server queries,
- SQLite trigger/outbox adapters for local/native apps,
- Postgres logical replication or Electric-style shape adapters later,
- custom API-backed streams for non-SQL sources.

The core protocol should stay cursor-based and opaque. Adapter crates can
decide whether a cursor is a sequence number, timestamp, LSN, changelog
id, or backend token.

### 5. Conflict Resolution Helpers

The protocol already separates:

- `accepted`: server applied the mutation,
- `rejected`: mutation is invalid or not allowed,
- `conflicts`: mutation is valid but based on stale server state.

The author-facing helpers on generated resources and on
`CrudClientResource`:

```rust
customers.use_server(id).await?;
customers.retry_local(id, draft).await?;
customers.merge_with(id, merged_draft).await?;
customers.discard_local(id).await?;
```

`use_server` clears the local conflict marker after the user accepts the
known server row. It does not remove unrelated pending mutations for that
row. `retry_local` and `merge_with` queue a new save against the latest
canonical `base_version`; the conflict marker remains visible until the
server accepts the retry and returns a new canonical row.

`discard_local` is the inverse of `use_server` for pending edits: it
durably purges every queued mutation targeting one row, clears the
conflict marker, and rebases the rendered view from canonical. The
durable purge runs before the in-memory rebase so a mid-flight crash
cannot leave the rendered view ahead of disk. Unkeyed pending mutations
and mutations for other rows are left untouched.

The default should be conservative: never silently overwrite newer server
data when a `base_version` was supplied. Automatic CRDT merging belongs in
`pocopine-collab` / Yrs-backed document fields, not in the default CRUD
row model.

### 6. Local Query Layer

SQLite storage is not the same thing as a local-first application query
layer.

The framework needs a typed client-side view layer that can:

- subscribe components to local resource rows,
- expose loading/pending/conflict state without raw protocol structs,
- efficiently update only affected rows,
- read from the local store before the network responds,
- work with the generated CRUD methods.

This can borrow ideas from TanStack DB, but Pocopine should keep the API
resource-oriented and framework-native.

The first implementation step adds a scoped generated resource observer,
not a database query engine. `Resource::observe_view(...)` watches the
Pocopine scope that owns the resource collection and emits a typed
`LocalResourceViewState<Id, Row>` whenever the view changes. It gives
component authors a reactive read path while preserving the existing
`CollectionState` boundary as an implementation detail.

### 7. Auth And Multi-Tenant Boundaries

Sync does not make local data trusted.

The per-stream guard registered via
`pocopine_sync::SyncServerBuilder::guarded_stream` runs on every `/open`,
`/pull`, and `/push`. The app's `CrudSource` / `SyncStreamSource` filters
run on `/pull` and `/push` — `/open` only resolves the stream and runs
the registered guard. Sources that need to surface a per-request
authorization failure (for example a missing tenant header) return
`SyncError::unauthorized(msg)`; the server maps that to
`ServerError::Unauthorized` so app errors and framework guard errors
share one wire shape. See the auth tests in
`crates/pocopine-sync/src/server.rs`. Mutation logs must be
scoped to the same authorization domain as the source query, for example
`(tenant_id, mutation_id)`, not only `mutation_id`. Both
`SqlxCrudMutationLog::new(scope_fn)` and
`MemoryCrudMutationLog::with_scope_fn(scope_fn)` derive the scope from
the per-request context, so the source query and the mutation log share
one tenant boundary without app-side glue. The end-to-end example is in
`crates/pocopine-sync-crud/tests/tenant_boundary.rs`.

The local store is not an authorization boundary. It may legitimately
cache data that was once visible to a previous user, tenant, or auth
scope. Apps that switch identities call
`SyncClient::sign_out` from the auth-aware code path — it durably wipes
every stream snapshot, cached row, pending mutation, and conflict marker
via `SyncLocalStore::clear_all_streams`, then broadcasts a sign-out
event so sibling tabs in the same browser origin can drop their
in-memory `CollectionState` and re-hydrate from the (now empty) local
store. Subscribe to the broadcast with `SyncClient::watch_sign_outs`.

The persisted `SyncLocalIdentity` (device id + mutation counter) is
intentionally **preserved** across sign-out. Rotating the device id
would silently reset the mutation counter and could collide with
accepted-log entries the server still retains — the same `(client_id,
counter)` shape used by Yjs, Automerge, Loro, Jazz, Replicache, and
Linear keeps the identity stable per browser-origin-bucket and lets
future P2P modes coordinate without changing the id format.

Deliverables (status):

- documented user/tenant cache partitioning — this section + roadmap §3
  on transactional source/log scoping,
- helpers for clearing sync state on sign-out or tenant switch —
  `SyncClient::sign_out`, `SyncLocalStore::clear_all_streams`,
  `SyncClient::watch_sign_outs`,
- tests that a guarded stream cannot be pulled or pushed anonymously —
  `guarded_stream_denies_anonymous_open_and_pull`,
  `guarded_stream_denies_push_before_default_unsupported`,
  `role_guard_checks_authenticated_principal_roles` in
  `pocopine-sync/src/server.rs`,
- end-to-end example where source queries and mutation logs use the
  same tenant boundary — `tests/tenant_boundary.rs` in
  `pocopine-sync-crud`.

### 8. CI And Regression Tests

The test matrix should prove the engine behavior without requiring every
database backend:

- pure protocol/state tests in `pocopine-sync`,
- memory local-store tests for deterministic semantics,
- native SQLite tests in `pocopine-sync-sqlite`,
- wasm SQLite/OPFS browser smoke tests,
- CRUD adapter route tests,
- browser tests for optimistic push, online-only push, pending replay,
  rejection rollback, conflict handling, and rebase.

Backend-specific adapters can add their own integration tests later. The
core sync contract should not need Redis, Postgres, or SQLx metadata to
stay green.

## Why Not A Sync Database Now

Building a sync database would mean owning schema design, query planning,
backend-specific replication, migrations, conflict policies, and
production operational semantics. That is too much surface for the
framework right now and would fight Pocopine's goal of letting apps own
their database code.

The better path is:

```text
Pocopine owns local-first resource semantics.
Adapters own database-specific mechanics.
Apps own business rules and authorization.
```

This keeps the core database-agnostic while still making the common path
simple and hard to misuse.

## Recommended Implementation Order

1. ~~Extend typed local resource/query views beyond owned snapshots into
   framework-native subscriptions.~~ Done — `Resource::observe_view`.
2. ~~Add transaction-backed server helper paths for source write + mutation
   log insert.~~ Done — `TransactionalCrudSource` + `CrudTransactionRunner`.
3. Add SQLx helper adapters for server CRUD sources, without becoming an
   ORM. Transaction-runner and accepted-mutation-log helpers are in
   place; richer source examples remain.
4. ~~Add a durable row-scoped pending-mutation purge operation if the
   author-facing API needs true "discard local edits" semantics.~~ Done —
   `SyncLocalStore::purge_pending_for_row` + `CrudClientResource::discard_local`.
5. ~~Document auth/cache partitioning and sign-out reset behavior.~~
   Done — `SyncLocalStore::clear_all_streams`,
   `SyncClient::sign_out` / `watch_sign_outs`, and the §7 deliverables
   above.
6. Add broader browser CI around SQLite local-first flows.
7. Only then consider database-specific changefeed adapters.

This order uses the SQLite sync store we already have while preserving the
database-agnostic engine boundary.
