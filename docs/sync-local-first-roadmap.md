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
  binding, `CrudSource`, bounded snapshot pulls, and exact mutation
  replay dedupe.

That means we are past "can the browser persist sync state?" The next
work is making persisted sync state behave like a complete local-first
resource model.

## What Is Still Missing

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

The remaining work is generated resource ergonomics and typed resource
views. Generated CRUD methods need to produce the durable optimistic rows
automatically, and typed query/resource views still need to expose the
rebased state without forcing app code to inspect protocol structs
directly.

Deliverables:

- a row-level pending overlay model,
- deterministic replay order for queued mutations,
- rollback helpers for rejected mutations,
- conflict marking that keeps user-visible data available,
- tests for pull-while-push races and reload-with-pending-mutations.

### 2. Generated CRUD Resource API

Users should not hand-build sync envelopes for normal CRUD.

The macro layer should generate simple methods:

```rust
customers.create(id, draft)?;
customers.save(id, draft)?;
customers.remove(id)?;
```

Defaults:

- queue offline,
- reserve a durable mutation id,
- use the current local row version as `base_version`,
- apply an optimistic row when possible,
- push when online,
- rebase after canonical pulls,
- surface accepted/rejected/conflict states through typed helpers.

Advanced call sites use explicit escape hatches:

```rust
customers
    .save_options()
    .require_online()
    .base_version(version)
    .send(id, draft)?;
```

The generated API should hide protocol plumbing, not database access.

### 3. Atomic Server Writes And Idempotency

Production server writes must be atomic:

```text
begin transaction
  -> authorize and validate
  -> check base_version and write row
  -> record accepted mutation id
commit
publish live wake-up after commit
```

The current non-macro adapter exposes the needed pieces but cannot force
the source write and mutation-log insert to share one transaction. That is
acceptable for the contract slice, but not enough as the recommended
production path.

Deliverables:

- transaction-backed CRUD adapter helpers,
- durable mutation log examples,
- tests for crash/retry boundaries where possible,
- docs that make app-level idempotency keys explicit for payments,
  inventory, uniqueness-sensitive, or other side-effecting domains.

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

The framework still needs author-facing helpers:

```rust
conflict.use_server();
conflict.retry_local();
conflict.merge_with(draft);
conflict.discard_local();
```

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

### 7. Auth And Multi-Tenant Boundaries

Sync does not make local data trusted.

Server guards and source filters must run on every `/open`, `/pull`, and
`/push`. Mutation logs must be scoped to the same authorization domain as
the source query, for example `(tenant_id, mutation_id)`, not only
`mutation_id`.

The local store may cache data that was once visible to a user. Apps that
switch users, tenants, or auth scopes need a clear cache reset or
partition strategy.

Deliverables:

- documented user/tenant cache partitioning,
- helpers for clearing sync state on sign-out or tenant switch,
- tests that a guarded stream cannot be pulled or pushed anonymously,
- examples where source queries and mutation logs use the same tenant
  boundary.

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

1. Harden resource-level rebase on top of the core canonical/overlay
   state model.
2. Add typed local resource/query views over `SyncLocalStore`.
3. Generate CRUD client methods and options APIs.
4. Add transaction-backed server helper paths for source write + mutation
   log insert.
5. Add SQLx helper adapters for server CRUD sources, without becoming an
   ORM.
6. Document auth/cache partitioning and sign-out reset behavior.
7. Add broader browser CI around SQLite local-first flows.
8. Only then consider database-specific changefeed adapters.

This order uses the SQLite sync store we already have while preserving the
database-agnostic engine boundary.
