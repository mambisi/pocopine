#![cfg(not(target_arch = "wasm32"))]

use std::{cell::RefCell, rc::Rc};

use pocopine_core::ScopeId;
use pocopine_sync::{
    sync_plugin, CollectionSelector, CollectionState, Handle, MemoryLocalStore, RowVersion,
    SyncClient, SyncCollection, SyncDeviceId, SyncLocalIdentity, SyncLocalStoreHandle, SyncResult,
    SyncStreamName,
};
use pocopine_sync_crud::{
    CrudClientResource, CrudOutcome, CrudRemoveResult, CrudSource, CrudWriteResult, Queued,
};
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

#[derive(Default)]
struct TestPage {
    customers: CollectionState<Customer>,
}

fn customers_state(page: &mut TestPage) -> &mut CollectionState<Customer> {
    &mut page.customers
}

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

#[test]
fn resource_attribute_generates_module_contract() {
    assert_eq!(customers::NAME, "customers");

    let state = CollectionState::<Customer>::default();
    let view = customers::view(&state).unwrap();
    assert!(view.is_empty());

    let _id: SyncResult<customers::Id> = customers::new_id();
    let _create_options = customers::CreateOptions::new();
    let _save_options = customers::SaveOptions::new();
    let _remove_options = customers::RemoveOptions::new();
    let _builder = customers::resource(Customers).unwrap();
    let _resource: Option<customers::Resource<()>> = None;
    let _resource_new = customers::Resource::<()>::new;
    let _use_resource = customers::use_resource::<()>;
    let _collection = customers::collection::<()>;

    fn assert_aliases<C: 'static>(
        collection: SyncCollection<C, customers::Row>,
        state: &CollectionState<customers::Row>,
    ) -> SyncResult<customers::Client<C>> {
        let _outcome: Option<customers::Outcome> = None;
        let _queued: Option<customers::Queued> = None;
        let _runtime_outcome: Option<CrudOutcome<customers::Id, customers::Row>> = None;
        let _runtime_queued: Option<Queued<customers::Id>> = None;
        let _runtime_client: Option<CrudClientResource<C, customers::Id, customers::Row>> = None;

        customers::client(collection, state)
    }

    fn assert_resource_api(
        sync: &SyncClient,
        handle: Handle<()>,
        selector: CollectionSelector<(), customers::Row>,
    ) -> customers::Resource<()> {
        customers::use_resource(sync, handle, selector)
    }

    let _ = assert_aliases::<()>;
    let _ = assert_resource_api;
}

#[tokio::test]
async fn generated_resource_create_queues_to_local_store() {
    let store: SyncLocalStoreHandle = Rc::new(MemoryLocalStore::new());
    store
        .save_identity(SyncLocalIdentity::new(
            SyncDeviceId::new("device_resource_test").unwrap(),
        ))
        .await
        .unwrap();
    let sync = sync_plugin()
        .shared_local_store(store.clone())
        .into_client();
    let page = Rc::new(RefCell::new(TestPage::default()));
    let handle = Handle::new(page.clone(), ScopeId(1));
    let resource = customers::use_resource(&sync, handle, customers_state);

    let outcome = resource
        .create_with_options(
            "customer_1".to_string(),
            CustomerDraft {
                id: "customer_1".to_string(),
            },
            customers::CreateOptions::new().optimistic(Customer {
                id: "customer_1".to_string(),
            }),
        )
        .await
        .unwrap();

    match outcome {
        CrudOutcome::Queued(queued) => {
            assert_eq!(queued.id, "customer_1");
            assert_eq!(queued.mutation_id.as_str(), "device_resource_test:1");
        }
        other => panic!("expected queued outcome, got {other:?}"),
    }

    assert!(page.borrow().customers.rows.is_empty());

    let pending = store
        .pending_mutations(&SyncStreamName::new(customers::NAME).unwrap())
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
}

#[test]
fn generated_resource_view_and_client_return_error_when_state_is_already_borrowed() {
    let sync = sync_plugin().into_client();
    let page = Rc::new(RefCell::new(TestPage::default()));
    let handle = Handle::new(page, ScopeId(2));
    let resource = customers::use_resource(&sync, handle.clone(), customers_state);
    let _borrow = handle.borrow_mut();

    let view_err = match resource.view() {
        Ok(_) => panic!("expected view borrow error"),
        Err(err) => err,
    };
    let client_err = match resource.client() {
        Ok(_) => panic!("expected client borrow error"),
        Err(err) => err,
    };

    assert!(view_err.to_string().contains("already borrowed"));
    assert!(client_err.to_string().contains("already borrowed"));
}
