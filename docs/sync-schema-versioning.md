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

## Common pitfalls

- **Forgetting to bump `schema_version`** after a row/draft change.
  Older clients then push stale data and the server silently
  mis-deserializes. The compiler doesn't help — bumping the macro
  attribute is your responsibility.

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

- **`0` is not a valid schema_version.** The macro, the builder, and
  the wire-level validator all reject `schema_version = 0`. Start at
  `1`.

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
