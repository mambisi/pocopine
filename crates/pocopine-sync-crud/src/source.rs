use pocopine_sync::SyncResult;
use serde::{de::DeserializeOwned, Serialize};

use crate::ResourceId;

/// Server-side CRUD source contract.
#[async_trait::async_trait]
pub trait CrudSource: Send + Sync + 'static {
    type Id: ResourceId;
    type Row: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;
    type Draft: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    async fn list(&self, ctx: pocopine_auth::RequestContext) -> SyncResult<Vec<Self::Row>>;

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
    ) -> SyncResult<Self::Row>;

    async fn remove(&self, ctx: pocopine_auth::RequestContext, id: Self::Id) -> SyncResult<()>;
}
