//! Mutation-lifecycle primitives for Query-native resources.
//!
//! RFC 090 Phase 2a — moves the typed mutation envelope, idempotency
//! log, optimistic-concurrency outcomes, and conflict types from
//! `pocopine-sync-crud` into their long-term home in
//! `pocopine-sync-query`. The CRUD crate re-exports them under their
//! old `Crud*` names for back-compat through Phase 5; Phase 6
//! deletes the CRUD copies.
//!
//! # Why this module exists
//!
//! Phase 1 (the `Source` trait) put `WriteResult`/`RemoveResult`/`Conflict`
//! in `pocopine_sync_query::source` because they appear in `Source`
//! method signatures. That was expedient but wrong long-term — the
//! same types are also needed by:
//!
//! - The push lifecycle (Phase 2b) that dispatches mutations to
//!   `Source::create`/`save`/`remove`.
//! - The macro-emitted typed write API (Phase 4): `Issue::save(id,
//!   draft)` returns `WriteResult<Issue>`.
//! - The `MutationLog` (this module) which records accepted
//!   mutations and their canonical outcomes.
//!
//! Putting them with the mutation lifecycle is the correct home.
//! `source.rs` re-exports them so existing Phase 1 user code keeps
//! compiling.
//!
//! # Naming
//!
//! The types here drop the `Crud` prefix CRUD's versions carry. The
//! semantics are identical — Phase 1 promised this when it cloned
//! the CRUD types into `source.rs`. CRUD's `pocopine-sync-crud`
//! crate re-exports the moved types as `CrudWriteResult`,
//! `CrudMutationLog`, etc., so existing imports keep working
//! through Phase 5.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pocopine_auth::RequestContext;
use pocopine_sync::{MutationId, RowKey, SyncError, SyncOp, SyncResult};
use serde_json::Value;

/// Outcome of an optimistic-concurrency `save`.
///
/// Moved from `pocopine_sync_query::source` (Phase 1) — see the
/// module-level doc for why. The Phase 1 re-export from `source`
/// keeps existing imports working.
///
/// Derives match the CRUD original (`pocopine_sync_crud::CrudWriteResult`)
/// so the back-compat alias is drop-in for downstream code that does
/// `WriteResult<X> == WriteResult<X>` comparisons.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteResult<Row> {
    /// The write applied. `row` is the canonical post-write state.
    Applied(Row),
    /// The base_version check failed. `Conflict` carries the
    /// server-visible row (if any) so the framework can surface it
    /// to the client for merge / overwrite / discard handling.
    Conflict(Conflict<Row>),
}

impl<Row> WriteResult<Row> {
    pub fn applied(row: Row) -> Self {
        Self::Applied(row)
    }

    /// Construct a conflict with an explicit reason. Mirrors
    /// `CrudWriteResult::conflict` — kept for the back-compat alias.
    pub fn conflict(server_row: Option<Row>, reason: impl Into<String>) -> Self {
        Self::Conflict(Conflict::new(server_row, reason))
    }

    pub fn stale(server_row: Option<Row>) -> Self {
        Self::Conflict(Conflict::stale(server_row))
    }
}

/// Outcome of an optimistic-concurrency `remove`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoveResult<Row> {
    Applied,
    Conflict(Conflict<Row>),
}

impl<Row> RemoveResult<Row> {
    pub fn applied() -> Self {
        Self::Applied
    }

    /// Construct a conflict with an explicit reason. Mirrors
    /// `CrudRemoveResult::conflict`.
    pub fn conflict(server_row: Option<Row>, reason: impl Into<String>) -> Self {
        Self::Conflict(Conflict::new(server_row, reason))
    }

    pub fn stale(server_row: Option<Row>) -> Self {
        Self::Conflict(Conflict::stale(server_row))
    }
}

/// Server-visible state at the moment a conflict was detected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict<Row> {
    /// The row as the server sees it. `None` when the conflict
    /// arises from a missing row (e.g. a stale save against a deleted
    /// id).
    pub server_row: Option<Row>,
    /// Human-readable reason. The framework forwards this verbatim
    /// to the client; sources should make it diagnostic.
    pub reason: String,
}

impl<Row> Conflict<Row> {
    /// Construct a conflict with an explicit reason. Mirrors
    /// `CrudConflict::new` for the back-compat alias.
    pub fn new(server_row: Option<Row>, reason: impl Into<String>) -> Self {
        Self {
            server_row,
            reason: reason.into(),
        }
    }

    /// Construct a conflict with the canonical "base_version stale"
    /// reason. Mirrors `CrudConflict::stale`.
    pub fn stale(server_row: Option<Row>) -> Self {
        Self {
            server_row,
            reason: "base version is stale".to_string(),
        }
    }
}

// MutationPayload + Create/Save/RemovePayload deferred to Phase 2c.
// CRUD's `CrudMutationPayload` uses a `#[serde(tag = "op", content =
// "payload", ...)]` wire shape that this module's first cut didn't
// match. Phase 2c will reconcile the wire shape and move the
// envelope here; until then, CRUD users keep using
// `pocopine_sync_crud::CrudMutationPayload` and new `Source` users
// hand-construct mutations or wait for Phase 4's macro-emitted
// typed writes.

/// Accepted-mutation log entry. Mirrors
/// `pocopine_sync_crud::CrudAcceptedMutation`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedMutation {
    pub mutation_id: MutationId,
    pub op: SyncOp,
    pub key: Option<RowKey>,
    pub payload: Value,
}

impl AcceptedMutation {
    pub fn new(mutation_id: MutationId, op: SyncOp, key: Option<RowKey>, payload: Value) -> Self {
        Self {
            mutation_id,
            op,
            key,
            payload,
        }
    }

    /// `true` iff a replay of the same mutation_id carries an
    /// identical wire envelope. Defensive against retries that
    /// change their mind — e.g. a client that retries a Create as
    /// a Save with the same id is treated as a distinct mutation.
    pub fn matches(&self, op: SyncOp, key: Option<&RowKey>, payload: &Value) -> bool {
        self.op == op && self.key.as_ref() == key && self.payload == *payload
    }
}

/// Result of reserving an accepted mutation id inside a transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationReservation {
    /// The caller reserved this mutation id and should continue
    /// applying the source write in the same transaction.
    Reserved,
    /// The mutation id already exists in the accepted log.
    AlreadyAccepted(AcceptedMutation),
}

/// Idempotency log for sync-query mutations. Production
/// implementations MUST scope lookups to the same authorization
/// domain as the underlying `Source`, for example
/// `(tenant_id, mutation_id)`.
///
/// Mirrors `pocopine_sync_crud::CrudMutationLog` byte-for-byte —
/// only the trait name changes (drops the `Crud` prefix). The CRUD
/// crate re-exports this trait under the old name.
#[async_trait::async_trait]
pub trait MutationLog<Row>: Send + Sync + 'static
where
    Row: Clone + Send + Sync + 'static,
{
    async fn accepted_mutation(
        &self,
        ctx: &RequestContext,
        mutation_id: &MutationId,
    ) -> SyncResult<Option<AcceptedMutation>>;

    async fn record_accepted_mutation(
        &self,
        ctx: &RequestContext,
        accepted: AcceptedMutation,
    ) -> SyncResult<()>;
}

/// Function used by `MemoryMutationLog::with_scope_fn` to project an
/// authorization scope (e.g. a tenant id) out of the per-request
/// context.
pub type MemoryScopeFn = Arc<dyn Fn(&RequestContext) -> SyncResult<String> + Send + Sync + 'static>;

/// Process-local mutation log for tests and single-process demos.
///
/// The default constructor uses one global scope. Multi-tenant
/// production resources should use [`MemoryMutationLog::with_scope_fn`]
/// to derive the authorization scope from the request context.
#[derive(Clone)]
pub struct MemoryMutationLog<Row> {
    accepted: Arc<Mutex<BTreeMap<(String, String), AcceptedMutation>>>,
    scope: MemoryScopeFn,
    _marker: std::marker::PhantomData<fn() -> Row>,
}

impl<Row> std::fmt::Debug for MemoryMutationLog<Row> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryMutationLog").finish_non_exhaustive()
    }
}

impl<Row> Default for MemoryMutationLog<Row> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Row> MemoryMutationLog<Row> {
    /// Build a single-scope memory log. Every accepted mutation
    /// lives in one global keyspace; use
    /// [`with_scope_fn`](Self::with_scope_fn) for multi-tenant tests.
    pub fn new() -> Self {
        Self {
            accepted: Arc::new(Mutex::new(BTreeMap::new())),
            scope: Arc::new(|_ctx| Ok(String::new())),
            _marker: std::marker::PhantomData,
        }
    }

    /// Build a memory log that derives its authorization scope from
    /// the request context. Mirrors `pocopine_sync_sqlx::SqlxCrudMutationLog`'s
    /// shape (which Phase 2b/sqlx migration will rename to drop the
    /// `Crud` prefix) so tests can exercise the same scoping
    /// boundary as production logs.
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
impl<Row> MutationLog<Row> for MemoryMutationLog<Row>
where
    Row: Clone + Send + Sync + 'static,
{
    async fn accepted_mutation(
        &self,
        ctx: &RequestContext,
        mutation_id: &MutationId,
    ) -> SyncResult<Option<AcceptedMutation>> {
        let scope = (self.scope)(ctx)?;
        let accepted = self
            .accepted
            .lock()
            .map_err(|_| SyncError::backend("memory mutation log lock poisoned"))?;
        Ok(accepted
            .get(&(scope, mutation_id.as_str().to_string()))
            .cloned())
    }

    async fn record_accepted_mutation(
        &self,
        ctx: &RequestContext,
        accepted: AcceptedMutation,
    ) -> SyncResult<()> {
        let scope = (self.scope)(ctx)?;
        let mut entries = self
            .accepted
            .lock()
            .map_err(|_| SyncError::backend("memory mutation log lock poisoned"))?;
        entries.insert((scope, accepted.mutation_id.as_str().to_string()), accepted);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderMap, Method};

    fn ctx() -> RequestContext {
        RequestContext::new(Method::POST, "/test".parse().unwrap(), HeaderMap::new())
    }

    fn mutation(id: &str) -> AcceptedMutation {
        AcceptedMutation::new(
            MutationId::new(id).unwrap(),
            SyncOp::Upsert,
            None,
            Value::Null,
        )
    }

    #[derive(Clone)]
    struct Row;

    #[tokio::test]
    async fn memory_log_records_and_retrieves() {
        let log: MemoryMutationLog<Row> = MemoryMutationLog::new();
        let m = mutation("m1");
        log.record_accepted_mutation(&ctx(), m.clone())
            .await
            .unwrap();
        let found = log.accepted_mutation(&ctx(), &m.mutation_id).await.unwrap();
        assert_eq!(found, Some(m));
    }

    #[tokio::test]
    async fn memory_log_distinct_scopes_isolate_mutations() {
        // Tenant boundary: the same mutation_id in two tenants must
        // not collide. This is the contract that
        // `pocopine_sync_crud::tests::tenant_boundary` was the
        // canonical test for; moving it here pins the same semantics
        // on the renamed types.
        let log: MemoryMutationLog<Row> =
            MemoryMutationLog::with_scope_fn(|ctx: &RequestContext| {
                Ok(ctx
                    .header("x-tenant-id")
                    .map(str::to_string)
                    .unwrap_or_default())
            });

        let mut tenant_a_headers = HeaderMap::new();
        tenant_a_headers.insert("x-tenant-id", "A".parse().unwrap());
        let ctx_a = RequestContext::new(Method::POST, "/t".parse().unwrap(), tenant_a_headers);

        let mut tenant_b_headers = HeaderMap::new();
        tenant_b_headers.insert("x-tenant-id", "B".parse().unwrap());
        let ctx_b = RequestContext::new(Method::POST, "/t".parse().unwrap(), tenant_b_headers);

        let m = mutation("m1");
        log.record_accepted_mutation(&ctx_a, m.clone())
            .await
            .unwrap();
        // Tenant A sees the mutation, tenant B does not.
        assert_eq!(
            log.accepted_mutation(&ctx_a, &m.mutation_id).await.unwrap(),
            Some(m.clone())
        );
        assert_eq!(
            log.accepted_mutation(&ctx_b, &m.mutation_id).await.unwrap(),
            None
        );
    }

    #[test]
    fn accepted_mutation_matches_only_exact_replay() {
        let m = mutation("m1");
        assert!(m.matches(SyncOp::Upsert, None, &Value::Null));
        // Different op
        assert!(!m.matches(SyncOp::Delete, None, &Value::Null));
        // Different key
        let k = RowKey::new("k").unwrap();
        assert!(!m.matches(SyncOp::Upsert, Some(&k), &Value::Null));
        // Different payload
        assert!(!m.matches(SyncOp::Upsert, None, &Value::Bool(true)));
    }
}
