use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pocopine_auth::RequestContext;
use pocopine_sync::{
    default_schema_version_one, MutationId, RowKey, RowVersion, SyncBoxFuture, SyncCollectionName,
    SyncConflict, SyncError, SyncOp, SyncPullRequest, SyncPullResponse, SyncPushRequest,
    SyncPushResponse, SyncRejectedMutation, SyncResult, SyncRow, SyncStreamName, SyncStreamSource,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    CrudMutationPayload, CrudRemoveResult, CrudSource, CrudTransactionRunner, CrudWriteResult,
    ResourceId, TransactionalCrudSource,
};

/// Default maximum rows returned by one CRUD snapshot pull.
pub const DEFAULT_CRUD_SNAPSHOT_ROW_LIMIT: usize = 1_000;

/// Start building a CRUD-backed sync stream.
///
/// The `name` is used as both the sync stream and collection name. The builder
/// does not generate SQL; it only wires a [`CrudSource`] into Pocopine sync.
pub fn resource<S>(name: impl Into<String>, source: S) -> SyncResult<CrudResourceBuilder<S>>
where
    S: CrudSource,
{
    let name = name.into();
    Ok(CrudResourceBuilder {
        stream: SyncStreamName::new(name.clone())?,
        collection: SyncCollectionName::new(name)?,
        source,
        max_snapshot_rows: DEFAULT_CRUD_SNAPSHOT_ROW_LIMIT,
        // Pull the default from one source-of-truth: future changes to
        // the wire/trait default (e.g. switching to an `Option<u32>`
        // 'unspecified' sentinel) propagate here without a separate edit.
        schema_version: default_schema_version_one(),
        // No registered migrator by default — the trait default impl
        // of `SyncStreamSource::migrate_payload` returns
        // `SyncError::SchemaMigration` so stale-schema pushes get a
        // per-mutation rejection.
        migrate_payload: None,
    })
}

/// User-supplied function that migrates a stale-schema mutation
/// payload up to the source's current schema version. Registered via
/// [`CrudResourceBuilder::migrate_with`] (or the `#[resource(...,
/// migrate_with = path)]` macro attribute) and called by
/// [`SyncStreamSource::migrate_payload`] inside the server's
/// `push_handler` before delegating to the source's `push`.
///
/// On `Ok(value)`, the migrated payload replaces the original.
/// On `Err(SyncError::SchemaMigration { .. })`, the framework adds
/// the mutation to the response's `rejected` array with the error's
/// `reason`; the rest of the batch continues.
pub type CrudMigrateFn = dyn Fn(u32, u32, Value) -> SyncResult<Value> + Send + Sync + 'static;

/// Builder returned by [`resource`].
pub struct CrudResourceBuilder<S> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    source: S,
    max_snapshot_rows: usize,
    schema_version: u32,
    migrate_payload: Option<Arc<CrudMigrateFn>>,
}

impl<S> CrudResourceBuilder<S>
where
    S: CrudSource,
{
    /// Set the maximum rows a snapshot pull may return.
    pub fn max_snapshot_rows(mut self, limit: usize) -> SyncResult<Self> {
        if limit == 0 {
            return Err(SyncError::client(
                "CRUD max snapshot rows must be greater than zero",
            ));
        }
        self.max_snapshot_rows = limit;
        Ok(self)
    }

    /// Set the application-level schema version this resource serves.
    ///
    /// Bump this whenever the row, draft, or payload shape changes in a
    /// way that older cached data or queued mutations cannot deserialize
    /// into. Defaults to `1`. The server advertises this on every `/open`
    /// response; clients compare against their cached value and clear
    /// the stream when they differ. `0` is rejected — versions start at
    /// `1`.
    pub fn schema_version(mut self, version: u32) -> SyncResult<Self> {
        if version == 0 {
            return Err(SyncError::client(
                "schema_version must be >= 1 (schema versions start at 1)",
            ));
        }
        self.schema_version = version;
        Ok(self)
    }

    /// Register a function that migrates a stale-schema mutation
    /// payload to the resource's current schema version.
    ///
    /// The opt-in escape hatch for apps that must preserve queued
    /// mutations across a `schema_version` bump (e.g. payments,
    /// inventory writes — places where dropping queued local edits
    /// is unacceptable). The function is called by the server's
    /// `push_handler` once per mutation when the request's
    /// `schema_version` differs from the resource's current version;
    /// migrated payloads continue into `source.push`, errors land in
    /// the response's `rejected` array.
    ///
    /// The default behaviour (no `migrate_with` registered) returns
    /// `SyncError::SchemaMigration`, which the framework surfaces as
    /// a per-mutation rejection. This matches Replicache's default
    /// drop-the-queue posture; `migrate_with` is the opt-in
    /// "transform instead of reject" path.
    pub fn migrate_with<F>(mut self, migrate: F) -> Self
    where
        F: Fn(u32, u32, Value) -> SyncResult<Value> + Send + Sync + 'static,
    {
        self.migrate_payload = Some(Arc::new(migrate));
        self
    }

    /// Attach the row id extractor for this resource.
    pub fn id<IdOf>(self, id_of: IdOf) -> CrudResource<S, IdOf>
    where
        IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    {
        CrudResource {
            stream: self.stream,
            collection: self.collection,
            source: self.source,
            max_snapshot_rows: self.max_snapshot_rows,
            schema_version: self.schema_version,
            migrate_payload: self.migrate_payload,
            id_of,
            version_of: NoRowVersion,
            mutation_log: MissingMutationLog,
        }
    }
}

/// CRUD resource adapter that can be registered with `pocopine-sync`.
pub struct CrudResource<S, IdOf, VersionOf = NoRowVersion, Log = MissingMutationLog> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    source: S,
    max_snapshot_rows: usize,
    schema_version: u32,
    migrate_payload: Option<Arc<CrudMigrateFn>>,
    id_of: IdOf,
    version_of: VersionOf,
    mutation_log: Log,
}

impl<S, IdOf, VersionOf, Log> CrudResource<S, IdOf, VersionOf, Log>
where
    S: CrudSource,
{
    /// Attach a row version extractor used for base-version conflict checks.
    pub fn version<NextVersionOf, Version>(
        self,
        version_of: NextVersionOf,
    ) -> CrudResource<S, IdOf, NextVersionOf, Log>
    where
        NextVersionOf: Fn(&S::Row) -> Version + Send + Sync + 'static,
        Version: RowVersionValue,
    {
        CrudResource {
            stream: self.stream,
            collection: self.collection,
            source: self.source,
            max_snapshot_rows: self.max_snapshot_rows,
            schema_version: self.schema_version,
            migrate_payload: self.migrate_payload.clone(),
            id_of: self.id_of,
            version_of,
            mutation_log: self.mutation_log,
        }
    }
}

impl<S, IdOf, VersionOf> CrudResource<S, IdOf, VersionOf, MissingMutationLog>
where
    S: CrudSource,
{
    /// Attach a mutation log used to dedupe replayed accepted mutations.
    ///
    /// Production logs should be backed by the same database as the source and
    /// recorded in the same transaction as the row write. The non-macro
    /// adapter exposes the pieces explicitly; generated database adapters are
    /// expected to bind the row write and log insert into one transaction.
    pub fn mutation_log<Log>(self, mutation_log: Log) -> CrudResource<S, IdOf, VersionOf, Log>
    where
        Log: CrudMutationLog<S::Row>,
    {
        CrudResource {
            stream: self.stream,
            collection: self.collection,
            source: self.source,
            max_snapshot_rows: self.max_snapshot_rows,
            schema_version: self.schema_version,
            migrate_payload: self.migrate_payload.clone(),
            id_of: self.id_of,
            version_of: self.version_of,
            mutation_log,
        }
    }

    /// Attach a process-local mutation log for tests and single-process demos.
    ///
    /// This is not a production idempotency backend because it is not durable.
    pub fn memory_mutation_log(
        self,
    ) -> CrudResource<S, IdOf, VersionOf, MemoryCrudMutationLog<S::Row>> {
        self.mutation_log(MemoryCrudMutationLog::new())
    }

    /// Attach a transaction runner and mutation log for atomic server writes.
    ///
    /// This path runs each accepted CRUD mutation in one app/database
    /// transaction: reserve the accepted-mutation id, apply the source write,
    /// then commit both together. Use this for production resources where the
    /// source write and idempotency reservation must not split across separate
    /// commits.
    pub fn transactional<Runner, Log>(
        self,
        transaction_runner: Runner,
        mutation_log: Log,
    ) -> TransactionalCrudResource<S, IdOf, VersionOf, Log, Runner>
    where
        Runner: CrudTransactionRunner,
        S: TransactionalCrudSource<Runner::Tx>,
        Log: TransactionalCrudMutationLog<Runner::Tx, S::Row>,
    {
        TransactionalCrudResource {
            stream: self.stream,
            collection: self.collection,
            source: self.source,
            max_snapshot_rows: self.max_snapshot_rows,
            schema_version: self.schema_version,
            migrate_payload: self.migrate_payload.clone(),
            id_of: self.id_of,
            version_of: self.version_of,
            mutation_log,
            transaction_runner,
        }
    }
}

/// CRUD resource adapter that applies writes and idempotency log inserts in one
/// app/database transaction.
pub struct TransactionalCrudResource<S, IdOf, VersionOf, Log, Runner> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    source: S,
    max_snapshot_rows: usize,
    schema_version: u32,
    migrate_payload: Option<Arc<CrudMigrateFn>>,
    id_of: IdOf,
    version_of: VersionOf,
    mutation_log: Log,
    transaction_runner: Runner,
}

/// Marker used before a resource has an idempotency backend.
pub struct MissingMutationLog;

/// Marker used when a resource does not expose row versions.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRowVersion;

/// Convert a row version extractor value into an optional sync row version.
pub trait RowVersionValue {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>>;
}

impl RowVersionValue for RowVersion {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        Ok(Some(self))
    }
}

impl RowVersionValue for Option<RowVersion> {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        Ok(self)
    }
}

impl RowVersionValue for String {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        if self.is_empty() {
            Ok(None)
        } else {
            Ok(Some(RowVersion::new(self)?))
        }
    }
}

impl RowVersionValue for Option<String> {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        self.filter(|version| !version.is_empty())
            .map(RowVersion::new)
            .transpose()
    }
}

impl RowVersionValue for &str {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        if self.is_empty() {
            Ok(None)
        } else {
            Ok(Some(RowVersion::new(self)?))
        }
    }
}

impl RowVersionValue for Option<&str> {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        self.filter(|version| !version.is_empty())
            .map(RowVersion::new)
            .transpose()
    }
}

macro_rules! integer_row_version {
    ($($ty:ty),* $(,)?) => {
        $(
            impl RowVersionValue for $ty {
                fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
                    Ok(Some(RowVersion::new(self.to_string())?))
                }
            }

            impl RowVersionValue for Option<$ty> {
                fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
                    self.map(|version| RowVersion::new(version.to_string())).transpose()
                }
            }
        )*
    };
}

integer_row_version!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

/// Row-version extractor used by [`CrudResource::version`].
///
/// Users normally pass a closure to [`CrudResource::version`] and never
/// implement this trait by hand. It is public because the configured resource
/// type mentions it in its generic bounds.
pub trait RowVersionOf<Row>: Send + Sync + 'static {
    /// Return the row version to expose in sync responses.
    fn row_version(&self, row: &Row) -> SyncResult<Option<RowVersion>>;
}

impl<Row> RowVersionOf<Row> for NoRowVersion {
    fn row_version(&self, row: &Row) -> SyncResult<Option<RowVersion>> {
        let _ = row;
        Ok(None)
    }
}

impl<Row, F, V> RowVersionOf<Row> for F
where
    F: Fn(&Row) -> V + Send + Sync + 'static,
    V: RowVersionValue,
{
    fn row_version(&self, row: &Row) -> SyncResult<Option<RowVersion>> {
        (self)(row).into_row_version()
    }
}

/// Accepted mutation entry stored by a [`CrudMutationLog`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrudAcceptedMutation {
    pub mutation_id: MutationId,
    pub op: SyncOp,
    pub key: Option<RowKey>,
    pub payload: Value,
}

impl CrudAcceptedMutation {
    pub fn new(mutation_id: MutationId, op: SyncOp, key: Option<RowKey>, payload: Value) -> Self {
        Self {
            mutation_id,
            op,
            key,
            payload,
        }
    }

    fn matches(&self, op: SyncOp, key: Option<&RowKey>, payload: &Value) -> bool {
        self.op == op && self.key.as_ref() == key && self.payload == *payload
    }
}

/// Result of reserving an accepted mutation id inside a transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CrudMutationReservation {
    /// The caller reserved this mutation id and should continue applying the
    /// source write in the same transaction.
    Reserved,
    /// The mutation id already exists in the accepted log.
    AlreadyAccepted(CrudAcceptedMutation),
}

/// Idempotency log for CRUD mutations.
///
/// Production implementations must scope lookups to the same authorization
/// domain as the underlying source, for example `(tenant_id, mutation_id)`.
/// The adapter intentionally does not return cached rows on dedupe hits; it
/// only acknowledges exact replay of the same accepted mutation.
#[async_trait::async_trait]
pub trait CrudMutationLog<Row>: Send + Sync + 'static
where
    Row: Clone + Send + Sync + 'static,
{
    async fn accepted_mutation(
        &self,
        ctx: &RequestContext,
        mutation_id: &MutationId,
    ) -> SyncResult<Option<CrudAcceptedMutation>>;

    async fn record_accepted_mutation(
        &self,
        ctx: &RequestContext,
        accepted: CrudAcceptedMutation,
    ) -> SyncResult<()>;
}

/// Transaction-aware idempotency log for CRUD mutations.
///
/// Production implementations should use the same transaction handle and
/// authorization scope as the corresponding [`TransactionalCrudSource`].
/// The reservation method must be the concurrency boundary: production
/// implementations should insert first under a unique key for the same scope
/// and mutation id, then return the existing accepted mutation on duplicate.
/// This prevents concurrent retries from both observing "missing" before the
/// source write.
#[async_trait::async_trait]
pub trait TransactionalCrudMutationLog<Tx, Row>: Send + Sync + 'static
where
    Tx: Send,
    Row: Clone + Send + Sync + 'static,
{
    async fn reserve_accepted_mutation_in_tx(
        &self,
        tx: &mut Tx,
        ctx: &RequestContext,
        accepted: CrudAcceptedMutation,
    ) -> SyncResult<CrudMutationReservation>;
}

/// Function used by `MemoryCrudMutationLog::with_scope_fn` to project an
/// authorization scope (e.g. a tenant id) out of the per-request context.
pub type MemoryCrudScopeFn =
    Arc<dyn Fn(&RequestContext) -> SyncResult<String> + Send + Sync + 'static>;

/// Process-local mutation log for tests and single-process demos.
///
/// The default constructor uses one global scope — accepted mutations across
/// every tenant share one keyspace. Multi-tenant resources should use
/// [`MemoryCrudMutationLog::with_scope_fn`] to derive the authorization scope
/// from the request context; this mirrors `SqlxCrudMutationLog`'s scoping
/// model so the same boundary that protects production logs is what the tests
/// exercise.
#[derive(Clone)]
pub struct MemoryCrudMutationLog<Row> {
    accepted: Arc<Mutex<BTreeMap<(String, String), CrudAcceptedMutation>>>,
    scope: MemoryCrudScopeFn,
    _marker: std::marker::PhantomData<fn() -> Row>,
}

impl<Row> std::fmt::Debug for MemoryCrudMutationLog<Row> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryCrudMutationLog")
            .finish_non_exhaustive()
    }
}

impl<Row> Default for MemoryCrudMutationLog<Row> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Row> MemoryCrudMutationLog<Row> {
    /// Build a single-scope memory log. Every accepted mutation lives in one
    /// global keyspace; use [`with_scope_fn`](Self::with_scope_fn) for
    /// multi-tenant tests.
    pub fn new() -> Self {
        Self {
            accepted: Arc::new(Mutex::new(BTreeMap::new())),
            scope: Arc::new(|_ctx| Ok(String::new())),
            _marker: std::marker::PhantomData,
        }
    }

    /// Build a memory log that derives its authorization scope from the
    /// request context — the same shape as
    /// [`SqlxCrudMutationLog::new`](https://docs.rs/pocopine-sync-sqlx).
    ///
    /// The scope function is invoked on every `accepted_mutation` lookup and
    /// every `record_accepted_mutation`. Returning an error from the function
    /// fails the log call (typically surfaced as `SyncError::Unauthorized`
    /// from the request handler).
    pub fn with_scope_fn<F>(scope: F) -> Self
    where
        F: Fn(&RequestContext) -> SyncResult<String> + Send + Sync + 'static,
    {
        Self {
            accepted: Arc::new(Mutex::new(BTreeMap::new())),
            scope: Arc::new(scope),
            _marker: std::marker::PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<Row> CrudMutationLog<Row> for MemoryCrudMutationLog<Row>
where
    Row: Clone + Send + Sync + 'static,
{
    async fn accepted_mutation(
        &self,
        ctx: &RequestContext,
        mutation_id: &MutationId,
    ) -> SyncResult<Option<CrudAcceptedMutation>> {
        let scope = (self.scope)(ctx)?;
        let accepted = self
            .accepted
            .lock()
            .map_err(|_| SyncError::backend("memory CRUD mutation log lock poisoned"))?;
        Ok(accepted
            .get(&(scope, mutation_id.as_str().to_string()))
            .cloned())
    }

    async fn record_accepted_mutation(
        &self,
        ctx: &RequestContext,
        accepted: CrudAcceptedMutation,
    ) -> SyncResult<()> {
        let scope = (self.scope)(ctx)?;
        let mut entries = self
            .accepted
            .lock()
            .map_err(|_| SyncError::backend("memory CRUD mutation log lock poisoned"))?;
        entries.insert((scope, accepted.mutation_id.as_str().to_string()), accepted);
        Ok(())
    }
}

impl<S, IdOf, VersionOf, Log> SyncStreamSource for CrudResource<S, IdOf, VersionOf, Log>
where
    S: CrudSource,
    IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    VersionOf: RowVersionOf<S::Row>,
    Log: CrudMutationLog<S::Row>,
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

    fn migrate_payload<'a>(&'a self, from: u32, to: u32, value: Value) -> SyncBoxFuture<'a, Value> {
        if let Some(migrate) = self.migrate_payload.clone() {
            Box::pin(async move { migrate(from, to, value) })
        } else {
            let stream = self.stream.as_str().to_string();
            Box::pin(async move { Err(SyncError::schema_migration(stream, from, to)) })
        }
    }

    fn pull<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncBoxFuture<'a, SyncPullResponse<Value>> {
        Box::pin(async move { self.pull_snapshot(ctx, request).await })
    }

    fn push<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'a, SyncPushResponse<Value>> {
        Box::pin(async move { self.push_mutations(ctx, request).await })
    }
}

impl<S, IdOf, VersionOf, Log> CrudResource<S, IdOf, VersionOf, Log>
where
    S: CrudSource,
    IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    VersionOf: RowVersionOf<S::Row>,
    Log: CrudMutationLog<S::Row>,
{
    async fn pull_snapshot(
        &self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncResult<SyncPullResponse<Value>> {
        if request.stream != self.stream {
            return Err(SyncError::UnknownStream(request.stream.to_string()));
        }

        let rows = self.source.list(ctx, self.max_snapshot_rows).await?;
        if rows.len() > self.max_snapshot_rows {
            return Err(SyncError::backend(format!(
                "CRUD source returned {} rows, exceeding max snapshot row limit {}",
                rows.len(),
                self.max_snapshot_rows
            )));
        }
        let rows = rows
            .into_iter()
            .map(|row| self.row_to_value(row))
            .collect::<SyncResult<Vec<_>>>()?;

        Ok(SyncPullResponse::snapshot(
            self.stream.clone(),
            self.collection.clone(),
            rows,
            None,
        ))
    }

    async fn push_mutations(
        &self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncResult<SyncPushResponse<Value>> {
        if request.stream != self.stream {
            return Err(SyncError::UnknownStream(request.stream.to_string()));
        }

        let mut response = SyncPushResponse::new(self.stream.clone());
        response.collection = Some(self.collection.clone());

        for mutation in request.mutations {
            let mutation_id = mutation.id.clone();
            let key = mutation.key.clone();
            let op = mutation.op;
            let base_version = mutation.base_version;
            let payload_value = mutation.payload;

            if let Some(accepted) = self
                .mutation_log
                .accepted_mutation(&ctx, &mutation_id)
                .await?
            {
                if accepted.matches(op, key.as_ref(), &payload_value) {
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

            let payload = match serde_json::from_value::<CrudMutationPayload<S::Id, S::Draft>>(
                payload_value.clone(),
            ) {
                Ok(payload) => payload,
                Err(err) => {
                    response.rejected.push(SyncRejectedMutation {
                        mutation_id,
                        key,
                        reason: format!("invalid CRUD mutation payload: {err}"),
                    });
                    continue;
                }
            };

            if payload.sync_op() != op {
                response.rejected.push(SyncRejectedMutation {
                    mutation_id,
                    key,
                    reason: "CRUD payload does not match sync operation".to_string(),
                });
                continue;
            }

            let expected_key = payload.id().to_row_key()?;
            if key.as_ref() != Some(&expected_key) {
                response.rejected.push(SyncRejectedMutation {
                    mutation_id,
                    key,
                    reason: "CRUD mutation row key does not match payload id".to_string(),
                });
                continue;
            }

            match self
                .apply_payload(
                    &ctx,
                    mutation_id,
                    expected_key.clone(),
                    base_version,
                    payload,
                )
                .await?
            {
                CrudApplyOutcome::Accepted { mutation_id, row } => {
                    self.mutation_log
                        .record_accepted_mutation(
                            &ctx,
                            CrudAcceptedMutation::new(
                                mutation_id.clone(),
                                op,
                                Some(expected_key),
                                payload_value,
                            ),
                        )
                        .await?;
                    response.accepted.push(mutation_id);
                    if let Some(row) = row {
                        response.rows.push(row_to_value(row)?);
                    }
                }
                CrudApplyOutcome::Rejected(rejected) => response.rejected.push(rejected),
                CrudApplyOutcome::Conflict(conflict) => {
                    response.conflicts.push(conflict_to_value(conflict)?);
                }
            }
        }

        Ok(response)
    }

    async fn apply_payload(
        &self,
        ctx: &RequestContext,
        mutation_id: MutationId,
        key: pocopine_sync::RowKey,
        base_version: Option<RowVersion>,
        payload: CrudMutationPayload<S::Id, S::Draft>,
    ) -> SyncResult<CrudApplyOutcome<S::Row>> {
        match payload {
            CrudMutationPayload::Create(payload) => {
                if base_version.is_some() {
                    return Ok(CrudApplyOutcome::Rejected(SyncRejectedMutation {
                        mutation_id,
                        key: Some(key),
                        reason: "create does not accept a base row version".to_string(),
                    }));
                }

                let row = self
                    .source
                    .create(ctx.clone(), payload.id, payload.draft)
                    .await?;
                let row = self.row_to_sync(row)?;
                Ok(CrudApplyOutcome::Accepted {
                    mutation_id,
                    row: Some(row),
                })
            }
            CrudMutationPayload::Save(payload) => {
                let outcome = self
                    .source
                    .save(ctx.clone(), payload.id, payload.draft, base_version)
                    .await?;
                match outcome {
                    CrudWriteResult::Applied(row) => {
                        let row = self.row_to_sync(row)?;
                        Ok(CrudApplyOutcome::Accepted {
                            mutation_id,
                            row: Some(row),
                        })
                    }
                    CrudWriteResult::Conflict(conflict) => Ok(CrudApplyOutcome::Conflict(
                        self.source_conflict(mutation_id, Some(key), conflict)?,
                    )),
                }
            }
            CrudMutationPayload::Remove(payload) => {
                let outcome = self
                    .source
                    .remove(ctx.clone(), payload.id, base_version)
                    .await?;
                match outcome {
                    CrudRemoveResult::Applied => Ok(CrudApplyOutcome::Accepted {
                        mutation_id,
                        row: None,
                    }),
                    CrudRemoveResult::Conflict(conflict) => Ok(CrudApplyOutcome::Conflict(
                        self.source_conflict(mutation_id, Some(key), conflict)?,
                    )),
                }
            }
        }
    }

    fn source_conflict(
        &self,
        mutation_id: MutationId,
        key: Option<pocopine_sync::RowKey>,
        conflict: crate::CrudConflict<S::Row>,
    ) -> SyncResult<SyncConflict<S::Row>> {
        Ok(SyncConflict {
            mutation_id,
            key,
            server_row: conflict
                .server_row
                .map(|row| self.row_to_sync(row))
                .transpose()?,
            reason: conflict.reason,
        })
    }

    fn row_to_sync(&self, row: S::Row) -> SyncResult<SyncRow<S::Row>> {
        let key = (self.id_of)(&row).to_row_key()?;
        let version = self.version_of.row_version(&row)?;
        Ok(SyncRow {
            key,
            version,
            value: row,
            pending: false,
            conflict: false,
        })
    }

    fn row_to_value(&self, row: S::Row) -> SyncResult<SyncRow<Value>> {
        row_to_value(self.row_to_sync(row)?)
    }
}

impl<S, IdOf, VersionOf, Log, Runner> SyncStreamSource
    for TransactionalCrudResource<S, IdOf, VersionOf, Log, Runner>
where
    S: TransactionalCrudSource<Runner::Tx>,
    IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    VersionOf: RowVersionOf<S::Row>,
    Log: TransactionalCrudMutationLog<Runner::Tx, S::Row>,
    Runner: CrudTransactionRunner,
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

    fn migrate_payload<'a>(&'a self, from: u32, to: u32, value: Value) -> SyncBoxFuture<'a, Value> {
        if let Some(migrate) = self.migrate_payload.clone() {
            Box::pin(async move { migrate(from, to, value) })
        } else {
            let stream = self.stream.as_str().to_string();
            Box::pin(async move { Err(SyncError::schema_migration(stream, from, to)) })
        }
    }

    fn pull<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncBoxFuture<'a, SyncPullResponse<Value>> {
        Box::pin(async move { self.pull_snapshot(ctx, request).await })
    }

    fn push<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'a, SyncPushResponse<Value>> {
        Box::pin(async move { self.push_mutations(ctx, request).await })
    }
}

impl<S, IdOf, VersionOf, Log, Runner> TransactionalCrudResource<S, IdOf, VersionOf, Log, Runner>
where
    S: TransactionalCrudSource<Runner::Tx>,
    IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    VersionOf: RowVersionOf<S::Row>,
    Log: TransactionalCrudMutationLog<Runner::Tx, S::Row>,
    Runner: CrudTransactionRunner,
{
    async fn pull_snapshot(
        &self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncResult<SyncPullResponse<Value>> {
        if request.stream != self.stream {
            return Err(SyncError::UnknownStream(request.stream.to_string()));
        }

        let rows = self.source.list(ctx, self.max_snapshot_rows).await?;
        if rows.len() > self.max_snapshot_rows {
            return Err(SyncError::backend(format!(
                "CRUD source returned {} rows, exceeding max snapshot row limit {}",
                rows.len(),
                self.max_snapshot_rows
            )));
        }
        let rows = rows
            .into_iter()
            .map(|row| self.row_to_value(row))
            .collect::<SyncResult<Vec<_>>>()?;

        Ok(SyncPullResponse::snapshot(
            self.stream.clone(),
            self.collection.clone(),
            rows,
            None,
        ))
    }

    async fn push_mutations(
        &self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncResult<SyncPushResponse<Value>> {
        if request.stream != self.stream {
            return Err(SyncError::UnknownStream(request.stream.to_string()));
        }

        let mut response = SyncPushResponse::new(self.stream.clone());
        response.collection = Some(self.collection.clone());

        for mutation in request.mutations {
            let mutation_id = mutation.id.clone();
            let key = mutation.key.clone();
            let op = mutation.op;
            let base_version = mutation.base_version;
            let payload_value = mutation.payload;

            let payload = match serde_json::from_value::<CrudMutationPayload<S::Id, S::Draft>>(
                payload_value.clone(),
            ) {
                Ok(payload) => payload,
                Err(err) => {
                    response.rejected.push(SyncRejectedMutation {
                        mutation_id,
                        key,
                        reason: format!("invalid CRUD mutation payload: {err}"),
                    });
                    continue;
                }
            };

            if payload.sync_op() != op {
                response.rejected.push(SyncRejectedMutation {
                    mutation_id,
                    key,
                    reason: "CRUD payload does not match sync operation".to_string(),
                });
                continue;
            }

            let expected_key = payload.id().to_row_key()?;
            if key.as_ref() != Some(&expected_key) {
                response.rejected.push(SyncRejectedMutation {
                    mutation_id,
                    key,
                    reason: "CRUD mutation row key does not match payload id".to_string(),
                });
                continue;
            }

            match self
                .apply_transactional_payload(
                    &ctx,
                    TransactionalCrudMutation {
                        mutation_id,
                        key: expected_key,
                        op,
                        base_version,
                        payload_value,
                        payload,
                    },
                )
                .await?
            {
                CrudApplyOutcome::Accepted { mutation_id, row } => {
                    response.accepted.push(mutation_id);
                    if let Some(row) = row {
                        response.rows.push(row_to_value(row)?);
                    }
                }
                CrudApplyOutcome::Rejected(rejected) => response.rejected.push(rejected),
                CrudApplyOutcome::Conflict(conflict) => {
                    response.conflicts.push(conflict_to_value(conflict)?);
                }
            }
        }

        Ok(response)
    }

    async fn apply_transactional_payload(
        &self,
        ctx: &RequestContext,
        mutation: TransactionalCrudMutation<S::Id, S::Draft>,
    ) -> SyncResult<CrudApplyOutcome<S::Row>> {
        let TransactionalCrudMutation {
            mutation_id,
            key,
            op,
            base_version,
            payload_value,
            payload,
        } = mutation;
        let mut tx = self.transaction_runner.begin().await?;
        let result = async {
            let accepted = CrudAcceptedMutation::new(
                mutation_id.clone(),
                op,
                Some(key.clone()),
                payload_value.clone(),
            );

            match self
                .mutation_log
                .reserve_accepted_mutation_in_tx(&mut tx, ctx, accepted)
                .await?
            {
                CrudMutationReservation::AlreadyAccepted(accepted) => {
                    if accepted.matches(op, Some(&key), &payload_value) {
                        return Ok(CrudTransactionDecision::Commit(
                            CrudApplyOutcome::Accepted {
                                mutation_id,
                                row: None,
                            },
                        ));
                    }

                    return Ok(CrudTransactionDecision::Commit(CrudApplyOutcome::Rejected(
                        SyncRejectedMutation {
                            mutation_id,
                            key: Some(key),
                            reason: "mutation id was already accepted with different contents"
                                .to_string(),
                        },
                    )));
                }
                CrudMutationReservation::Reserved => {}
            }

            let outcome = self
                .apply_payload_in_tx(&mut tx, ctx, mutation_id, key, base_version, payload)
                .await?;

            if matches!(outcome, CrudApplyOutcome::Accepted { .. }) {
                Ok(CrudTransactionDecision::Commit(outcome))
            } else {
                Ok(CrudTransactionDecision::Rollback(outcome))
            }
        }
        .await;

        match result {
            Ok(CrudTransactionDecision::Commit(outcome)) => {
                self.transaction_runner.commit(tx).await?;
                Ok(outcome)
            }
            Ok(CrudTransactionDecision::Rollback(outcome)) => {
                self.transaction_runner.rollback(tx).await?;
                Ok(outcome)
            }
            Err(err) => {
                if let Err(rollback_err) = self.transaction_runner.rollback(tx).await {
                    return Err(SyncError::backend(format!(
                        "CRUD transaction rollback failed after {err}: {rollback_err}"
                    )));
                }
                Err(err)
            }
        }
    }

    async fn apply_payload_in_tx(
        &self,
        tx: &mut Runner::Tx,
        ctx: &RequestContext,
        mutation_id: MutationId,
        key: RowKey,
        base_version: Option<RowVersion>,
        payload: CrudMutationPayload<S::Id, S::Draft>,
    ) -> SyncResult<CrudApplyOutcome<S::Row>> {
        match payload {
            CrudMutationPayload::Create(payload) => {
                if base_version.is_some() {
                    return Ok(CrudApplyOutcome::Rejected(SyncRejectedMutation {
                        mutation_id,
                        key: Some(key),
                        reason: "create does not accept a base row version".to_string(),
                    }));
                }

                let row = self
                    .source
                    .create_in_tx(tx, ctx, payload.id, payload.draft)
                    .await?;
                let row = self.row_to_sync(row)?;
                Ok(CrudApplyOutcome::Accepted {
                    mutation_id,
                    row: Some(row),
                })
            }
            CrudMutationPayload::Save(payload) => {
                let outcome = self
                    .source
                    .save_in_tx(tx, ctx, payload.id, payload.draft, base_version)
                    .await?;
                match outcome {
                    CrudWriteResult::Applied(row) => {
                        let row = self.row_to_sync(row)?;
                        Ok(CrudApplyOutcome::Accepted {
                            mutation_id,
                            row: Some(row),
                        })
                    }
                    CrudWriteResult::Conflict(conflict) => Ok(CrudApplyOutcome::Conflict(
                        self.source_conflict(mutation_id, Some(key), conflict)?,
                    )),
                }
            }
            CrudMutationPayload::Remove(payload) => {
                let outcome = self
                    .source
                    .remove_in_tx(tx, ctx, payload.id, base_version)
                    .await?;
                match outcome {
                    CrudRemoveResult::Applied => Ok(CrudApplyOutcome::Accepted {
                        mutation_id,
                        row: None,
                    }),
                    CrudRemoveResult::Conflict(conflict) => Ok(CrudApplyOutcome::Conflict(
                        self.source_conflict(mutation_id, Some(key), conflict)?,
                    )),
                }
            }
        }
    }

    fn source_conflict(
        &self,
        mutation_id: MutationId,
        key: Option<RowKey>,
        conflict: crate::CrudConflict<S::Row>,
    ) -> SyncResult<SyncConflict<S::Row>> {
        Ok(SyncConflict {
            mutation_id,
            key,
            server_row: conflict
                .server_row
                .map(|row| self.row_to_sync(row))
                .transpose()?,
            reason: conflict.reason,
        })
    }

    fn row_to_sync(&self, row: S::Row) -> SyncResult<SyncRow<S::Row>> {
        let key = (self.id_of)(&row).to_row_key()?;
        let version = self.version_of.row_version(&row)?;
        Ok(SyncRow {
            key,
            version,
            value: row,
            pending: false,
            conflict: false,
        })
    }

    fn row_to_value(&self, row: S::Row) -> SyncResult<SyncRow<Value>> {
        row_to_value(self.row_to_sync(row)?)
    }
}

enum CrudApplyOutcome<Row> {
    Accepted {
        mutation_id: MutationId,
        row: Option<SyncRow<Row>>,
    },
    Rejected(SyncRejectedMutation),
    Conflict(SyncConflict<Row>),
}

enum CrudTransactionDecision<Row> {
    Commit(CrudApplyOutcome<Row>),
    Rollback(CrudApplyOutcome<Row>),
}

struct TransactionalCrudMutation<Id, Draft> {
    mutation_id: MutationId,
    key: RowKey,
    op: SyncOp,
    base_version: Option<RowVersion>,
    payload_value: Value,
    payload: CrudMutationPayload<Id, Draft>,
}

fn row_to_value<Row>(row: SyncRow<Row>) -> SyncResult<SyncRow<Value>>
where
    Row: Serialize,
{
    Ok(SyncRow {
        key: row.key,
        version: row.version,
        value: serde_json::to_value(row.value)?,
        pending: row.pending,
        conflict: row.conflict,
    })
}

fn conflict_to_value<Row>(conflict: SyncConflict<Row>) -> SyncResult<SyncConflict<Value>>
where
    Row: Serialize,
{
    Ok(SyncConflict {
        mutation_id: conflict.mutation_id,
        key: conflict.key,
        server_row: conflict.server_row.map(row_to_value).transpose()?,
        reason: conflict.reason,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use http::{HeaderMap, Method, Uri};
    use pocopine_sync::{ClientMutation, SyncPullMode};
    use serde::{Deserialize, Serialize};

    use crate::TransactionFuture;

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Post {
        id: String,
        title: String,
        version: u64,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct PostDraft {
        title: String,
    }

    #[derive(Clone, Default)]
    struct Posts {
        rows: Arc<Mutex<BTreeMap<String, Post>>>,
        calls: Arc<Mutex<Vec<String>>>,
        ignore_list_limit: bool,
    }

    impl Posts {
        fn ignoring_list_limit() -> Self {
            Self {
                ignore_list_limit: true,
                ..Self::default()
            }
        }

        fn insert(&self, post: Post) {
            self.rows.lock().unwrap().insert(post.id.clone(), post);
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: impl Into<String>) {
            self.calls.lock().unwrap().push(call.into());
        }
    }

    #[async_trait::async_trait]
    impl CrudSource for Posts {
        type Id = String;
        type Row = Post;
        type Draft = PostDraft;

        async fn list(&self, ctx: RequestContext, limit: usize) -> SyncResult<Vec<Self::Row>> {
            let _ = ctx;
            let rows = self.rows.lock().unwrap();
            if self.ignore_list_limit {
                Ok(rows.values().cloned().collect())
            } else {
                Ok(rows.values().take(limit).cloned().collect())
            }
        }

        async fn get(&self, ctx: RequestContext, id: Self::Id) -> SyncResult<Option<Self::Row>> {
            let _ = ctx;
            Ok(self.rows.lock().unwrap().get(&id).cloned())
        }

        async fn create(
            &self,
            ctx: RequestContext,
            id: Self::Id,
            draft: Self::Draft,
        ) -> SyncResult<Self::Row> {
            let _ = ctx;
            self.record(format!("create:{id}"));
            let post = Post {
                id: id.clone(),
                title: draft.title,
                version: 1,
            };
            self.rows.lock().unwrap().insert(id, post.clone());
            Ok(post)
        }

        async fn save(
            &self,
            ctx: RequestContext,
            id: Self::Id,
            draft: Self::Draft,
            base_version: Option<RowVersion>,
        ) -> SyncResult<CrudWriteResult<Self::Row>> {
            let _ = ctx;
            let mut rows = self.rows.lock().unwrap();
            let row = rows
                .get_mut(&id)
                .ok_or_else(|| SyncError::backend("missing post"))?;
            if let Some(base_version) = &base_version {
                if base_version.as_str() != row.version.to_string() {
                    return Ok(CrudWriteResult::stale(Some(row.clone())));
                }
            }
            self.record(format!("save:{id}"));
            row.title = draft.title;
            row.version += 1;
            Ok(CrudWriteResult::applied(row.clone()))
        }

        async fn remove(
            &self,
            ctx: RequestContext,
            id: Self::Id,
            base_version: Option<RowVersion>,
        ) -> SyncResult<CrudRemoveResult<Self::Row>> {
            let _ = ctx;
            let mut rows = self.rows.lock().unwrap();
            if let Some(base_version) = base_version {
                let Some(row) = rows.get(&id) else {
                    return Ok(CrudRemoveResult::stale(None));
                };
                if base_version.as_str() != row.version.to_string() {
                    return Ok(CrudRemoveResult::stale(Some(row.clone())));
                }
            }
            self.record(format!("remove:{id}"));
            rows.remove(&id);
            Ok(CrudRemoveResult::applied())
        }
    }

    fn ctx() -> RequestContext {
        RequestContext::new(Method::GET, Uri::from_static("/sync"), HeaderMap::new())
    }

    fn posts_resource(
        posts: Posts,
    ) -> CrudResource<
        Posts,
        impl Fn(&Post) -> String + Send + Sync + 'static,
        impl Fn(&Post) -> u64 + Send + Sync + 'static,
        MemoryCrudMutationLog<Post>,
    > {
        resource("posts", posts)
            .unwrap()
            .id(|post: &Post| post.id.clone())
            .version(|post: &Post| post.version)
            .memory_mutation_log()
    }

    #[derive(Clone, Debug, Default)]
    struct TxState {
        rows: BTreeMap<String, Post>,
        accepted: BTreeMap<String, CrudAcceptedMutation>,
    }

    #[derive(Clone, Debug, Default)]
    struct TxFailures {
        source_write: bool,
        log_record: bool,
        rollback: bool,
    }

    #[derive(Clone, Debug, Default)]
    struct TxHarness {
        state: Arc<Mutex<TxState>>,
        events: Arc<Mutex<Vec<String>>>,
        failures: Arc<Mutex<TxFailures>>,
    }

    #[derive(Debug)]
    struct FakeTx {
        state: TxState,
    }

    impl TxHarness {
        fn insert(&self, post: Post) {
            self.state
                .lock()
                .unwrap()
                .rows
                .insert(post.id.clone(), post);
        }

        fn state(&self) -> TxState {
            self.state.lock().unwrap().clone()
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }

        fn clear_events(&self) {
            self.events.lock().unwrap().clear();
        }

        fn fail_source_write(&self) {
            self.failures.lock().unwrap().source_write = true;
        }

        fn fail_log_record(&self) {
            self.failures.lock().unwrap().log_record = true;
        }

        fn fail_rollback(&self) {
            self.failures.lock().unwrap().rollback = true;
        }

        fn record(&self, event: impl Into<String>) {
            self.events.lock().unwrap().push(event.into());
        }
    }

    impl CrudTransactionRunner for TxHarness {
        type Tx = FakeTx;

        fn begin<'runner>(&'runner self) -> TransactionFuture<'runner, Self::Tx> {
            Box::pin(async move {
                self.record("begin");
                Ok(FakeTx {
                    state: self.state(),
                })
            })
        }

        fn commit<'runner>(&'runner self, tx: Self::Tx) -> TransactionFuture<'runner, ()> {
            Box::pin(async move {
                self.record("commit");
                *self.state.lock().unwrap() = tx.state;
                Ok(())
            })
        }

        fn rollback<'runner>(&'runner self, tx: Self::Tx) -> TransactionFuture<'runner, ()> {
            Box::pin(async move {
                let _ = tx;
                self.record("rollback");
                if self.failures.lock().unwrap().rollback {
                    return Err(SyncError::backend("rollback failed"));
                }
                Ok(())
            })
        }
    }

    #[async_trait::async_trait]
    impl CrudSource for TxHarness {
        type Id = String;
        type Row = Post;
        type Draft = PostDraft;

        async fn list(&self, ctx: RequestContext, limit: usize) -> SyncResult<Vec<Self::Row>> {
            let _ = ctx;
            Ok(self
                .state
                .lock()
                .unwrap()
                .rows
                .values()
                .take(limit)
                .cloned()
                .collect())
        }

        async fn get(&self, ctx: RequestContext, id: Self::Id) -> SyncResult<Option<Self::Row>> {
            let _ = ctx;
            Ok(self.state.lock().unwrap().rows.get(&id).cloned())
        }

        async fn create(
            &self,
            ctx: RequestContext,
            id: Self::Id,
            draft: Self::Draft,
        ) -> SyncResult<Self::Row> {
            let _ = (ctx, id, draft);
            Err(SyncError::backend(
                "transactional resource called non-transactional create",
            ))
        }

        async fn save(
            &self,
            ctx: RequestContext,
            id: Self::Id,
            draft: Self::Draft,
            base_version: Option<RowVersion>,
        ) -> SyncResult<CrudWriteResult<Self::Row>> {
            let _ = (ctx, id, draft, base_version);
            Err(SyncError::backend(
                "transactional resource called non-transactional save",
            ))
        }

        async fn remove(
            &self,
            ctx: RequestContext,
            id: Self::Id,
            base_version: Option<RowVersion>,
        ) -> SyncResult<CrudRemoveResult<Self::Row>> {
            let _ = (ctx, id, base_version);
            Err(SyncError::backend(
                "transactional resource called non-transactional remove",
            ))
        }
    }

    #[async_trait::async_trait]
    impl TransactionalCrudSource<FakeTx> for TxHarness {
        async fn create_in_tx(
            &self,
            tx: &mut FakeTx,
            ctx: &RequestContext,
            id: Self::Id,
            draft: Self::Draft,
        ) -> SyncResult<Self::Row> {
            let _ = ctx;
            self.record(format!("source:create:{id}"));
            let post = Post {
                id: id.clone(),
                title: draft.title,
                version: 1,
            };
            tx.state.rows.insert(id, post.clone());
            if self.failures.lock().unwrap().source_write {
                return Err(SyncError::backend("source write failed"));
            }
            Ok(post)
        }

        async fn save_in_tx(
            &self,
            tx: &mut FakeTx,
            ctx: &RequestContext,
            id: Self::Id,
            draft: Self::Draft,
            base_version: Option<RowVersion>,
        ) -> SyncResult<CrudWriteResult<Self::Row>> {
            let _ = ctx;
            self.record(format!("source:save:{id}"));
            let row = tx
                .state
                .rows
                .get_mut(&id)
                .ok_or_else(|| SyncError::backend("missing post"))?;
            if let Some(base_version) = &base_version {
                if base_version.as_str() != row.version.to_string() {
                    return Ok(CrudWriteResult::stale(Some(row.clone())));
                }
            }
            row.title = draft.title;
            row.version += 1;
            if self.failures.lock().unwrap().source_write {
                return Err(SyncError::backend("source write failed"));
            }
            Ok(CrudWriteResult::applied(row.clone()))
        }

        async fn remove_in_tx(
            &self,
            tx: &mut FakeTx,
            ctx: &RequestContext,
            id: Self::Id,
            base_version: Option<RowVersion>,
        ) -> SyncResult<CrudRemoveResult<Self::Row>> {
            let _ = ctx;
            self.record(format!("source:remove:{id}"));
            if let Some(base_version) = base_version {
                let Some(row) = tx.state.rows.get(&id) else {
                    return Ok(CrudRemoveResult::stale(None));
                };
                if base_version.as_str() != row.version.to_string() {
                    return Ok(CrudRemoveResult::stale(Some(row.clone())));
                }
            }
            tx.state.rows.remove(&id);
            if self.failures.lock().unwrap().source_write {
                return Err(SyncError::backend("source write failed"));
            }
            Ok(CrudRemoveResult::applied())
        }
    }

    #[async_trait::async_trait]
    impl TransactionalCrudMutationLog<FakeTx, Post> for TxHarness {
        async fn reserve_accepted_mutation_in_tx(
            &self,
            tx: &mut FakeTx,
            ctx: &RequestContext,
            accepted: CrudAcceptedMutation,
        ) -> SyncResult<CrudMutationReservation> {
            let _ = ctx;
            self.record(format!("log:lookup:{}", accepted.mutation_id));
            if let Some(existing) = tx
                .state
                .accepted
                .get(accepted.mutation_id.as_str())
                .cloned()
            {
                return Ok(CrudMutationReservation::AlreadyAccepted(existing));
            }

            self.record(format!("log:record:{}", accepted.mutation_id));
            tx.state
                .accepted
                .insert(accepted.mutation_id.as_str().to_string(), accepted);
            if self.failures.lock().unwrap().log_record {
                return Err(SyncError::backend("mutation log failed"));
            }
            Ok(CrudMutationReservation::Reserved)
        }
    }

    fn transactional_posts_resource(
        harness: TxHarness,
    ) -> TransactionalCrudResource<
        TxHarness,
        impl Fn(&Post) -> String + Send + Sync + 'static,
        impl Fn(&Post) -> u64 + Send + Sync + 'static,
        TxHarness,
        TxHarness,
    > {
        resource("posts", harness.clone())
            .unwrap()
            .id(|post: &Post| post.id.clone())
            .version(|post: &Post| post.version)
            .transactional(harness.clone(), harness)
    }

    fn value_mutation(
        mutation: pocopine_sync::ClientMutation<CrudMutationPayload<String, PostDraft>>,
    ) -> pocopine_sync::ClientMutation<Value> {
        pocopine_sync::ClientMutation {
            id: mutation.id,
            key: mutation.key,
            op: mutation.op,
            base_version: mutation.base_version,
            payload: serde_json::to_value(mutation.payload).unwrap(),
        }
    }

    fn push_request(
        mutations: impl IntoIterator<
            Item = pocopine_sync::ClientMutation<CrudMutationPayload<String, PostDraft>>,
        >,
    ) -> SyncPushRequest<Value> {
        SyncPushRequest::new(
            SyncStreamName::new("posts").unwrap(),
            mutations.into_iter().map(value_mutation),
        )
    }

    #[test]
    fn resource_defaults_schema_version_to_one() {
        let r = posts_resource(Posts::default());
        // SyncStreamSource::schema_version flows through every wrapping
        // layer (builder -> .id -> .version -> .memory_mutation_log).
        assert_eq!(
            <CrudResource<_, _, _, _> as SyncStreamSource>::schema_version(&r),
            1
        );
    }

    #[test]
    fn resource_builder_propagates_explicit_schema_version() {
        // Set a non-default version, then walk the full wrapping chain
        // (.id -> .version -> .memory_mutation_log) and assert the
        // SyncStreamSource impl returns it. A regression that drops the
        // `schema_version: self.schema_version` propagation from any
        // single constructor would silently revert this to 1 — this
        // test pins the contract.
        let r = resource("posts", Posts::default())
            .unwrap()
            .schema_version(7)
            .unwrap()
            .id(|post: &Post| post.id.clone())
            .version(|post: &Post| post.version)
            .memory_mutation_log();
        assert_eq!(
            <CrudResource<_, _, _, _> as SyncStreamSource>::schema_version(&r),
            7
        );
    }

    #[test]
    fn transactional_resource_propagates_explicit_schema_version() {
        // Same end-to-end propagation test, but through the
        // transactional path (.transactional consumes a CrudResource).
        let harness = TxHarness::default();
        let r = resource("posts", harness.clone())
            .unwrap()
            .schema_version(11)
            .unwrap()
            .id(|post: &Post| post.id.clone())
            .version(|post: &Post| post.version)
            .transactional(harness.clone(), harness);
        assert_eq!(
            <TransactionalCrudResource<_, _, _, _, _> as SyncStreamSource>::schema_version(&r),
            11
        );
    }

    #[test]
    fn resource_builder_rejects_zero_schema_version() {
        let result = resource("posts", Posts::default())
            .unwrap()
            .schema_version(0);
        match result {
            Ok(_) => panic!("expected error for schema_version=0"),
            Err(err) => {
                // Matches the macro's parser-time error message exactly
                // so grep / docs / error-catalog tooling sees one
                // canonical wording for the same logical failure.
                assert!(
                    err.to_string().contains("schema_version must be >= 1"),
                    "unexpected error: {err}"
                );
            }
        }
    }

    #[tokio::test]
    async fn pull_returns_snapshot_rows() {
        let posts = Posts::default();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "hello".to_string(),
            version: 7,
        });
        let resource = posts_resource(posts);

        let response = resource
            .pull(
                ctx(),
                SyncPullRequest::new(SyncStreamName::new("posts").unwrap()),
            )
            .await
            .unwrap();

        assert_eq!(response.mode, SyncPullMode::Snapshot);
        assert_eq!(response.rows.len(), 1);
        assert_eq!(response.rows[0].key.as_str(), "post_1");
        assert_eq!(response.rows[0].version.as_ref().unwrap().as_str(), "7");
        assert_eq!(response.rows[0].value["title"], "hello");
    }

    #[tokio::test]
    async fn snapshot_pull_errors_if_source_exceeds_limit() {
        let posts = Posts::ignoring_list_limit();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "one".to_string(),
            version: 1,
        });
        posts.insert(Post {
            id: "post_2".to_string(),
            title: "two".to_string(),
            version: 1,
        });
        let resource = resource("posts", posts)
            .unwrap()
            .max_snapshot_rows(1)
            .unwrap()
            .id(|post: &Post| post.id.clone())
            .version(|post: &Post| post.version)
            .memory_mutation_log();

        let err = resource
            .pull(
                ctx(),
                SyncPullRequest::new(SyncStreamName::new("posts").unwrap()),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("exceeding max snapshot row limit"));
    }

    #[tokio::test]
    async fn push_routes_create_save_and_remove() {
        let posts = Posts::default();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "old".to_string(),
            version: 1,
        });
        let resource = posts_resource(posts.clone());

        let create = CrudMutationPayload::create(
            "post_2".to_string(),
            PostDraft {
                title: "created".to_string(),
            },
        );
        let save = CrudMutationPayload::save(
            "post_1".to_string(),
            PostDraft {
                title: "saved".to_string(),
            },
        );
        let remove: CrudMutationPayload<String, PostDraft> =
            CrudMutationPayload::remove("post_2".to_string());
        let mutations = vec![
            create
                .into_sync_draft()
                .unwrap()
                .with_id(MutationId::new("device_1:1").unwrap()),
            save.into_sync_draft_with_base_version(Some(RowVersion::new("1").unwrap()))
                .unwrap()
                .with_id(MutationId::new("device_1:2").unwrap()),
            remove
                .into_sync_draft()
                .unwrap()
                .with_id(MutationId::new("device_1:3").unwrap()),
        ];

        let response = resource.push(ctx(), push_request(mutations)).await.unwrap();

        assert_eq!(response.accepted.len(), 3);
        assert_eq!(response.rows.len(), 2);
        assert!(response.rejected.is_empty());
        assert!(response.conflicts.is_empty());
        assert_eq!(
            posts.calls(),
            vec!["create:post_2", "save:post_1", "remove:post_2"]
        );
    }

    #[tokio::test]
    async fn transactional_push_commits_source_write_and_mutation_log() {
        let harness = TxHarness::default();
        let resource = transactional_posts_resource(harness.clone());
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "created".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device_1:1").unwrap());

        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();
        let state = harness.state();

        assert_eq!(response.accepted.len(), 1);
        assert_eq!(response.rows.len(), 1);
        assert_eq!(state.rows["post_1"].title, "created");
        assert!(state.accepted.contains_key("device_1:1"));
        assert_eq!(
            harness.events(),
            vec![
                "begin",
                "log:lookup:device_1:1",
                "log:record:device_1:1",
                "source:create:post_1",
                "commit"
            ]
        );
    }

    #[tokio::test]
    async fn transactional_duplicate_replay_dedupes_inside_transaction() {
        let harness = TxHarness::default();
        let resource = transactional_posts_resource(harness.clone());
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "created".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device_1:1").unwrap());
        resource
            .push(ctx(), push_request([mutation.clone()]))
            .await
            .unwrap();
        harness.clear_events();

        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();

        assert_eq!(response.accepted.len(), 1);
        assert!(
            response.rows.is_empty(),
            "dedupe hits acknowledge exact replay without returning cached rows"
        );
        assert_eq!(
            harness.events(),
            vec!["begin", "log:lookup:device_1:1", "commit"]
        );
    }

    #[tokio::test]
    async fn transactional_conflict_rolls_back_reserved_mutation_log() {
        let harness = TxHarness::default();
        harness.insert(Post {
            id: "post_1".to_string(),
            title: "server".to_string(),
            version: 2,
        });
        let resource = transactional_posts_resource(harness.clone());
        let payload = CrudMutationPayload::save(
            "post_1".to_string(),
            PostDraft {
                title: "client".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft_with_base_version(Some(RowVersion::new("1").unwrap()))
            .unwrap()
            .with_id(MutationId::new("device_1:stale").unwrap());

        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();
        let state = harness.state();

        assert!(response.accepted.is_empty());
        assert_eq!(response.conflicts.len(), 1);
        assert_eq!(state.rows["post_1"].title, "server");
        assert!(state.accepted.is_empty());
        assert_eq!(
            harness.events(),
            vec![
                "begin",
                "log:lookup:device_1:stale",
                "log:record:device_1:stale",
                "source:save:post_1",
                "rollback"
            ]
        );
    }

    #[tokio::test]
    async fn transactional_push_uses_one_transaction_per_mutation() {
        let harness = TxHarness::default();
        let resource = transactional_posts_resource(harness.clone());
        let first = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "one".to_string(),
            },
        )
        .into_sync_draft()
        .unwrap()
        .with_id(MutationId::new("device_1:1").unwrap());
        let second = CrudMutationPayload::create(
            "post_2".to_string(),
            PostDraft {
                title: "two".to_string(),
            },
        )
        .into_sync_draft()
        .unwrap()
        .with_id(MutationId::new("device_1:2").unwrap());

        let response = resource
            .push(ctx(), push_request([first, second]))
            .await
            .unwrap();
        let state = harness.state();

        assert_eq!(response.accepted.len(), 2);
        assert_eq!(state.rows["post_1"].title, "one");
        assert_eq!(state.rows["post_2"].title, "two");
        assert!(state.accepted.contains_key("device_1:1"));
        assert!(state.accepted.contains_key("device_1:2"));
        assert_eq!(
            harness.events(),
            vec![
                "begin",
                "log:lookup:device_1:1",
                "log:record:device_1:1",
                "source:create:post_1",
                "commit",
                "begin",
                "log:lookup:device_1:2",
                "log:record:device_1:2",
                "source:create:post_2",
                "commit",
            ]
        );
    }

    #[tokio::test]
    async fn transactional_pull_uses_source_snapshot_without_write_transaction() {
        let harness = TxHarness::default();
        harness.insert(Post {
            id: "post_1".to_string(),
            title: "cached".to_string(),
            version: 9,
        });
        let resource = transactional_posts_resource(harness.clone());

        let response = resource
            .pull(
                ctx(),
                SyncPullRequest::new(SyncStreamName::new("posts").unwrap()),
            )
            .await
            .unwrap();

        assert_eq!(response.mode, SyncPullMode::Snapshot);
        assert_eq!(response.rows.len(), 1);
        assert_eq!(response.rows[0].key.as_str(), "post_1");
        assert_eq!(response.rows[0].version.as_ref().unwrap().as_str(), "9");
        assert_eq!(
            harness.events(),
            Vec::<String>::new(),
            "snapshot reads should not begin a write transaction"
        );
    }

    #[tokio::test]
    async fn transactional_source_error_rolls_back_reserved_mutation_log() {
        let harness = TxHarness::default();
        harness.fail_source_write();
        let resource = transactional_posts_resource(harness.clone());
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "created".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device_1:1").unwrap());

        let err = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap_err();
        let state = harness.state();

        assert!(err.to_string().contains("source write failed"));
        assert!(state.rows.is_empty());
        assert!(state.accepted.is_empty());
        assert_eq!(
            harness.events(),
            vec![
                "begin",
                "log:lookup:device_1:1",
                "log:record:device_1:1",
                "source:create:post_1",
                "rollback"
            ]
        );
    }

    #[tokio::test]
    async fn transactional_log_record_error_rolls_back_reserved_mutation() {
        let harness = TxHarness::default();
        harness.fail_log_record();
        let resource = transactional_posts_resource(harness.clone());
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "created".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device_1:1").unwrap());

        let err = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap_err();
        let state = harness.state();

        assert!(err.to_string().contains("mutation log failed"));
        assert!(state.rows.is_empty());
        assert!(state.accepted.is_empty());
        assert_eq!(
            harness.events(),
            vec![
                "begin",
                "log:lookup:device_1:1",
                "log:record:device_1:1",
                "rollback"
            ]
        );
    }

    #[tokio::test]
    async fn transactional_rollback_error_preserves_original_failure_context() {
        let harness = TxHarness::default();
        harness.fail_source_write();
        harness.fail_rollback();
        let resource = transactional_posts_resource(harness.clone());
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "created".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device_1:1").unwrap());

        let err = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap_err();

        assert!(err
            .to_string()
            .contains("CRUD transaction rollback failed after"));
        assert!(err.to_string().contains("source write failed"));
        assert!(err.to_string().contains("rollback failed"));
        assert_eq!(
            harness.events(),
            vec![
                "begin",
                "log:lookup:device_1:1",
                "log:record:device_1:1",
                "source:create:post_1",
                "rollback"
            ]
        );
    }

    #[tokio::test]
    async fn duplicate_accepted_mutation_does_not_reapply_source_write() {
        let posts = Posts::default();
        let resource = posts_resource(posts.clone());
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "hello".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device_1:1").unwrap());

        resource
            .push(ctx(), push_request([mutation.clone()]))
            .await
            .unwrap();
        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();

        assert_eq!(response.accepted.len(), 1);
        assert!(
            response.rows.is_empty(),
            "dedupe hits acknowledge exact replay without returning cached rows"
        );
        assert_eq!(posts.calls(), vec!["create:post_1"]);
    }

    #[tokio::test]
    async fn duplicate_mutation_id_with_different_contents_is_rejected() {
        let posts = Posts::default();
        let resource = posts_resource(posts.clone());
        let first = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "first".to_string(),
            },
        )
        .into_sync_draft()
        .unwrap()
        .with_id(MutationId::new("device_1:1").unwrap());
        resource.push(ctx(), push_request([first])).await.unwrap();

        let changed = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "changed".to_string(),
            },
        )
        .into_sync_draft()
        .unwrap()
        .with_id(MutationId::new("device_1:1").unwrap());
        let response = resource.push(ctx(), push_request([changed])).await.unwrap();

        assert!(response.accepted.is_empty());
        assert_eq!(response.rejected.len(), 1);
        assert!(response.rejected[0]
            .reason
            .contains("already accepted with different contents"));
        assert_eq!(posts.calls(), vec!["create:post_1"]);
    }

    #[tokio::test]
    async fn duplicate_mutation_id_with_different_key_is_rejected() {
        let posts = Posts::default();
        let resource = posts_resource(posts.clone());
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "first".to_string(),
            },
        );
        let first = payload
            .clone()
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device_1:1").unwrap());
        resource.push(ctx(), push_request([first])).await.unwrap();

        let replay_with_other_key = payload
            .into_sync_draft()
            .unwrap()
            .row_key(RowKey::new("post_2").unwrap())
            .with_id(MutationId::new("device_1:1").unwrap());
        let response = resource
            .push(ctx(), push_request([replay_with_other_key]))
            .await
            .unwrap();

        assert!(response.accepted.is_empty());
        assert_eq!(response.rejected.len(), 1);
        assert!(response.rejected[0]
            .reason
            .contains("already accepted with different contents"));
        assert_eq!(posts.calls(), vec!["create:post_1"]);
    }

    #[tokio::test]
    async fn bad_payload_is_rejected_per_mutation() {
        let posts = Posts::default();
        let resource = posts_resource(posts);
        let mutation = ClientMutation::upsert(
            MutationId::new("device_1:bad").unwrap(),
            serde_json::json!({ "op": "create", "payload": { "id": "post_1" } }),
        )
        .key("post_1")
        .unwrap();

        let response = resource
            .push(
                ctx(),
                SyncPushRequest::new(SyncStreamName::new("posts").unwrap(), [mutation]),
            )
            .await
            .unwrap();

        assert_eq!(response.rejected.len(), 1);
        assert!(response.rejected[0]
            .reason
            .contains("invalid CRUD mutation payload"));
    }

    #[tokio::test]
    async fn stale_base_version_returns_conflict() {
        let posts = Posts::default();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "server".to_string(),
            version: 2,
        });
        let resource = posts_resource(posts.clone());
        let payload = CrudMutationPayload::save(
            "post_1".to_string(),
            PostDraft {
                title: "client".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft_with_base_version(Some(RowVersion::new("1").unwrap()))
            .unwrap()
            .with_id(MutationId::new("device_1:stale").unwrap());

        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();

        assert_eq!(response.conflicts.len(), 1);
        assert_eq!(response.conflicts[0].reason, "base version is stale");
        assert!(response.accepted.is_empty());
        assert!(posts.calls().is_empty());
    }

    #[tokio::test]
    async fn base_version_without_version_mapper_conflicts_before_write() {
        let posts = Posts::default();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "server".to_string(),
            version: 2,
        });
        let resource = resource("posts", posts.clone())
            .unwrap()
            .id(|post: &Post| post.id.clone())
            .memory_mutation_log();
        let payload = CrudMutationPayload::save(
            "post_1".to_string(),
            PostDraft {
                title: "client".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft_with_base_version(Some(RowVersion::new("1").unwrap()))
            .unwrap()
            .with_id(MutationId::new("device_1:no_version").unwrap());

        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();

        assert_eq!(response.conflicts.len(), 1);
        assert_eq!(response.conflicts[0].reason, "base version is stale");
        assert!(posts.calls().is_empty());
    }

    #[test]
    fn empty_string_row_versions_are_treated_as_missing() {
        assert!(String::new().into_row_version().unwrap().is_none());
        assert!(Option::<String>::Some(String::new())
            .into_row_version()
            .unwrap()
            .is_none());
        assert!("".into_row_version().unwrap().is_none());
        assert!(Option::<&str>::Some("")
            .into_row_version()
            .unwrap()
            .is_none());
    }

    #[test]
    fn snapshot_row_limit_rejects_zero() {
        let err = match resource("posts", Posts::default())
            .unwrap()
            .max_snapshot_rows(0)
        {
            Ok(_) => panic!("zero max snapshot rows should be rejected"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("greater than zero"));
    }
}
