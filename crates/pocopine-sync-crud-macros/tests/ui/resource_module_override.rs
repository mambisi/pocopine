use pocopine_sync::{CollectionState, RowVersion, SyncResult};
use pocopine_sync_crud::{CrudRemoveResult, CrudSource, CrudWriteResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Customer {
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomerDraft {
    pub id: String,
}

pub struct Customers;

#[pocopine_sync_crud::resource(name = "tenant-customers", module = tenant_customers)]
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

fn main() {
    assert_eq!(tenant_customers::NAME, "tenant-customers");
    let state = CollectionState::<Customer>::default();
    let _ = tenant_customers::view(&state).unwrap();
    let _ = tenant_customers::resource(Customers).unwrap();
}
