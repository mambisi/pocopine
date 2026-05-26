# Sync schema versioning

When you change a synced row's shape — add a required field, rename a
column, narrow a type — clients with **cached rows** or **queued
mutations** encoded against the old shape will silently corrupt their
views unless the server tells them their cache is stale.

Pocopine's sync engine ships a three-piece versioning system. Two of
the three are automatic; the third (migration) is opt-in for apps that
must preserve queued local edits across a bump.

## The contract

Every CRUD resource declares an integer `schema_version`. The server
advertises it on every `/open` response; the client compares against
its locally-cached value and reacts on mismatch.

```rust
#[pocopine_sync_crud::resource(name = "customers", schema_version = 2)]
#[pocopine_sync_crud::async_trait]
impl CrudSource for Customers { … }
```

Defaults to `1` when omitted. Bump the number whenever the row, draft,
or payload shape changes in a way that older cached data cannot
deserialize into. Versions start at `1`; `0` is rejected at the macro
level and at the wire level.

## What happens on a mismatch

### Default: drop the client's cache + queue

When the server advertises `schema_version = N` and the client's
cached value is `M ≠ N`:

1. The client durably wipes the stream's rows + pending mutations via
   `SyncLocalStore::clear_stream`.
2. The in-memory `CollectionState` is reset (rows, pending overlay,
   cursor all cleared) via `CollectionState::reset_for_schema_invalidation`.
3. The advertised version is stamped onto the local store.
4. The post-open pull rebuilds canonical state under the new shape.

If `clear_stream` fails, the client surfaces the error via
`apply_error` and stops rather than proceeding to the pull — this keeps
the cached version at the OLD value so the next open retries.

If `cached_schema_version == None` (fresh install or a stream the store
has never observed), the advertised version is adopted silently — no
wipe is needed.

### Default for pushed mutations: per-mutation reject

When a `/push` carries `request.schema_version < source.schema_version()`,
the server invokes `SyncStreamSource::migrate_payload` on each
mutation. The trait default returns
`SyncError::SchemaMigration { stream, from, to }`. The framework adds
the failing mutation to the response's `rejected` array with the
error's `reason`; the rest of the batch continues. Clients see a
typed `Rejected` outcome on those mutations.

This matches Replicache and Linear: drop queued local edits when the
schema bumps; the user's view is rebuilt from canonical.

## Escape hatch: register a migrator

For apps where dropping queued mutations is unacceptable (payments,
inventory writes, anything with side effects), register a
`migrate_with` function:

```rust
fn migrate_v1_to_v2(from: u32, to: u32, mut value: Value) -> SyncResult<Value> {
    // The framework only invokes this when `from < to`, so we only
    // need to handle the strict-forward direction. Bail on any
    // unexpected version pair so a future v3 doesn't silently
    // double-migrate through this fn.
    if from != 1 || to != 2 {
        return Err(SyncError::schema_migration("customers", from, to));
    }
    // CrudMutationPayload's wire shape is
    //   { "op": "create"|"save"|"remove",
    //     "payload": { "id": …, "draft": <draft> } }
    if let Some(payload) = value.get_mut("payload") {
        if let Some(draft) = payload.get_mut("draft") {
            if let Some(obj) = draft.as_object_mut() {
                obj.entry("email".to_string())
                    .or_insert_with(|| Value::String(String::new()));
            }
        }
    }
    Ok(value)
}

#[pocopine_sync_crud::resource(
    name = "customers",
    schema_version = 2,
    migrate_with = migrate_v1_to_v2,
)]
#[pocopine_sync_crud::async_trait]
impl CrudSource for Customers { … }
```

The framework calls your fn once per mutation when the request's
`schema_version` differs from the resource's current version. On `Ok`,
the migrated payload replaces the original and continues into
`source.push`. On `Err`, the mutation lands in `rejected` with the
error's `reason`.

The wire payload your migrator receives is a `serde_json::Value`
representing the entire `CrudMutationPayload<Id, Draft>` enum. The
shape is documented above; mutate it in place and return.

## When to choose which

| Situation | Choice |
|---|---|
| Most apps; queued edits are best-effort | **Default drop** (no `migrate_with`) |
| Payments, inventory, anything side-effecting | **`migrate_with`** with explicit field defaults |
| Field rename only, no semantic change | **`migrate_with`** that just renames |
| Backward-incompatible deletion of a field | **`migrate_with` that returns `Err`** for those drafts and use the drop-default for the rest. The errors lose the drafts; that's correct. |

## Out-of-tree sources

If you implement `SyncStreamSource` directly (not through
`CrudResource`), you become responsible for two contract points:

1. **Use the migration sidecar.** The framework's `push_handler`
   attaches the migration result to each `ClientMutation` as
   `migration_outcome: Option<MigrationOutcome<Value>>`. Call
   `mutation.take_processing_payload()` to get the payload to apply
   — it returns `Ok(value)` (migrated or original) or `Err(reason)`
   for a migrator-rejected mutation. The mutation's `payload` field
   stays the ORIGINAL wire value; use it for your idempotency log
   key. NEVER read `mutation.payload` for the actual write — that
   would silently bypass any registered migrator.

2. **Check the idempotency log BEFORE consuming the migration
   result.** A retry of a previously-accepted mutation should
   succeed even when the current migrator now rejects (or panics)
   on the same inputs. The accepted-log lookup uses
   `mutation.payload` (original); only mutations not in the log
   need their `take_processing_payload()` outcome inspected.

The CRUD `push_mutations` (and its transactional sibling) implement
both points; copy the pattern if you write a custom source.

## Migration cannot change the envelope

`migrate_payload` migrates the `serde_json::Value` payload only —
it cannot change `ClientMutation.key`, `op`, or `base_version`.
This matters when a schema bump renames row IDs (e.g. moving from
`"42"` to `"tenant:42"`):

* The migrator can update the embedded `id` inside the payload, but
  the outer `key` (set by the client at queue-time) stays at the v1
  form.
* CRUD validates `payload.id().to_row_key() == mutation.key` and
  rejects the mismatch with a typed `row key does not match payload
  id` reason.

The recommended approach for ID-shape changes: bump
`schema_version` and rely on the **default drop-the-cache** path.
Local rows are rebuilt from canonical via a fresh `/pull`; pending
mutations queued under the old ID shape are dropped. If you must
preserve queued mutations across an ID rename, you'd need a custom
`SyncStreamSource::push` that re-derives the key from the migrated
payload before delegating to your store.

## Migrator caveats

A registered `migrate_with` function runs with these constraints:

* **Synchronous and blocking.** The closure executes on the tokio
  runtime worker handling the push. Keep it fast — pure JSON
  manipulation. Any I/O blocks every other concurrent task on that
  worker thread until the migrator returns. A future iteration of
  the API may accept an async migrator; today, it does not.

* **Out-of-transaction (for `TransactionalCrudResource`).** The
  framework calls `migrate_payload` BEFORE the per-mutation
  transaction opens, so any side effects the migrator performs (e.g.
  writing to an audit table on the same database) are NOT rolled
  back if the subsequent transactional write fails. Keep migrators
  side-effect-free.

* **Panic-safe.** A panic inside your migrator is caught by
  `std::panic::catch_unwind`; the framework converts it into a
  per-mutation `SyncRejectedMutation` with a `migrate_with panicked`
  reason. The push request itself is not crashed — adjacent
  well-formed mutations continue through `source.push` normally.

* **Forward-only.** The migrator only fires when the client's wire
  `schema_version` is STRICTLY LESS than the server's
  `schema_version()`. A newer client pushing to an older server (a
  rolling-deploy edge case) is NOT routed through `migrate_payload`;
  the source receives the newer-shape payload and either accepts it
  (if forward-compatible) or rejects it with a typed deserialization
  error.

## Common pitfalls

- **Forgetting to bump `schema_version`** after a row/draft change.
  Older clients then push stale data and the server silently
  mis-deserializes. The macro guards one related case for you: a
  `migrate_with = ...` declaration without `schema_version >= 2`
  fails to compile, because such a migrator would be dead code
  (clients and servers both default to v1).

- **Migrator that loses required fields.** A migrator that drops a
  field the v2 source needs (e.g. removes `version` from a save
  payload) will succeed in `migrate_payload` and then fail in
  `source.save`. The mutation ends up in `rejected` either way, but
  the reason will read like a deserialization or backend error
  instead of a migration error.

- **Bumping without coordinating with the client app.** The server
  rejects stale client pushes, but it does NOT prevent a stale client
  from reading new-shape rows over `/pull`. If the row's `Deserialize`
  fails, the row is dropped silently. Plan a transition window where
  the server accepts both shapes (via `migrate_with`) until you're
  confident every client deployment has updated.

- **Non-deterministic migrators break idempotency.** If your migrator
  injects server-side timestamps, UUIDs, or other non-deterministic
  defaults, the same wire payload will migrate to different values on
  retries. The framework stores the ORIGINAL wire payload (not the
  migrated value) in its idempotency log to keep replays correct, but
  if you also write your own bookkeeping keyed on the migrated
  payload, you'll diverge.

- **`0` is not a valid schema_version.** The macro, the
  `SyncCollection::schema_version()` builder, the
  `SyncPushRequest::with_schema_version()` builder (which silently
  coerces `0` → `1`), AND the server's push handler (which rejects
  inbound `schema_version: 0` with `BadRequest`) all enforce this.
  Versions start at `1`.

- **`rejected[]` ordering is not preserved.** Correlate rejections
  with request mutations by `mutation_id`, not by index. The server
  may interleave migration-time rejections with source-side
  rejections in any order.

## Wire & error reference

- `SyncOpenStream.schema_version: u32` — server advertises per stream.
- `SyncPushRequest.schema_version: u32` — client tags every push.
- `SyncStreamSource::schema_version(&self) -> u32` — server-side
  declaration.
- `SyncStreamSource::migrate_payload(from, to, value) -> Value` —
  default impl returns `SchemaMigration`; override to opt in.
- `SyncError::SchemaMigration { stream, from, to }` —
  client-and-server-shared error variant; maps to
  `ServerError::BadRequest` on the wire.
- `CrudMigrateFn` — type alias for the migrator function signature.

## End-to-end example

See `crates/pocopine-sync-crud/tests/schema_migration.rs` for a
complete test that exercises both the default-reject path and the
`migrate_with` path against an axum-mounted resource.
