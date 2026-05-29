//! Server-side `Source` trait for Query-native resources.
//!
//! This is the **read-and-write** counterpart to `Query<Row>` /
//! `QueryView<Row>`. See [RFC 090](../../rfcs/rfc-090-merge-crud-into-query.md)
//! for the motivation; in short:
//!
//! - `pocopine-sync-crud`'s `CrudSource::list(ctx, limit)` is
//!   bandwidth-blind. It fetches every row the caller could see, then
//!   the macro's predicate evaluator filters client-side.
//! - `Source::list(ctx, &Query<Row>)` hands the typed query to the
//!   source so SQLx / SQLite / Firestore can push a `WHERE
//!   workspace_id = ? AND status IN (?)` clause down to storage.
//!
//! At 1M users that's the difference between a working multi-tenant
//! app and one that times out. This is the "Phase 1" deliverable
//! from RFC 090: trait shape + a thin adapter that proves the
//! Query-aware list works end-to-end via `SyncStreamSource`.
//!
//! # Status — Phase 1 of RFC 090
//!
//! Phase 1 ships the trait and a minimal `Resource<S>` adapter that
//! handles the **pull** lifecycle (Query-aware `list`). The **push**
//! side is best-effort: every push attempt re-runs (no idempotency
//! log yet). The mutation log moves into `pocopine-sync-query` in
//! Phase 2; client-side push routing lands in Phase 3.
//!
//! Apps that need production-grade idempotency / atomic transactions
//! TODAY should use `pocopine-sync-crud::resource(...).mutation_log(
//! ...).transactional(...)`. After Phase 2 the same primitives live
//! in `pocopine-sync-query` under shorter names.

use std::sync::Arc;

use pocopine_sync::{
    RowVersion, StreamParams, SyncCollectionName, SyncError, SyncOp, SyncRejectedMutation,
    SyncResult, SyncRow, SyncStreamName,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::query::Query;
use crate::write::{AcceptedMutation, MutationLog, MutationPayload};

/// Identity boundary for `Source` rows. Mirrors
/// `pocopine_sync_crud::ResourceId` — Query reuses the same contract
/// so a `Source` and a `CrudSource` impl can share the same `Id`
/// type in apps that adopt Source incrementally.
///
/// The `Serialize + DeserializeOwned` bounds let `Source::push`
/// decode the wire mutation payload (`MutationPayload<Id, Draft>`)
/// directly. Phase 1 only required `to_row_key`; Phase 2b widens to
/// the full identity contract now that push lands.
pub trait SourceId: Clone + Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Project this id into a sync row key. Used by the adapter to
    /// fill the `key` field on `SyncRow<Value>` envelopes.
    fn to_row_key(&self) -> SyncResult<pocopine_sync::RowKey>;
}

impl SourceId for String {
    fn to_row_key(&self) -> SyncResult<pocopine_sync::RowKey> {
        pocopine_sync::RowKey::new(self.clone())
    }
}

// RFC 090 Phase 2a — `WriteResult`/`DeleteResult`/`Conflict` were
// briefly defined here in Phase 1 because they appear in `Source`
// method signatures. Phase 2a moved them to `crate::write` (their
// long-term home alongside the mutation log + idempotency
// infrastructure). The `pub use` here keeps the Phase 1 public
// surface working — code that wrote
// `use pocopine_sync_query::source::{WriteResult, Conflict, DeleteResult};`
// keeps compiling.
pub use crate::write::{Conflict, DeleteResult, WriteResult};

/// Boxed async-trait future. Sources don't need to name this — the
/// `#[async_trait]` macro generates the right wrappers automatically.
pub type SourceFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Type-erased projector from a canonical row into the RFC 088 §C
/// `(stream, params_hash)` partition fields. Stored on
/// [`SourceResource`]; attached via [`SourceResource::params_of`].
/// Phase 4 of RFC 090 auto-wires this from `#[query_resource]`;
/// until then, callers pass `<resource>::row_to_params_typed`
/// explicitly.
pub type SourceParamsFn<Row> = dyn Fn(&Row) -> StreamParams + Send + Sync + 'static;

/// Type-erased extractor that pulls the optimistic-concurrency
/// version off a typed row. Stored on [`SourceResource`]; attached
/// via [`SourceResource::version`]. When unset, pulled rows ship
/// with `SyncRow::version = None` and Phase 2's `Source::save`
/// `base_version` checks degrade to "no concurrency control."
///
/// Phase 2 will accept polyglot input types (numeric versions,
/// `String`, `Option<String>`, etc.) via a `RowVersionValue`-style
/// blanket impl. Phase 1 keeps it simple: the user returns an
/// `Option<RowVersion>` directly. Mechanical to widen later
/// without breaking the closure-shaped surface.
pub type SourceVersionFn<Row> =
    dyn Fn(&Row) -> SyncResult<Option<RowVersion>> + Send + Sync + 'static;

/// Query-native server source. Replaces `CrudSource` in RFC 090's
/// merged design.
///
/// **Key shape difference from `CrudSource`**: `list` receives a
/// `&Query<Self::Row>` instead of a `limit`. The source can
/// introspect the query's params and translate them into a storage
/// filter (`WHERE workspace_id = ? …`) instead of fetching every
/// row and relying on client-side predicate matching.
///
/// # Use cases
///
/// - **Reads only** (CDC-fed streams, server-rendered views): implement
///   `list` + `get`; mark `create`/`save`/`remove` `unsupported()`.
/// - **Writes only** (auth-gated write endpoints with no filtered
///   reads): implement `create`/`save`/`remove`; `list` can return
///   an empty vec.
/// - **Both** (the canonical multi-tenant app): implement everything.
///
/// # Migration from `CrudSource`
///
/// Most CrudSource impls port mechanically:
///
/// ```ignore
/// // Before
/// async fn list(&self, ctx: RequestContext, limit: usize) -> ... {
///     /* fetch first `limit` rows */
/// }
///
/// // After
/// async fn list(&self, ctx: RequestContext, query: &Query<Row>) -> ... {
///     let limit = query.limit().unwrap_or(DEFAULT_LIMIT) as usize;
///     /* fetch first `limit` rows */   // identical body
/// }
/// ```
///
/// Sources that want the §C scaling win override `list` to honor
/// `query.params()`:
///
/// ```ignore
/// async fn list(&self, ctx: RequestContext, query: &Query<Issue>) -> ... {
///     let ws = query.params().get("workspace_id")
///         .and_then(|v| v.as_str())
///         .ok_or_else(|| SyncError::client("workspace_id required"))?;
///     sqlx::query_as::<_, Issue>("SELECT * FROM issues WHERE workspace_id = ? LIMIT ?")
///         .bind(ws)
///         .bind(query.limit().unwrap_or(1000) as i64)
///         .fetch_all(&self.pool)
///         .await
///         .map_err(|e| SyncError::backend(e.to_string()))
/// }
/// ```
#[allow(async_fn_in_trait)]
pub trait Source: Send + Sync + 'static {
    type Id: SourceId;
    type Row: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;
    type Draft: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Snapshot pull. The query is server-reconstructed from the
    /// wire `SyncPullRequest`'s `params`, `order_by`, and `limit`
    /// fields. Implementations SHOULD honor `query.params()` and
    /// `query.limit()`; the default behavior is "ignore the query
    /// shape and return up to `limit` rows", which preserves
    /// `CrudSource::list(ctx, limit)` semantics for incremental
    /// migration.
    fn list<'a>(
        &'a self,
        ctx: pocopine_auth::RequestContext,
        query: &'a Query<Self::Row>,
    ) -> SourceFuture<'a, SyncResult<Vec<Self::Row>>>;

    /// Point lookup. Returns `None` for missing rows AND for rows the
    /// caller cannot see — the source decides whether to leak
    /// existence.
    fn get<'a>(
        &'a self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
    ) -> SourceFuture<'a, SyncResult<Option<Self::Row>>>;

    /// Create a row. Server-controlled fields (id, version, created_at)
    /// are filled by the implementation; the `Draft` carries only
    /// client-supplied fields.
    fn create<'a>(
        &'a self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SourceFuture<'a, SyncResult<Self::Row>>;

    /// Update an existing row. `expected_version` is the optimistic-
    /// concurrency token the client observed before editing. Source
    /// MUST compare it against the stored row's version in the same
    /// DB operation as the write. Return `WriteResult::Conflict` if
    /// the versions differ.
    fn update<'a>(
        &'a self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        draft: Self::Draft,
        expected_version: Option<RowVersion>,
    ) -> SourceFuture<'a, SyncResult<WriteResult<Self::Row>>>;

    /// Delete an existing row. Same concurrency contract as `update`.
    fn delete<'a>(
        &'a self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        expected_version: Option<RowVersion>,
    ) -> SourceFuture<'a, SyncResult<DeleteResult<Self::Row>>>;
}

/// Reconstruct a `Query<Row>` from a wire `SyncPullRequest`. The
/// reconstructed query has no `matches_fn` / `partition_hash_fn`
/// because those are macro-emitted client-side and irrelevant for
/// the server's filter translation — sources read `query.params()`,
/// `query.order_by()`, `query.limit()` and never call `.matches()`
/// in their `list` impls.
///
/// `cfg(not(target_arch = "wasm32"))` because the server adapter is
/// host-only.
pub fn query_from_pull_request<Row>(request: &pocopine_sync::SyncPullRequest) -> Query<Row> {
    let mut builder = Query::<Row>::builder(request.stream.clone());
    for (key, value) in request.params.iter() {
        builder = builder.raw_param(key.clone(), value.clone());
    }
    // The wire `SyncPullRequest` carries `limit: u32`, with `0`
    // meaning "use the source's default". Translate the zero
    // sentinel to `None` so sources can pattern-match
    // `query.limit()` cleanly.
    //
    // `order_by` doesn't ride on the pull request today (the
    // server defines snapshot ordering). When the wire protocol
    // grows ordering, this is where it lands.
    if request.limit > 0 {
        builder = builder.limit(request.limit);
    }
    builder.build()
}

/// Default snapshot row cap when the request doesn't carry one.
/// Matches `pocopine_sync_crud::DEFAULT_CRUD_SNAPSHOT_ROW_LIMIT` so
/// migration doesn't shift the silent fallback.
pub const DEFAULT_SOURCE_SNAPSHOT_ROW_LIMIT: usize = 1_000;

/// Builder returned by [`source`]. Phase 1 keeps it minimal —
/// `.id(...)` is the only required call. `.mutation_log(...)`,
/// `.params_of(...)`, `.transactional(...)` land in Phases 2-4 as
/// the lifecycle pieces migrate over.
pub struct SourceResourceBuilder<S: Source> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    source: S,
    max_snapshot_rows: usize,
    schema_version: u32,
}

/// Start building a Query-native sync stream from a `Source` impl.
///
/// Phase 1 of RFC 090 — the entry point that demonstrates Query-aware
/// `list` works end-to-end. Phase 2 adds `.mutation_log(...)` for
/// idempotency; Phase 3 hooks `QueryClient::push` for client-side
/// optimistic routing.
pub fn source<S>(name: impl Into<String>, source: S) -> SyncResult<SourceResourceBuilder<S>>
where
    S: Source,
{
    let name = name.into();
    Ok(SourceResourceBuilder {
        stream: SyncStreamName::new(name.clone())?,
        collection: SyncCollectionName::new(name)?,
        source,
        max_snapshot_rows: DEFAULT_SOURCE_SNAPSHOT_ROW_LIMIT,
        schema_version: pocopine_sync::default_schema_version_one(),
    })
}

impl<S: Source> SourceResourceBuilder<S> {
    /// Override the default snapshot row cap.
    pub fn max_snapshot_rows(mut self, limit: usize) -> SyncResult<Self> {
        if limit == 0 {
            return Err(SyncError::client(
                "Source max snapshot rows must be greater than zero",
            ));
        }
        self.max_snapshot_rows = limit;
        Ok(self)
    }

    /// Set the schema version advertised on `/open` responses.
    pub fn schema_version(mut self, version: u32) -> SyncResult<Self> {
        if version == 0 {
            return Err(SyncError::client(
                "schema_version must be >= 1 (versions start at 1)",
            ));
        }
        self.schema_version = version;
        Ok(self)
    }

    /// Attach the row id extractor. Required because the wire layer
    /// keys every row by `RowKey` and the source's `Id` type might
    /// not be `String`-shaped.
    pub fn id<IdOf>(self, id_of: IdOf) -> SourceResource<S, IdOf>
    where
        IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    {
        SourceResource {
            stream: self.stream,
            collection: self.collection,
            source: Arc::new(self.source),
            max_snapshot_rows: self.max_snapshot_rows,
            schema_version: self.schema_version,
            id_of: Arc::new(id_of),
            version_of: None,
            params_of: None,
            mutation_log: None,
        }
    }
}

/// `SyncStreamSource` adapter wrapping a `Source` impl. Phase 1
/// handles `pull` end-to-end (Query-aware `list`); `push` is
/// best-effort (no idempotency log until Phase 2).
pub struct SourceResource<S: Source, IdOf> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    source: Arc<S>,
    max_snapshot_rows: usize,
    schema_version: u32,
    id_of: Arc<IdOf>,
    /// Row-version extractor for optimistic-concurrency. Set via
    /// [`Self::version`]. When `None`, pulled rows ship with
    /// `SyncRow::version = None` and Phase 2's `Source::save`
    /// `base_version` checks degrade to "no concurrency control."
    version_of: Option<Arc<SourceVersionFn<S::Row>>>,
    /// RFC 088 §C row → partition projector. Set via
    /// [`Self::params_of`]. When `None`, per-params publishes are
    /// disabled and the server publishes only the bare stream
    /// topic. Phase 4 will auto-wire this from `#[query_resource]`.
    params_of: Option<Arc<SourceParamsFn<S::Row>>>,
    /// Idempotency log for push mutations. Set via
    /// [`Self::mutation_log`] (RFC 090 Phase 2b). When `None`,
    /// `push` rejects with an `unsupported` error — the adapter
    /// won't perform writes without an idempotency boundary because
    /// a retry without dedup would run `Source::create`/`save`
    /// twice on the same mutation_id, double-applying writes
    /// under client retries. Apps that genuinely want non-
    /// idempotent push should attach a no-op log explicitly.
    mutation_log: Option<Arc<dyn MutationLog<S::Row>>>,
}

impl<S, IdOf> SourceResource<S, IdOf>
where
    S: Source,
    IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
{
    /// Extract the optimistic-concurrency version from a row.
    /// Without this, pulled rows ship `SyncRow::version = None` and
    /// `Source::update`'s `expected_version` check has nothing to
    /// compare against — concurrent writers go undetected.
    ///
    /// The closure returns `SyncResult<Option<RowVersion>>`:
    /// - `Ok(None)` for rows with no meaningful version
    /// - `Ok(Some(v))` for the canonical version token
    /// - `Err(...)` when extraction itself fails (corrupt row shape)
    ///
    /// Most apps map an integer `version` field:
    /// `.version_field(|r| Ok(Some(RowVersion::new(r.version.to_string())?)))`.
    pub fn version_field<F>(mut self, extract: F) -> Self
    where
        F: Fn(&S::Row) -> SyncResult<Option<RowVersion>> + Send + Sync + 'static,
    {
        self.version_of = Some(Arc::new(extract));
        self
    }

    /// Partition rows for RFC 088 §C precise live wakeups: the
    /// closure projects a row into the `StreamParams` that identify
    /// its tenant/partition. Without this, the server publishes
    /// only the bare stream topic (every subscriber wakes for every
    /// row).
    ///
    /// Phase 4 will auto-wire this from `#[query_resource]`'s
    /// macro-emitted `row_to_params_typed`. Until then, pass the
    /// macro output explicitly:
    ///
    /// ```ignore
    /// source("issues", IssueSource::default())?
    ///     .id(|r| r.id.clone())
    ///     .partition_by(issues::row_to_params_typed);
    /// ```
    pub fn partition_by<F>(mut self, projector: F) -> Self
    where
        F: Fn(&S::Row) -> StreamParams + Send + Sync + 'static,
    {
        self.params_of = Some(Arc::new(projector));
        self
    }

    /// Attach an idempotency log for the push lifecycle (RFC 090
    /// Phase 2b). Without it, `push` rejects with `unsupported`
    /// because a client retry of the same `mutation_id` would
    /// otherwise run `Source::create`/`save`/`remove` twice and
    /// double-apply writes.
    ///
    /// For tests and single-process demos, pass
    /// `MemoryMutationLog::new()`. For production, pair with a
    /// scoped backend (`MemoryMutationLog::with_scope_fn(...)` or
    /// the sqlx-backed log in `pocopine-sync-sqlx`) so the
    /// idempotency boundary aligns with your tenant authorization
    /// scope.
    pub fn mutation_log<Log>(mut self, log: Log) -> Self
    where
        Log: MutationLog<S::Row>,
    {
        self.mutation_log = Some(Arc::new(log));
        self
    }
}

impl<S, IdOf> pocopine_sync::SyncStreamSource for SourceResource<S, IdOf>
where
    S: Source,
    IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
{
    fn stream(&self) -> &SyncStreamName {
        &self.stream
    }

    fn collection(&self) -> &SyncCollectionName {
        &self.collection
    }

    fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Query-native sources accept arbitrary params (the source
    /// decides which shapes it can serve). Reserved-key-only param
    /// maps pass; non-reserved keys go through to the source's
    /// `list` for translation.
    fn validate_params(&self, _params: &StreamParams) -> SyncResult<()> {
        Ok(())
    }

    /// Source pushes don't carry params on the wire today; accept any.
    fn validate_push_params(&self, _params: &StreamParams) -> SyncResult<()> {
        Ok(())
    }

    /// RFC 088 §C: delegate to the closure attached via
    /// [`SourceResource::params_of`]. Phase 4 will auto-wire from the
    /// macro.
    fn row_to_params(&self, row: &Value) -> SyncResult<StreamParams> {
        let Some(params_of) = self.params_of.as_ref() else {
            return Ok(StreamParams::new());
        };
        // The row reaches `row_to_params` from the push-publish path
        // (after `Source::create`/`save` already returned it); it's
        // server-controlled state, not the client's request body.
        // A deserialize failure here therefore signals a Source impl
        // bug or schema drift, not bad client input — classify as
        // `backend` so observability tools blame the right side.
        let typed: S::Row = serde_json::from_value(row.clone()).map_err(|e| {
            SyncError::backend(format!(
                "row_to_params: {} row didn't deserialize for §C projection: {}",
                self.stream.as_str(),
                e
            ))
        })?;
        Ok(params_of(&typed))
    }

    fn pull<'a>(
        &'a self,
        ctx: pocopine_auth::RequestContext,
        request: pocopine_sync::SyncPullRequest,
    ) -> pocopine_sync::SyncBoxFuture<'a, pocopine_sync::SyncPullResponse<Value>> {
        let source = self.source.clone();
        let id_of = self.id_of.clone();
        let version_of = self.version_of.clone();
        let stream = self.stream.clone();
        let collection = self.collection.clone();
        let max_snapshot_rows = self.max_snapshot_rows;
        Box::pin(async move {
            if request.stream != stream {
                return Err(SyncError::UnknownStream(request.stream.to_string()));
            }

            // The whole point of Phase 1: hand the Source a typed
            // Query reconstructed from the wire request. SQLx-style
            // impls translate `query.params()` to SQL filters; CRUD-
            // style fallbacks ignore the params and use `query.limit()`.
            //
            // Clamp the query's `limit()` to `max_snapshot_rows`
            // BEFORE calling `Source::list`. This is a defense in
            // depth against pathological sources that ignore the
            // limit altogether: even a buggy `list` impl that
            // `SELECT *`s a 10M-row table sees a bounded `limit()`
            // and can't allocate unbounded memory if it honors it
            // at all. The post-call `rows.len()` check below is the
            // belt-and-braces — it still catches sources that ALSO
            // ignore the clamped value, but at least the memory
            // ceiling is now visible from the contract.
            let mut query: Query<S::Row> = query_from_pull_request(&request);
            let effective_limit = query
                .limit()
                .map(|l| l.min(max_snapshot_rows as u32))
                .unwrap_or(max_snapshot_rows as u32);
            query.set_limit(effective_limit);

            let rows = source.list(ctx, &query).await?;
            if rows.len() > max_snapshot_rows {
                return Err(SyncError::backend(format!(
                    "Source returned {} rows, exceeding max snapshot row limit {} \
                     (the adapter clamped the query's limit to {} before calling list — \
                     this source impl is ignoring `query.limit()` entirely)",
                    rows.len(),
                    max_snapshot_rows,
                    effective_limit
                )));
            }

            let mut sync_rows: Vec<SyncRow<Value>> = Vec::with_capacity(rows.len());
            for row in rows {
                let key = (id_of)(&row).to_row_key()?;
                // Extract the row's optimistic-concurrency version
                // BEFORE serializing — the closure operates on the
                // typed `&S::Row`, not the JSON Value. A `None`
                // extractor means the source has opted out of
                // version tracking (`SyncRow::version = None`,
                // Phase 2's `base_version` checks degrade to
                // "trust the client"). This is the fix for
                // RFC 090 phase 1 code-review finding #1.
                let version = match version_of.as_ref() {
                    Some(extract) => (extract)(&row)?,
                    None => None,
                };
                let value = serde_json::to_value(&row).map_err(|e| {
                    SyncError::backend(format!("Source row failed to serialize: {e}"))
                })?;
                sync_rows.push(SyncRow {
                    key,
                    version,
                    value,
                    pending: false,
                    conflict: false,
                });
            }

            Ok(pocopine_sync::SyncPullResponse::snapshot(
                stream, collection, sync_rows, None,
            ))
        })
    }

    /// RFC 090 Phase 2b push lifecycle.
    ///
    /// For each mutation in the request:
    ///   1. Consult `MutationLog::accepted_mutation` for idempotency.
    ///      A previously-accepted replay with matching wire envelope
    ///      returns `accepted` directly (no Source call). A replay
    ///      with different contents is rejected.
    ///   2. Decode the wire payload as `MutationPayload<S::Id,
    ///      S::Draft>` and validate it against the wire op.
    ///   3. Dispatch to `Source::create` / `save` / `remove`.
    ///   4. On `Accepted`, record in the mutation log and return
    ///      the canonical row.
    ///   5. On `Conflict`, surface a `SyncConflict` carrying the
    ///      server-visible row.
    ///
    /// Without a `.mutation_log(...)` attachment, push returns
    /// `unsupported` — see the field doc on `mutation_log` for
    /// rationale.
    fn push<'a>(
        &'a self,
        ctx: pocopine_auth::RequestContext,
        request: pocopine_sync::SyncPushRequest<Value>,
    ) -> pocopine_sync::SyncBoxFuture<'a, pocopine_sync::SyncPushResponse<Value>> {
        let source = self.source.clone();
        let id_of = self.id_of.clone();
        let version_of = self.version_of.clone();
        let mutation_log = self.mutation_log.clone();
        let stream = self.stream.clone();
        let collection = self.collection.clone();
        Box::pin(async move {
            if request.stream != stream {
                return Err(SyncError::UnknownStream(request.stream.to_string()));
            }

            let Some(mutation_log) = mutation_log else {
                return Err(SyncError::unsupported(
                    "Source push requires .mutation_log(...) on the builder. \
                     For tests use MemoryMutationLog::new(); for production pair \
                     with a scoped backend so idempotency aligns with your \
                     tenant authorization domain.",
                ));
            };

            let mut response = pocopine_sync::SyncPushResponse::new(stream.clone());
            response.collection = Some(collection.clone());

            for mut mutation in request.mutations {
                let mutation_id = mutation.id.clone();
                let key = mutation.key.clone();
                let op = mutation.op;
                let base_version = mutation.base_version.clone();
                // RFC 090 review #5: honor the sync framework's
                // schema-migration outcome. `take_processing_payload`
                // returns the MIGRATED value if migration ran
                // successfully, or the original wire payload
                // otherwise. The idempotency reservation uses the
                // ORIGINAL wire payload (so retries of the same wire
                // envelope hit the log regardless of whether/how the
                // server migrated them).
                let processing_payload = mutation.take_processing_payload();
                let payload_value = mutation.payload;

                // 1. Atomic reserve-or-return-existing.
                // RFC 090 review #3: replaces check-then-record with
                // a single primitive so concurrent retries can't both
                // run Source::create/update/delete on the same
                // mutation_id.
                let candidate = AcceptedMutation::new(
                    mutation_id.clone(),
                    op,
                    key.clone(),
                    payload_value.clone(),
                );
                let reservation = mutation_log.reserve_mutation(&ctx, candidate).await?;
                let reserved = match reservation {
                    crate::write::MutationReservation::Reserved => true,
                    crate::write::MutationReservation::AlreadyAccepted(prior) => {
                        if prior.matches(op, key.as_ref(), &payload_value) {
                            response.accepted.push(mutation_id);
                        } else {
                            response.rejected.push(SyncRejectedMutation {
                                mutation_id,
                                key,
                                reason: "mutation id was already accepted with different contents"
                                    .to_string(),
                            });
                        }
                        continue;
                    }
                };
                debug_assert!(reserved);

                // 2. Decode the (possibly migrated) payload.
                let processing_payload = match processing_payload {
                    Ok(value) => value,
                    Err(reason) => {
                        response.rejected.push(SyncRejectedMutation {
                            mutation_id,
                            key,
                            reason,
                        });
                        continue;
                    }
                };
                let payload = match serde_json::from_value::<MutationPayload<S::Id, S::Draft>>(
                    processing_payload,
                ) {
                    Ok(p) => p,
                    Err(err) => {
                        response.rejected.push(SyncRejectedMutation {
                            mutation_id,
                            key,
                            reason: format!("invalid Source mutation payload: {err}"),
                        });
                        continue;
                    }
                };

                if payload.sync_op() != op {
                    response.rejected.push(SyncRejectedMutation {
                        mutation_id,
                        key,
                        reason: "Source payload does not match sync operation".to_string(),
                    });
                    continue;
                }

                let expected_key = payload.id().to_row_key()?;
                if key.as_ref() != Some(&expected_key) {
                    response.rejected.push(SyncRejectedMutation {
                        mutation_id,
                        key,
                        reason: "Source mutation row key does not match payload id".to_string(),
                    });
                    continue;
                }

                // 3. Dispatch to the source.
                let outcome = apply_payload::<S>(
                    source.as_ref(),
                    ctx.clone(),
                    mutation_id.clone(),
                    expected_key.clone(),
                    base_version,
                    payload,
                )
                .await?;

                // 4-5. Surface outcome. Reservation was atomic at
                // step 1; no separate record step needed.
                match outcome {
                    SourceApplyOutcome::Accepted { mutation_id, row } => {
                        response.accepted.push(mutation_id);
                        if let Some(row) = row {
                            response.rows.push(row_to_value_with_version(
                                row,
                                expected_key,
                                version_of.as_deref(),
                            )?);
                        }
                    }
                    SourceApplyOutcome::Rejected(rejected) => response.rejected.push(rejected),
                    SourceApplyOutcome::Conflict {
                        mutation_id,
                        key,
                        conflict,
                    } => {
                        response.conflicts.push(pocopine_sync::SyncConflict {
                            mutation_id,
                            key,
                            server_row: conflict
                                .server_row
                                .map(|row| {
                                    let key = (id_of)(&row).to_row_key()?;
                                    row_to_value_with_version(row, key, version_of.as_deref())
                                })
                                .transpose()?,
                            reason: conflict.reason,
                        });
                    }
                }
            }

            Ok(response)
        })
    }
}

/// Outcome of one mutation's dispatch to `Source::create/save/remove`.
/// Internal — the `push` method translates these into wire response
/// variants.
enum SourceApplyOutcome<Row> {
    Accepted {
        mutation_id: pocopine_sync::MutationId,
        row: Option<Row>,
    },
    Rejected(SyncRejectedMutation),
    Conflict {
        mutation_id: pocopine_sync::MutationId,
        key: Option<pocopine_sync::RowKey>,
        conflict: Conflict<Row>,
    },
}

async fn apply_payload<S: Source>(
    source: &S,
    ctx: pocopine_auth::RequestContext,
    mutation_id: pocopine_sync::MutationId,
    key: pocopine_sync::RowKey,
    base_version: Option<RowVersion>,
    payload: MutationPayload<S::Id, S::Draft>,
) -> SyncResult<SourceApplyOutcome<S::Row>> {
    match payload {
        MutationPayload::Create(p) => {
            if base_version.is_some() {
                return Ok(SourceApplyOutcome::Rejected(SyncRejectedMutation {
                    mutation_id,
                    key: Some(key),
                    reason: "create does not accept a base row version".to_string(),
                }));
            }
            let row = source.create(ctx, p.id, p.draft).await?;
            Ok(SourceApplyOutcome::Accepted {
                mutation_id,
                row: Some(row),
            })
        }
        MutationPayload::Update(p) => {
            // Payload-carried expected_version takes precedence over
            // the wire envelope's base_version. They should agree
            // (the typed builder fills both) but if they diverge
            // the payload wins — it's the typed contract the source
            // understands.
            let version = p.expected_version.clone().or(base_version);
            let outcome = source.update(ctx, p.id, p.draft, version).await?;
            match outcome {
                WriteResult::Applied(row) => Ok(SourceApplyOutcome::Accepted {
                    mutation_id,
                    row: Some(row),
                }),
                WriteResult::Conflict(conflict) => Ok(SourceApplyOutcome::Conflict {
                    mutation_id,
                    key: Some(key),
                    conflict,
                }),
            }
        }
        MutationPayload::Delete(p) => {
            let version = p.expected_version.clone().or(base_version);
            let outcome = source.delete(ctx, p.id, version).await?;
            match outcome {
                DeleteResult::Applied => Ok(SourceApplyOutcome::Accepted {
                    mutation_id,
                    row: None,
                }),
                DeleteResult::Conflict(conflict) => Ok(SourceApplyOutcome::Conflict {
                    mutation_id,
                    key: Some(key),
                    conflict,
                }),
            }
        }
    }
}

fn row_to_value_with_version<Row>(
    row: Row,
    key: pocopine_sync::RowKey,
    version_of: Option<&SourceVersionFn<Row>>,
) -> SyncResult<SyncRow<Value>>
where
    Row: Serialize,
{
    let version = match version_of {
        Some(extract) => (extract)(&row)?,
        None => None,
    };
    let value = serde_json::to_value(&row)
        .map_err(|e| SyncError::backend(format!("Source row failed to serialize: {e}")))?;
    Ok(SyncRow {
        key,
        version,
        value,
        pending: false,
        conflict: false,
    })
}

// Unused-variable silencer: `SyncOp` is imported for the push impl
// even when the source tests don't reference it directly.
#[allow(dead_code)]
fn _suppress_unused() {
    let _ = SyncOp::Upsert;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_sync::SyncResult;

    fn test_ctx() -> pocopine_auth::RequestContext {
        pocopine_auth::RequestContext::new(
            http::Method::POST,
            "/__pocopine/sync/v1/pull".parse().unwrap(),
            http::HeaderMap::new(),
        )
    }

    // A trivial Source impl used to verify the trait shape compiles
    // and that `list` actually receives the typed Query.

    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Issue {
        id: String,
        workspace_id: String,
    }

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct IssueDraft {
        workspace_id: String,
    }

    struct StubSource;

    impl Source for StubSource {
        type Id = String;
        type Row = Issue;
        type Draft = IssueDraft;

        fn list<'a>(
            &'a self,
            _ctx: pocopine_auth::RequestContext,
            query: &'a Query<Self::Row>,
        ) -> SourceFuture<'a, SyncResult<Vec<Self::Row>>> {
            // The test below asserts the query reached us with the
            // expected wire params — proves the wire reconstruction
            // works end-to-end. We return rows tagged with whatever
            // workspace_id we received so the assertion can read it
            // back.
            let ws = query
                .params()
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                Ok(vec![Issue {
                    id: "stub_1".into(),
                    workspace_id: ws,
                }])
            })
        }

        fn get<'a>(
            &'a self,
            _ctx: pocopine_auth::RequestContext,
            _id: Self::Id,
        ) -> SourceFuture<'a, SyncResult<Option<Self::Row>>> {
            Box::pin(async move { Ok(None) })
        }

        fn create<'a>(
            &'a self,
            _ctx: pocopine_auth::RequestContext,
            _id: Self::Id,
            _draft: Self::Draft,
        ) -> SourceFuture<'a, SyncResult<Self::Row>> {
            Box::pin(async move { Err(SyncError::unsupported("test stub: create")) })
        }

        fn update<'a>(
            &'a self,
            _ctx: pocopine_auth::RequestContext,
            _id: Self::Id,
            _draft: Self::Draft,
            _expected_version: Option<RowVersion>,
        ) -> SourceFuture<'a, SyncResult<WriteResult<Self::Row>>> {
            Box::pin(async move { Err(SyncError::unsupported("stub: update")) })
        }

        fn delete<'a>(
            &'a self,
            _ctx: pocopine_auth::RequestContext,
            _id: Self::Id,
            _expected_version: Option<RowVersion>,
        ) -> SourceFuture<'a, SyncResult<DeleteResult<Self::Row>>> {
            Box::pin(async move { Err(SyncError::unsupported("stub: delete")) })
        }
    }

    #[test]
    fn query_from_pull_request_round_trips_params() {
        // The cross-language wire contract for the Source trait: the
        // server reconstructs Query<Row> from wire params. A
        // round-trip regression here would silently break server-
        // side filtering.
        let mut request =
            pocopine_sync::SyncPullRequest::new(SyncStreamName::new("issues").unwrap());
        request
            .params
            .insert("workspace_id".to_string(), serde_json::json!("W1"));
        request.params.insert(
            "status".to_string(),
            serde_json::json!({"in": ["open", "in_progress"]}),
        );
        request.limit = 50;

        let q: Query<Issue> = query_from_pull_request(&request);
        assert_eq!(q.stream().as_str(), "issues");
        assert_eq!(q.params().len(), 2);
        assert_eq!(
            q.params().get("workspace_id").unwrap(),
            &serde_json::json!("W1")
        );
        assert_eq!(q.limit(), Some(50));
    }

    #[test]
    fn query_from_pull_request_zero_limit_means_unset() {
        // The wire protocol's `limit: u32` field uses 0 to mean
        // "use the source's default". Make sure the reconstructed
        // Query reflects that as `limit().is_none()` so sources can
        // pattern-match without thinking about the sentinel.
        let mut request =
            pocopine_sync::SyncPullRequest::new(SyncStreamName::new("issues").unwrap());
        request.limit = 0;
        let q: Query<Issue> = query_from_pull_request(&request);
        assert_eq!(q.limit(), None);
    }

    #[tokio::test]
    async fn stub_source_receives_query_params_via_list() {
        // Verifies the Source trait's `list(ctx, &Query<Row>)` shape
        // works: a wire-reconstructed query passes its params through
        // to the source, which can introspect them and return
        // filtered rows. This is the basic Phase 1 contract.
        let source = StubSource;
        let mut request =
            pocopine_sync::SyncPullRequest::new(SyncStreamName::new("issues").unwrap());
        request
            .params
            .insert("workspace_id".to_string(), serde_json::json!("W42"));
        let query: Query<Issue> = query_from_pull_request(&request);

        let ctx = test_ctx();
        let rows = source.list(ctx, &query).await.expect("stub list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].workspace_id, "W42");
    }
}
