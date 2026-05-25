#![cfg(not(target_arch = "wasm32"))]

use std::{cell::RefCell, rc::Rc};

use pocopine_core::{ComponentState, Scope, ScopeId};
use pocopine_sync::{
    sync_plugin, CollectionSelector, CollectionState, Handle, MemoryLocalStore, RowVersion,
    SyncClient, SyncCollection, SyncCollectionName, SyncCursor, SyncDeviceId, SyncLocalIdentity,
    SyncLocalStoreHandle, SyncPullResponse, SyncResult, SyncRow, SyncStreamName,
};
use pocopine_sync_crud::{
    CrudClientResource, CrudOutcome, CrudRemoveResult, CrudSource, CrudWriteResult, Queued,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;

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

#[derive(Default)]
struct Consumer;

fn customers_state(page: &mut TestPage) -> &mut CollectionState<Customer> {
    &mut page.customers
}

fn host_safe_js_value() -> JsValue {
    // `JsValue::UNDEFINED` touches imported JS statics and panics in native
    // tests. The generated observer test never inspects this dummy value.
    JsValue::from_str("")
}

impl ComponentState for TestPage {
    fn get(&self, _key: &str) -> JsValue {
        host_safe_js_value()
    }

    fn set(&mut self, _key: &str, _value: JsValue) {}

    fn invoke(&mut self, _name: &str, _args: &js_sys::Array) -> JsValue {
        host_safe_js_value()
    }

    fn keys(&self) -> &'static [&'static str] {
        &[]
    }
}

impl ComponentState for Consumer {
    fn get(&self, _key: &str) -> JsValue {
        host_safe_js_value()
    }

    fn set(&mut self, _key: &str, _value: JsValue) {}

    fn invoke(&mut self, _name: &str, _args: &js_sys::Array) -> JsValue {
        host_safe_js_value()
    }

    fn keys(&self) -> &'static [&'static str] {
        &[]
    }
}

fn customer_row(id: &str, version: &str) -> SyncRow<Customer> {
    SyncRow::new(id, Customer { id: id.to_string() })
        .unwrap()
        .version(version)
        .unwrap()
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
    let _observe_view = customers::Resource::<()>::observe_view::<
        fn(&customers::ViewState, Option<&customers::ViewState>),
    >;
    let _use_resource = customers::use_resource::<()>;
    let _collection = customers::collection::<()>;
    let _use_server = customers::Resource::<()>::use_server;
    let _retry_local = customers::Resource::<()>::retry_local;
    let _merge_with = customers::Resource::<()>::merge_with;

    fn assert_aliases<C: 'static>(
        collection: SyncCollection<C, customers::Row>,
        state: &CollectionState<customers::Row>,
    ) -> SyncResult<customers::Client<C>> {
        let _outcome: Option<customers::Outcome> = None;
        let _queued: Option<customers::Queued> = None;
        let _runtime_outcome: Option<CrudOutcome<customers::Id, customers::Row>> = None;
        let _runtime_queued: Option<Queued<customers::Id>> = None;
        let _runtime_client: Option<CrudClientResource<C, customers::Id, customers::Row>> = None;
        let _view: Option<customers::View> = None;
        let _view_state: Option<customers::ViewState> = None;

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
fn generated_resource_observe_view_tracks_state() {
    pocopine_core::set_auto_flush(false);

    let sync = sync_plugin().into_client();
    let page = Rc::new(RefCell::new(TestPage::default()));
    let page_scope = Scope::new(page.clone());
    let handle = Handle::new(page, page_scope.id);
    let resource = customers::use_resource(&sync, handle.clone(), customers_state);
    let consumer_scope = Scope::new(Rc::new(RefCell::new(Consumer)));
    let observed: Rc<RefCell<Vec<customers::ViewState>>> = Rc::new(RefCell::new(Vec::new()));
    let observed_for_callback = observed.clone();

    pocopine_core::scope::with_current_scope_id(consumer_scope.id, || {
        resource
            .observe_view(move |state, _previous| {
                observed_for_callback.borrow_mut().push(state.clone());
            })
            .unwrap();
    });

    assert_eq!(observed.borrow().len(), 1);
    assert_eq!(observed.borrow()[0].view().unwrap().rows.len(), 0);

    {
        let mut page = handle.borrow_mut();
        let request = page.customers.begin_initial();
        page.customers.apply_pull(
            request,
            SyncPullResponse::snapshot(
                SyncStreamName::new(customers::NAME).unwrap(),
                SyncCollectionName::new(customers::NAME).unwrap(),
                vec![customer_row("customer_1", "row_1")],
                Some(SyncCursor::new("cursor_1").unwrap()),
            ),
        );
    }
    pocopine_core::trigger_scope(page_scope.id);
    pocopine_core::flush_sync();

    assert_eq!(observed.borrow().len(), 2);
    assert_eq!(
        observed.borrow()[1].view().unwrap().rows[0].id,
        "customer_1"
    );

    Scope::remove(consumer_scope.id);
    Scope::remove(page_scope.id);
    pocopine_core::set_auto_flush(true);
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
