#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use http_body_util::BodyExt;
use pocopine_auth::RequestContext;
use pocopine_server::{
    axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    },
    Server,
};
use pocopine_sync::{
    sync_server_plugin, ClientMutation, MutationId, RowVersion, SyncPullMode, SyncPullRequest,
    SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncStreamName, SYNC_PULL_PATH,
    SYNC_PUSH_PATH,
};
use pocopine_sync_crud::{
    async_trait, resource, CrudMutationPayload, CrudRemoveResult, CrudSource, CrudWriteResult,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

const STREAM: &str = "customers";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Customer {
    id: String,
    name: String,
    version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CustomerDraft {
    name: String,
}

#[derive(Clone, Default)]
struct Customers {
    rows: Arc<Mutex<BTreeMap<String, Customer>>>,
}

impl Customers {
    fn insert(&self, customer: Customer) {
        self.rows
            .lock()
            .unwrap()
            .insert(customer.id.clone(), customer);
    }
}

#[async_trait]
impl CrudSource for Customers {
    type Id = String;
    type Row = Customer;
    type Draft = CustomerDraft;

    async fn list(
        &self,
        ctx: RequestContext,
        limit: usize,
    ) -> pocopine_sync::SyncResult<Vec<Self::Row>> {
        let _ = ctx;
        Ok(self
            .rows
            .lock()
            .unwrap()
            .values()
            .take(limit)
            .cloned()
            .collect())
    }

    async fn get(
        &self,
        ctx: RequestContext,
        id: Self::Id,
    ) -> pocopine_sync::SyncResult<Option<Self::Row>> {
        let _ = ctx;
        Ok(self.rows.lock().unwrap().get(&id).cloned())
    }

    async fn create(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> pocopine_sync::SyncResult<Self::Row> {
        let _ = ctx;
        let customer = Customer {
            id: id.clone(),
            name: draft.name,
            version: 1,
        };
        self.rows.lock().unwrap().insert(id, customer.clone());
        Ok(customer)
    }

    async fn save(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        draft: Self::Draft,
        base_version: Option<RowVersion>,
    ) -> pocopine_sync::SyncResult<CrudWriteResult<Self::Row>> {
        let _ = ctx;
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .get_mut(&id)
            .ok_or_else(|| pocopine_sync::SyncError::backend("missing customer"))?;
        if let Some(base_version) = &base_version {
            if base_version.as_str() != row.version.to_string() {
                return Ok(CrudWriteResult::stale(Some(row.clone())));
            }
        }
        row.name = draft.name;
        row.version = row.version.saturating_add(1);
        Ok(CrudWriteResult::applied(row.clone()))
    }

    async fn remove(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        base_version: Option<RowVersion>,
    ) -> pocopine_sync::SyncResult<CrudRemoveResult<Self::Row>> {
        let _ = ctx;
        let mut rows = self.rows.lock().unwrap();
        if let Some(base_version) = &base_version {
            let Some(row) = rows.get(&id) else {
                return Ok(CrudRemoveResult::stale(None));
            };
            if base_version.as_str() != row.version.to_string() {
                return Ok(CrudRemoveResult::stale(Some(row.clone())));
            }
        }
        rows.remove(&id);
        Ok(CrudRemoveResult::applied())
    }
}

#[tokio::test]
async fn crud_resource_serves_pull_and_push_through_sync_routes() {
    let customers = Customers::default();
    customers.insert(Customer {
        id: "customer_1".to_string(),
        name: "Seeded".to_string(),
        version: 7,
    });
    let app = router(customers.clone());

    let pulled = post_json::<_, SyncPullResponse<Value>>(
        app.clone(),
        SYNC_PULL_PATH,
        &SyncPullRequest::new(SyncStreamName::new(STREAM).unwrap()),
    )
    .await;

    assert_eq!(pulled.mode, SyncPullMode::Snapshot);
    assert_eq!(pulled.rows.len(), 1);
    assert_eq!(pulled.rows[0].key.as_str(), "customer_1");
    assert_eq!(pulled.rows[0].value["name"], "Seeded");

    let create = CrudMutationPayload::create(
        "customer_2".to_string(),
        CustomerDraft {
            name: "Created through route".to_string(),
        },
    )
    .into_sync_draft()
    .unwrap()
    .with_id(MutationId::new("device_test:1").unwrap());
    let pushed = post_json::<_, SyncPushResponse<Value>>(
        app,
        SYNC_PUSH_PATH,
        &SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [to_value(create)]),
    )
    .await;

    assert_eq!(pushed.accepted[0].as_str(), "device_test:1");
    assert!(pushed.rejected.is_empty());
    assert!(pushed.conflicts.is_empty());
    assert_eq!(pushed.rows[0].key.as_str(), "customer_2");
    assert_eq!(pushed.rows[0].value["name"], "Created through route");
    assert_eq!(
        customers
            .rows
            .lock()
            .unwrap()
            .get("customer_2")
            .unwrap()
            .name,
        "Created through route"
    );
}

fn router(customers: Customers) -> Router {
    pocopine_server::__reset_for_test();
    let customers = resource(STREAM, customers)
        .unwrap()
        .id(|row: &Customer| row.id.clone())
        .version(|row: &Customer| row.version)
        .memory_mutation_log();
    let sync = pocopine_sync::SyncServer::builder()
        .public_stream(customers)
        .build();
    Server::new(Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .unwrap()
}

fn to_value(
    mutation: ClientMutation<CrudMutationPayload<String, CustomerDraft>>,
) -> ClientMutation<Value> {
    ClientMutation {
        id: mutation.id,
        key: mutation.key,
        op: mutation.op,
        base_version: mutation.base_version,
        payload: serde_json::to_value(mutation.payload).unwrap(),
        migrated_payload: None,
    }
}

async fn post_json<T, R>(router: Router, uri: &str, payload: &T) -> R
where
    T: Serialize,
    R: DeserializeOwned,
{
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine_core::ServerResult<R> = serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
}
