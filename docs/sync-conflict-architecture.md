# Sync And Conflict Resolution Architecture

This document is a deep dive for future Pocopine sync authors. It explains
how the current sync layers fit together, which parts own conflict
semantics, and what invariants new CRUD/resource helpers must preserve.

Pocopine sync is a database-agnostic local-first sync engine. It is not a
sync database. Framework code owns protocol shape, local durability,
optimistic overlay, rebase, conflict metadata, live wake-up integration,
and typed resource ergonomics. Apps and adapter crates own database
schema, source queries, authorization filters, and backend-specific change
tracking.

## Layers

The stack is intentionally split:

```text
application resource methods
  -> pocopine-sync-crud typed resource contract
  -> pocopine-sync protocol/client/server state machine
  -> SyncLocalStore implementation
  -> app database and optional backend adapters
```

`pocopine-sync` owns the protocol:

- `/open` validates and discovers streams,
- `/pull` returns snapshot or incremental canonical rows,
- `/push` submits client mutations and returns accepted, rejected, and
  conflict outcomes,
- `CollectionState<T>` owns the client-side state machine,
- `SyncLocalStore` owns local durability for identities, cached rows,
  cursors, pending mutations, and push outcomes.

`pocopine-sync-crud` owns resource typing:

- `ResourceId` maps app ids to sync row keys,
- `CrudMutationPayload` describes create/save/remove payloads,
- `CrudSource` adapts an app-owned data source to sync routes,
- `CrudMutationLog` records accepted mutation ids for replay dedupe,
- `LocalResourceView` exposes typed local rows and pending mutations to
  generated clients and UI code.

`pocopine-live` remains a wake-up channel. It should tell clients "pull
this stream again"; it should not carry row data or bypass sync conflict
rules.

## Protocol Objects

Rows are canonical transport units:

```text
SyncRow<T> {
  key,
  version,
  value,
  pending,
  conflict,
}
```

Mutations are client-authored write intents:

```text
ClientMutation<T> {
  id,
  key,
  op,
  base_version,
  payload,
}
```

Important meanings:

- `id` is the replay/idempotency key.
- `key` scopes the mutation to one row when possible.
- `op` is `upsert`, `delete`, or `reset`.
- `base_version` is the last canonical server row version the client
  believes it edited.
- `payload` is the author-facing write data. For CRUD this is not
  necessarily the same shape as the rendered row.

The local store persists `LocalPendingMutation`, not only
`ClientMutation<Value>`. The extra `optimistic_row` is local-only and is
never sent to `/push`. It exists so a browser reload can reconstruct the
rendered optimistic row even when the mutation payload is an envelope such
as `{ op: "save", payload: { id, draft } }`.

## Canonical Rows Versus Rendered Rows

The key local-first invariant is:

```text
rendered rows = canonical rows + pending local overlay
```

`CollectionState<T>` keeps canonical server rows separate from `rows`,
the rendered view. Server pulls update canonical rows. Pending mutations
then replay over that canonical set in deterministic queue order.

That separation matters because a live pull may arrive while local writes
are pending:

```text
server v1
  -> user edits locally
  -> UI shows local draft
  -> pull receives server v2
  -> UI still shows local draft
  -> base_version now points at server v2
```

The user does not see a flicker back to server data, but the next push or
retry uses the newest known canonical version. If the local mutation is
rejected, the UI rolls back to server v2 instead of the stale server v1.

This is why `row_version` and `base_version` are different in the typed
view:

- `row_version` belongs to the rendered row. A purely optimistic row may
  not have one.
- `base_version` belongs to the current canonical row and should be used
  for generated save/remove conflict detection.

## Open And Hydration Flow

Opening a stream should follow this order:

```text
load local identity
  -> hydrate cached canonical rows and cursor
  -> hydrate pending mutations and optimistic rows
  -> render rebased local view
  -> replay pending mutations when possible
  -> pull from server
  -> persist canonical rows and push outcomes
  -> rebase local view again
```

Hydration is allowed to be stale. The contract is immediate local
availability followed by reconciliation. `stale = true` means the UI is
rendering from local storage before a successful network pull has
confirmed freshness.

If pending replay fails because the app is offline, the pending overlay
must remain visible and durable. A replay failure is not permission to
drop local edits.

## Push Outcomes

The server returns three categories:

```text
accepted  - the mutation was applied or deduped as an exact replay
rejected  - the mutation cannot succeed as sent
conflict  - the mutation was valid, but its base version lost a race
```

Rejected and conflicted are deliberately separate.

Use `rejected` for invalid payloads, unauthorized writes, mismatched row
keys, reused mutation ids with different contents, or domain validation
failures where retrying the same mutation cannot help.

Use `conflict` when the write was well-formed but the server row changed
since the client's `base_version`. The UI can then offer conflict
resolution choices instead of treating the write as a malformed request.

On accepted outcomes:

- remove matching pending mutations,
- apply returned canonical rows,
- clear pending/conflict flags for returned rows,
- remove canonical rows for accepted deletes,
- persist the push result in the local store.

On rejected outcomes:

- remove matching pending mutations,
- rebase rendered rows from canonical rows,
- increment rejection metadata,
- keep the first reason visible as `state.error`.

On conflict outcomes:

- remove the matching pending mutation,
- mark the server row conflicted when provided,
- if no server row is provided, mark the visible fallback row conflicted
  when possible,
- keep user-visible data available rather than clearing the screen.

## Typed Local Resource View

`LocalResourceView<Id, Row>` is the bridge from low-level sync state to a
resource-oriented author surface.

It converts `CollectionState<Row>` into:

- typed `LocalResourceRow<Id, Row>` entries,
- per-row `LocalResourceRowStatus`,
- rendered row version,
- canonical `base_version`,
- queued `LocalResourcePendingMutation<Id>` entries,
- loading/syncing/stale/error counters.

This lets generated CRUD APIs and components ask resource questions:

```rust
let view = local_resource_view::<String, Customer>(&customers_state)?;

if let Some(row) = view.get(&customer_id) {
    if row.is_conflict() {
        // show conflict UI for this resource row
    }
}

let base_version = view.base_version(&customer_id).cloned();
```

The view also exposes pending deletes that have no visible row. That
matters for UI affordances such as "undo delete", disabled save buttons,
or a compact pending-mutations tray.

Generated CRUD methods should use this view instead of directly poking at
`SyncRow` flags. That keeps protocol structs as an implementation detail
and gives future query/resource layers one place to improve.

## Conflict Resolution Shape

The first conflict contract is conservative: mark the conflict, keep data
visible, and make the state machine deterministic. Rich resolution APIs
can build on top of that.

The intended future author surface is:

```rust
conflict.use_server();
conflict.retry_local();
conflict.merge_with(draft);
conflict.discard_local();
```

Those helpers should map to normal sync operations:

- `use_server` clears the local pending/conflict marker and keeps the
  canonical server row.
- `discard_local` is equivalent for simple row edits, but can include
  app-specific audit or UI behavior.
- `retry_local` creates a new mutation with the latest canonical
  `base_version`.
- `merge_with` creates a new mutation whose payload is the user-approved
  merge result, also based on the latest canonical `base_version`.

Do not hide conflicts by silently overwriting local or server state. For
ordinary CRUD, explicit conflict UI is safer than automatic last-write
wins. CRDT-backed document fields belong in `pocopine-collab`, not in the
default CRUD conflict policy.

## Server Atomicity And Idempotency

Server-side CRUD writes must be atomic at the app database boundary:

```text
begin transaction
  -> authorize and validate
  -> check base_version
  -> write row
  -> record accepted mutation id
commit
publish live wake-up after commit
```

`CrudMutationLog` exists so a replayed mutation id does not duplicate a
write. Production logs must be scoped to the same authorization domain as
the source rows, for example `(tenant_id, mutation_id)`, not only
`mutation_id`.

A mutation id dedupes sync replay. It is not a universal business
idempotency key. Payments, external side effects, uniqueness-sensitive
workflows, and inventory reservations still need app-level idempotency
keys and domain-specific transaction rules.

## Authorization And Cache Boundaries

Every `/open`, `/pull`, and `/push` request must run stream guards and
source filters. `/open` is discovery and validation, not a session grant.
Skipping `/open` must not bypass access control.

The local store may retain data that was once visible to a user. Apps that
switch users, tenants, roles, or auth scopes need either:

- a partitioned local-store identity/database per auth scope, or
- an explicit sync cache reset on sign-out or tenant switch.

Do not rely on client-side filtering to protect cached data from a
previous authorization scope.

## Testing Invariants

Core sync tests should prove these invariants without requiring every
database backend:

- stale pull responses are ignored by generation token,
- local hydration renders before network pull,
- pending mutations replay in queue order,
- duplicate pending ids replace the earlier local pending record,
- pull while pending rebase keeps optimistic rows visible,
- rejected mutations roll back to the latest canonical row,
- conflicts keep a visible conflicted row when possible,
- accepted deletes remove canonical rows,
- local stores preserve optimistic rows and legacy pending-mutation wire
  shapes,
- CRUD sources reject mismatched row key, operation, or duplicate mutation
  content,
- auth guards run independently for open, pull, and push.

Backend-specific adapters should add integration tests for their own
database, transaction, migration, and changefeed behavior. The core
contract should stay green with protocol/state tests, memory-store tests,
native SQLite tests, and targeted browser/wasm checks.

