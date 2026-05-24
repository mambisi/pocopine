use pocopine_sync::{RowVersion, SyncResult};
use pocopine_sync_crud::{CrudRemoveResult, CrudSource, CrudWriteResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Customer {
    id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CustomerDraft {
    id: String,
}

struct Customers;

#[pocopine_sync_crud::resource(name = "customers")]
#[pocopine_sync_crud::async_trait]
impl CrudSource for Customers {
    type Id = String;
    type Row = Customer;
    type Draft = CustomerDraft;

    async fn list(
        &self,
        _ctx: pocopine_auth::RequestContext,
        _limit: usize,
    ) -> SyncResult<Vec<Self::Row>> {
        Ok(Vec::new())
    }

    async fn get(
        &self,
        _ctx: pocopine_auth::RequestContext,
        _id: Self::Id,
    ) -> SyncResult<Option<Self::Row>> {
        Ok(None)
    }

    async fn create(
        &self,
        _ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        _draft: Self::Draft,
    ) -> SyncResult<Self::Row> {
        Ok(Customer { id })
    }

    async fn save(
        &self,
        _ctx: pocopine_auth::RequestContext,
        id: Self::Id,
        _draft: Self::Draft,
        _base_version: Option<RowVersion>,
    ) -> SyncResult<CrudWriteResult<Self::Row>> {
        Ok(CrudWriteResult::applied(Customer { id }))
    }

    async fn remove(
        &self,
        _ctx: pocopine_auth::RequestContext,
        _id: Self::Id,
        _base_version: Option<RowVersion>,
    ) -> SyncResult<CrudRemoveResult<Self::Row>> {
        Ok(CrudRemoveResult::applied())
    }
}

#[test]
fn resource_attribute_preserves_crud_source_impl() {
    fn assert_crud_source<S: CrudSource>() {}

    assert_crud_source::<Customers>();
}
