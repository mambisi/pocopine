# Migration: `pocopine-sync-crud` → `pocopine-sync-query`

`pocopine-sync-crud` is **deprecated** as of RFC 090. The next
release deletes the crate. This guide is the migration path for
existing imports — a mechanical search-and-replace in most cases.

## Why

RFC 090 unifies the read and write paths into one crate. CRUD's
primitives moved to `pocopine_sync_query::write` (mutation log,
conflict types, write outcomes) or have new shapes
(`Source` replaces `CrudSource`; `#[query_resource(draft = …)]`
replaces hand-built `CrudMutationPayload`).

The merge isn't just a rename. The Source-based API is more
ergonomic: filtered subscriptions via `Issue::query().eq(...)`,
typed writes via `Issue::create(id, draft).optimistic(|p| ...).push(...)`,
and the server-side `Source::list(ctx, &Query<Row>)` lets sources
translate query params into storage filters (impossible under
`CrudSource::list(ctx, limit)`).

## Server-side migration

### `CrudSource` → `Source`

```diff
-use pocopine_sync_crud::{CrudSource, CrudWriteResult, CrudRemoveResult};
+use pocopine_sync_query::source::{Source, SourceFuture};
+use pocopine_sync_query::write::{WriteResult, DeleteResult};

-#[pocopine_sync_crud::async_trait]
-impl CrudSource for IssuesSource {
+impl Source for IssuesSource {
     type Id = String;
     type Row = Issue;
     type Draft = IssueDraft;

-    async fn list(
-        &self,
-        ctx: RequestContext,
-        limit: usize,
-    ) -> SyncResult<Vec<Self::Row>> { ... }
+    fn list<'a>(
+        &'a self,
+        ctx: RequestContext,
+        query: &'a Query<Self::Row>,
+    ) -> SourceFuture<'a, SyncResult<Vec<Self::Row>>> {
+        // NEW: query.params() to push WHERE clauses to storage.
+        // CRUD-style fallback: ignore the query, return up to
+        // query.limit() rows.
+        Box::pin(async move { ... })
+    }

-    async fn save(
+    fn update<'a>(
-        &self, ctx, id, draft, base_version,
+        &'a self, ctx, id, draft, expected_version,
-    ) -> SyncResult<CrudWriteResult<Self::Row>> { ... }
+    ) -> SourceFuture<'a, SyncResult<WriteResult<Self::Row>>> { ... }

-    async fn remove(
+    fn delete<'a>(
-        &self, ctx, id, base_version,
+        &'a self, ctx, id, expected_version,
-    ) -> SyncResult<CrudRemoveResult<Self::Row>> { ... }
+    ) -> SourceFuture<'a, SyncResult<DeleteResult<Self::Row>>> { ... }
}
```

**Method renames**: `save` → `update`, `remove` → `delete`.
**Parameter rename**: `base_version` → `expected_version`.
**Outcome type rename**: `CrudRemoveResult` → `DeleteResult`.
**Async shape**: returns `SourceFuture<'a, T>` (boxed) instead of
`async fn` — same generated code via `Box::pin(async move { … })`.

### `CrudResource` builder → `source()`

```diff
-use pocopine_sync_crud::{resource, MemoryCrudMutationLog};
+use pocopine_sync_query::source::source;
+use pocopine_sync_query::write::MemoryMutationLog;

 let r = resource("issues", IssueSource::default())?
     .id(|r| r.id.clone())
-    .version(|r| r.version)
+    .version_field(|r| Ok(Some(RowVersion::new(r.version.to_string())?)))
-    .mutation_log(MemoryCrudMutationLog::new())
+    .mutation_log(MemoryMutationLog::new())
-    .params_of(issues::row_to_params_typed);
+    .partition_by(issues::row_to_params_typed);
```

**Function rename**: `resource(...)` → `source(...)` (from
`pocopine_sync_query::source`).
**Builder method renames**:
- `.version(closure)` → `.version_field(closure)` (closure now
  returns `SyncResult<Option<RowVersion>>` directly).
- `.params_of(closure)` → `.partition_by(closure)`.
- `.mutation_log(...)` keeps the name; the trait type renamed
  to `MutationLog<Row>` (was `CrudMutationLog<Row>`).

## Client-side migration

### Wire envelope

CRUD's `CrudMutationPayload` and sync-query's `MutationPayload` have
**different wire shapes**. CRUD uses
`{"op": "create", "payload": {...}}`. Sync-query uses the flat shape:
`{"op": "create", "id": "...", "draft": {...}}`.

This means **clients hitting a Source-backed server must serialize
the new shape**, not the old one. The macro-emitted typed methods
handle this automatically; hand-built mutations need an update.

### Typed writes via `#[query_resource(draft = …)]`

The macro now emits `Row::create(id, draft)` / `update` / `delete`
methods when you opt in with `draft = TypeName`:

```rust
#[query_resource(name = "issues", schema_version = 1, draft = IssueDraft)]
pub struct Issue { /* ... */ }

pub struct IssueDraft { /* editable fields */ }

// Then:
let outcome = Issue::create("i1".to_string(), draft)
    .optimistic(|p| /* build the optimistic Row from the payload */)
    .push(&client, mutation_id, push_url)
    .await?;
```

This replaces the CRUD pattern:

```diff
-let payload = CrudMutationPayload::create(id, draft);
-let mutation = payload.into_sync_draft()?;
-self.plugin::<SyncClient>()
-    .collection(this, |s| &mut s.writes)
-    .stream(STREAM)
-    .and_then(|c| c.push_with_generated_id(mutation, Some(optimistic)));
+Issue::create(id, draft)
+    .optimistic(|p| /* synthesize Issue */)
+    .push(&self.plugin::<QueryClient>(), mutation_id, push_url)
+    .await?;
```

### `CollectionState<T>` for writes

CRUD apps that carry a `CollectionState<Issue>` field on components
(to feed `SyncClient::collection(this, accessor).push(...)`) can
drop that field once they migrate to the typed write API.
`QueryClient::push` (and `push_typed`) routes the optimistic
overlay through the existing query subscriptions — no separate
write-state needed.

## Type renames at a glance

| CRUD (deprecated) | sync-query (canonical) |
|---|---|
| `CrudSource` | `Source` |
| `CrudResource` | `SourceResource` |
| `resource()` | `source()` |
| `.params_of(...)` | `.partition_by(...)` |
| `.version(...)` | `.version_field(...)` |
| `CrudMutationLog<Row>` | `MutationLog<Row>` |
| `MemoryCrudMutationLog` | `MemoryMutationLog` |
| `CrudAcceptedMutation` | `AcceptedMutation` |
| `CrudMutationReservation` | `MutationReservation` |
| `CrudWriteResult<Row>` | `WriteResult<Row>` |
| `CrudRemoveResult<Row>` | `DeleteResult<Row>` |
| `CrudConflict<Row>` | `Conflict<Row>` |
| `MemoryCrudScopeFn` | `MemoryScopeFn` |
| `Source::save` | `Source::update` |
| `Source::remove` | `Source::delete` |
| `base_version` arg | `expected_version` arg |
| wire `"op": "save"` | wire `"op": "update"` |
| wire `"op": "remove"` | wire `"op": "delete"` |
| wire `{op, payload: {...}}` | wire `{op, id, draft, ...}` (flat) |

## Help

If a CRUD pattern doesn't have an obvious sync-query equivalent,
open an issue with `[rfc-090]` in the title and we'll either point
at the canonical shape or land the missing piece.
