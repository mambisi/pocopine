use pocopine_auth::RequestContext;
use pocopine_sync::{RowVersion, SyncResult};
use serde::{de::DeserializeOwned, Serialize};

use crate::ResourceId;

// RFC 090 Phase 2a — `CrudConflict`, `CrudWriteResult`, and
// `CrudRemoveResult` moved to `pocopine_sync_query::write`. The
// `pub use ... as Crud*` aliases here keep existing CRUD imports
// (`pocopine_sync_crud::CrudConflict`, `CrudWriteResult`, etc.) and
// `==` comparisons working byte-for-byte. The canonical types live
// in sync-query; Phase 6 deletes CRUD entirely and downstream code
// drops the `Crud` prefix.
pub use pocopine_sync_query::write::{
    Conflict as CrudConflict, DeleteResult as CrudRemoveResult, WriteResult as CrudWriteResult,
};

/// Server-side CRUD source contract.
///
/// `list` receives the maximum number of rows the sync adapter is willing to
/// return in one snapshot response. Implementations should push this limit into
/// their database query instead of loading an unbounded table and trimming
/// after the fact.
///
/// `save` and `remove` receive the client supplied base row version. When it is
/// `Some`, implementations must check and write atomically, for example with
/// `UPDATE ... WHERE id = ? AND version = ? RETURNING ...`, and return
/// [`CrudWriteResult::Conflict`] or [`CrudRemoveResult::Conflict`] when the
/// version is stale. This keeps optimistic concurrency inside the database
/// operation instead of splitting it into a separate `get` and write.
#[async_trait::async_trait]
pub trait CrudSource: Send + Sync + 'static {
    type Id: ResourceId;
    type Row: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;
    type Draft: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    async fn list(
        &self,
        ctx: pocopine_auth::RequestContext,
        limit: usize,
    ) -> SyncResult<Vec<Self::Row>>;

    async fn get(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
    ) -> SyncResult<Option<Self::Row>>;

    async fn create(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SyncResult<Self::Row>;

    async fn save(
        &self,
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        draft: Self::Draft,
        base_version: Option<RowVersion>,
    ) -> SyncResult<CrudWriteResult<Self::Row>>;

    async fn remove(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        base_version: Option<RowVersion>,
    ) -> SyncResult<CrudRemoveResult<Self::Row>>;
}

/// Server-side CRUD source contract for transaction-backed writes.
///
/// Implement this in addition to [`CrudSource`] when the source write and
/// accepted-mutation log insert can share one app/database transaction. Reads
/// still use [`CrudSource::list`] and [`CrudSource::get`]; only mutating writes
/// receive the active transaction handle.
#[async_trait::async_trait]
pub trait TransactionalCrudSource<Tx>: CrudSource
where
    Tx: Send,
{
    async fn create_in_tx(
        &self,
        tx: &mut Tx,
        ctx: &RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SyncResult<Self::Row>;

    async fn save_in_tx(
        &self,
        tx: &mut Tx,
        ctx: &RequestContext,
        id: Self::Id,
        draft: Self::Draft,
        base_version: Option<RowVersion>,
    ) -> SyncResult<CrudWriteResult<Self::Row>>;

    async fn remove_in_tx(
        &self,
        tx: &mut Tx,
        ctx: &RequestContext,
        id: Self::Id,
        base_version: Option<RowVersion>,
    ) -> SyncResult<CrudRemoveResult<Self::Row>>;
}
