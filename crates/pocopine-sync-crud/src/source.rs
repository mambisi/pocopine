use pocopine_sync::{RowVersion, SyncResult};
use serde::{de::DeserializeOwned, Serialize};

use crate::ResourceId;

/// Conflict returned by an app-owned CRUD source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrudConflict<Row> {
    pub server_row: Option<Row>,
    pub reason: String,
}

impl<Row> CrudConflict<Row> {
    pub fn stale(server_row: Option<Row>) -> Self {
        Self {
            server_row,
            reason: "base version is stale".to_string(),
        }
    }

    pub fn new(server_row: Option<Row>, reason: impl Into<String>) -> Self {
        Self {
            server_row,
            reason: reason.into(),
        }
    }
}

/// Result of an app-owned create/save write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CrudWriteResult<Row> {
    Applied(Row),
    Conflict(CrudConflict<Row>),
}

impl<Row> CrudWriteResult<Row> {
    pub fn applied(row: Row) -> Self {
        Self::Applied(row)
    }

    pub fn conflict(server_row: Option<Row>, reason: impl Into<String>) -> Self {
        Self::Conflict(CrudConflict::new(server_row, reason))
    }

    pub fn stale(server_row: Option<Row>) -> Self {
        Self::Conflict(CrudConflict::stale(server_row))
    }
}

/// Result of an app-owned remove write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CrudRemoveResult<Row> {
    Applied,
    Conflict(CrudConflict<Row>),
}

impl<Row> CrudRemoveResult<Row> {
    pub fn applied() -> Self {
        Self::Applied
    }

    pub fn conflict(server_row: Option<Row>, reason: impl Into<String>) -> Self {
        Self::Conflict(CrudConflict::new(server_row, reason))
    }

    pub fn stale(server_row: Option<Row>) -> Self {
        Self::Conflict(CrudConflict::stale(server_row))
    }
}

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
        ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        base_version: Option<RowVersion>,
    ) -> SyncResult<CrudRemoveResult<Self::Row>>;
}
