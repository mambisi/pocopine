# Migrating an app from the legacy sync client to `pocopine-sync-query`

This guide walks the migration of an app off the legacy
`pocopine_sync` collection client (`SyncClient` / `SyncCollection` /
`CollectionState`) onto `pocopine-sync-query` (`QueryClient` /
`QueryView` + the `Source` trait). The `examples/keep` app is the
worked reference — every pattern below ships there.

> **Context.** `pocopine-sync-query` is the current data-layer crate; the
> legacy *collection* client in `pocopine-sync` is the last thing apps
> still depend on. `pocopine-sync` remains the shared protocol/foundation
> (wire types, `SyncServer`, `SyncLocalStore`).

---

## The shape of the change

```
            LEGACY (pocopine-sync collection client)        CURRENT (pocopine-sync-query)
 model      plain struct                                    #[query_resource] struct + a Draft type
 server     impl SyncStreamSource (change-log + cursors)    impl Source (create/update/delete/list) + MutationLog
 pull       incremental changes + server cursor             snapshot-only (re-list on every pull)
 client     SyncClient.collection(store, sel).open()        QueryClient::observe(query) -> QueryView
 state      CollectionState<T> stored on the app store      QueryView<T> (rows bridged into the store)
 writes     hand-built ClientMutation + SyncRow             KeepNote::create/update/delete(...) typed builders
 version    explicit RowVersion column                      any monotonic row field (keep reuses updated_at_ms)
```

The biggest behavioral change: **`Source` pull is snapshot-only.**
There are no incremental changes or server cursors — every pull
re-lists the current rows (clamped to `query.limit()`), and the live
wake-up just tells the client *when* to re-snapshot. This removes the
change-log / cursor bookkeeping the legacy server carried.

---

## 1. The model — `#[query_resource]` + a draft

Annotate the row struct with `#[query_resource]` (it must sit **before**
the derives so it can strip the per-field `#[query_param]` markers), and
declare a draft carrying every editable field (everything but the id):

```rust
#[query_resource(name = "keep_notes_for_user", schema_version = 1, draft = KeepNoteDraft)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeepNote {
    pub id: String,
    #[query_param] pub pinned: bool,     // queryable filters (optional)
    #[query_param] pub archived: bool,
    pub title: String,
    /* … */
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeepNoteDraft { /* every field except `id` */ }

// Enables the auto-optimistic overlay on create/update.
impl From<(String, KeepNoteDraft)> for KeepNote { /* … */ }
```

The macro emits: `KeepNote::query()` (typed builder), `KeepNote::create/update/delete(...)`
(typed write builders), the `keep_notes_for_user` module (`NAME`,
`field::*` markers, `matches`, `resource(...)`), and `row_to_params_typed`.
The `name` **must equal the stream name** so live-wake-up topics line up.

> `#[query_resource]` does **not** require `Eq` on the row, so a row
> carrying `serde_json::Value` (like keep's rich-text body) is fine.

---

## 2. The server — `impl Source` + `MutationLog`

Replace the `SyncStreamSource` impl with `Source`. Because pull is
snapshot-only, the store collapses to "rows + an idempotency log":

```rust
impl<R: KeepRow> Source for SqliteKeepSource<R> {
    type Id = String; type Row = R; type Draft = R::Draft; type Context = ();

    fn extract_context<'a>(&'a self, _ctx: RequestContext)
        -> SourceFuture<'a, SyncResult<()>> { Box::pin(async { Ok(()) }) }

    fn list_stream<'a>(&'a self, _ctx: (), _q: &'a Query<R>) -> SourceStream<'a, R> {
        /* stream every row; the framework clamps to query.limit() */
    }
    fn get<'a>(&'a self, _ctx: (), id: String) -> SourceFuture<'a, SyncResult<Option<R>>> { /* … */ }
    fn create<'a>(&'a self, _ctx: (), id: String, draft: R::Draft)
        -> SourceFuture<'a, SyncResult<R>> { /* persist R::build(id, draft) */ }
    fn update<'a>(&'a self, _ctx: (), id: String, draft: R::Draft, expected: Option<RowVersion>)
        -> SourceFuture<'a, SyncResult<WriteResult<R>>> {
        /* if expected != stored.version_token() -> WriteResult::Conflict(stale) */
    }
    fn delete<'a>(&'a self, _ctx: (), id: String, expected: Option<RowVersion>)
        -> SourceFuture<'a, SyncResult<DeleteResult<R>>> { /* same concurrency check */ }

    // Durable idempotency: the store itself is the log.
    fn mutation_log(&self) -> Option<Arc<dyn MutationLog<R>>> { Some(Arc::new(self.clone())) }
}
```

**Optimistic concurrency without a version column:** keep reuses
`updated_at_ms` (already bumped on every edit) as the version token,
exposed via `version_field`:

```rust
let notes = keep_notes_for_user::resource(notes_source)          // pre-wires id + partition_by + mutation_log
    .version_field(|r: &KeepNote| Ok(Some(RowVersion::new(r.updated_at_ms.to_string())?)));
SyncServer::builder().public_stream(notes).events(backend).build();
```

`<name>::resource(src)` is **host-only** (the `Source` trait is
`#[cfg(not(wasm32))]`); the macro gates it accordingly. The row type
and typed write builders stay available on every target.

---

## 3. The client — `observe` + a `QueryView`→store bridge

Install the plugin (carry the local cache over):

```rust
let store: Rc<dyn SyncLocalStore> = Rc::new(SqliteLocalStore::new());
app.plugin(query_client_plugin().config(QueryClientConfig::default().with_local_store(store)));
```

`QueryView` lives outside the app store, so bridge its rows in via
`on_update` (or `QueryView::into_signal()` for a `Signal<Vec<Row>>`):

```rust
let client = Plugins.get::<Rc<QueryClient>>().unwrap();
let notes_view = client.observe(KeepNote::query().build());
// seed once, then mirror on every change:
store::<KeepStore>().update(|s| s.notes = notes_view.rows());
let v = notes_view.clone();
let token = notes_view.on_update(move || {
    store::<KeepStore>().update(|s| s.notes = v.rows());
});
// keep `notes_view` + `token` alive for the component's lifetime.
```

keep's existing store observers (`note_view_signature` → rebuild) then
recompute derived view state — the bridge only mirrors rows.

**Writes** go through the macro-emitted typed builders. create/update
carry the auto-optimistic overlay; for a snappy delete, route a
`RowChange::Delete` through `push`:

```rust
// create / update
client.push_typed(stream, mutation_id, KeepNote::update(id, draft, Some(base_version)), SYNC_PUSH_PATH).await?;
// optimistic delete
client.push(stream, mutation_id,
    MutationPayload::<String, KeepNoteDraft>::Delete(DeletePayload::new(id).with_expected_version(v)),
    RowChange::<KeepNote>::Delete(RowKey::new(id)?), SYNC_PUSH_PATH).await?;
```

The `base_version` is the row's `updated_at_ms` *before* the edit; the
new draft sets a fresh `updated_at_ms`.

> **Re-entrancy:** these helpers run inside `store.update` closures, so
> defer any follow-up `store.update` (e.g. an error flag) to
> `tick::next` to avoid a `RefCell` double-borrow.

---

## 4. Cleanups

- `CollectionState<T>` store fields become plain `Vec<T>` mirrored from
  the view; drop per-collection `.cursor` / `.set_error` (cursor is the
  driver's; route errors to an app-level `status` field).
- "manual refresh" usually disappears — the driver polls + lives-wakes
  automatically. keep's `refresh`/`resync` just clear the status flag.
- Verify end-to-end with a host test that POSTs a `MutationPayload`
  create to `/sync/v1/push`, reads the `query.invalidated` SSE frame,
  and asserts the snapshot `/pull` contains the row (see
  `examples/keep/tests/keep_flow.rs`).
