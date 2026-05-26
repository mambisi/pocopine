#![cfg(not(target_arch = "wasm32"))]

//! End-to-end example for roadmap §7 (Auth & Multi-Tenant Boundaries).
//!
//! Demonstrates the contract the sync-local-first roadmap calls out:
//!
//! > Mutation logs must be scoped to the same authorization domain as the
//! > source query, for example `(tenant_id, mutation_id)`, not only
//! > `mutation_id`.
//!
//! Pattern:
//!
//! * `Customers` reads `x-tenant-id` from the per-request context and
//!   partitions its in-memory rows by tenant — so a `/pull` from tenant A
//!   cannot see tenant B's rows.
//! * `MemoryCrudMutationLog::with_scope_fn` derives its accepted-mutation
//!   key from the same header — so the same `MutationId` value can be used
//!   independently by two tenants without colliding on the log's unique
//!   key. (This mirrors `SqlxCrudMutationLog::new(scope_fn)` for callers
//!   that need a durable production log.)
//! * The source guard returns `Unauthorized` when the request omits the
//!   tenant header.
//!
//! What this proves:
//!
//! * **Server-side partitioning.** A pull as tenant A returns A's rows;
//!   B's pull cannot read A's data.
//! * **Mutation-id reuse is safe across tenants.** Both tenants push the
//!   same `MutationId` value with independent payloads; the log records
//!   each under its tenant scope and the accepted-row maps to the writer.
//! * **Anonymous requests are rejected at the framework boundary.** The
//!   source's per-request guard turns "no `x-tenant-id` header" into
//!   `SyncError::unauthorized("missing tenant")`, which the server mapper
//!   surfaces as `ServerError::Unauthorized` — the same wire shape
//!   framework-level stream guards already use.

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
    sync_server_plugin, ClientMutation, MutationId, RowVersion, SyncError, SyncPullRequest,
    SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncStreamName, SYNC_PULL_PATH,
    SYNC_PUSH_PATH,
};
use pocopine_sync_crud::{
    async_trait, resource, CrudMutationPayload, CrudRemoveResult, CrudSource, CrudWriteResult,
    MemoryCrudMutationLog,
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

/// Tenant-scoped customers source. Rows are partitioned by tenant id in
/// memory; every read and write requires the `x-tenant-id` header.
#[derive(Clone, Default)]
struct TenantCustomers {
    by_tenant: Arc<Mutex<BTreeMap<String, BTreeMap<String, Customer>>>>,
}

impl TenantCustomers {
    fn seed(&self, tenant: &str, customer: Customer) {
        let mut by_tenant = self.by_tenant.lock().unwrap();
        by_tenant
            .entry(tenant.to_string())
            .or_default()
            .insert(customer.id.clone(), customer);
    }

    fn tenant_of(ctx: &RequestContext) -> pocopine_sync::SyncResult<String> {
        ctx.header("x-tenant-id")
            .map(str::to_string)
            .ok_or_else(|| SyncError::unauthorized("missing tenant"))
    }

    fn tenant_rows(
        &self,
        tenant: &str,
        f: impl FnOnce(&BTreeMap<String, Customer>) -> Vec<Customer>,
    ) -> Vec<Customer> {
        let by_tenant = self.by_tenant.lock().unwrap();
        by_tenant.get(tenant).map(f).unwrap_or_default()
    }
}

#[async_trait]
impl CrudSource for TenantCustomers {
    type Id = String;
    type Row = Customer;
    type Draft = CustomerDraft;

    async fn list(
        &self,
        ctx: RequestContext,
        limit: usize,
    ) -> pocopine_sync::SyncResult<Vec<Self::Row>> {
        let tenant = Self::tenant_of(&ctx)?;
        Ok(self.tenant_rows(&tenant, |rows| rows.values().take(limit).cloned().collect()))
    }

    async fn get(
        &self,
        ctx: RequestContext,
        id: Self::Id,
    ) -> pocopine_sync::SyncResult<Option<Self::Row>> {
        let tenant = Self::tenant_of(&ctx)?;
        let by_tenant = self.by_tenant.lock().unwrap();
        Ok(by_tenant
            .get(&tenant)
            .and_then(|rows| rows.get(&id))
            .cloned())
    }

    async fn create(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> pocopine_sync::SyncResult<Self::Row> {
        let tenant = Self::tenant_of(&ctx)?;
        let customer = Customer {
            id: id.clone(),
            name: draft.name,
            version: 1,
        };
        let mut by_tenant = self.by_tenant.lock().unwrap();
        by_tenant
            .entry(tenant)
            .or_default()
            .insert(id, customer.clone());
        Ok(customer)
    }

    async fn save(
        &self,
        ctx: RequestContext,
        id: Self::Id,
        draft: Self::Draft,
        base_version: Option<RowVersion>,
    ) -> pocopine_sync::SyncResult<CrudWriteResult<Self::Row>> {
        let tenant = Self::tenant_of(&ctx)?;
        let mut by_tenant = self.by_tenant.lock().unwrap();
        let rows = by_tenant.entry(tenant).or_default();
        let row = rows
            .get_mut(&id)
            .ok_or_else(|| SyncError::backend("missing customer"))?;
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
        let tenant = Self::tenant_of(&ctx)?;
        let mut by_tenant = self.by_tenant.lock().unwrap();
        let rows = by_tenant.entry(tenant).or_default();
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
async fn tenant_pull_does_not_leak_across_tenants() {
    let customers = TenantCustomers::default();
    customers.seed(
        "tenant_a",
        Customer {
            id: "cust_a1".to_string(),
            name: "Tenant A only".to_string(),
            version: 1,
        },
    );
    customers.seed(
        "tenant_b",
        Customer {
            id: "cust_b1".to_string(),
            name: "Tenant B only".to_string(),
            version: 1,
        },
    );
    let app = router(customers);

    let pulled_a = post_json::<_, SyncPullResponse<Value>>(
        app.clone(),
        SYNC_PULL_PATH,
        Some("tenant_a"),
        &SyncPullRequest::new(SyncStreamName::new(STREAM).unwrap()),
    )
    .await;
    assert_eq!(pulled_a.rows.len(), 1);
    assert_eq!(pulled_a.rows[0].key.as_str(), "cust_a1");

    let pulled_b = post_json::<_, SyncPullResponse<Value>>(
        app,
        SYNC_PULL_PATH,
        Some("tenant_b"),
        &SyncPullRequest::new(SyncStreamName::new(STREAM).unwrap()),
    )
    .await;
    assert_eq!(pulled_b.rows.len(), 1);
    assert_eq!(pulled_b.rows[0].key.as_str(), "cust_b1");
}

#[tokio::test]
async fn same_mutation_id_is_safe_across_distinct_tenants() {
    let customers = TenantCustomers::default();
    let app = router(customers.clone());
    let mutation_id = MutationId::new("device_shared:1").unwrap();

    // tenant_a creates customer "shared" with mutation id "device_shared:1".
    let create_a = CrudMutationPayload::create(
        "shared".to_string(),
        CustomerDraft {
            name: "From tenant A".to_string(),
        },
    )
    .into_sync_draft()
    .unwrap()
    .with_id(mutation_id.clone());
    let pushed_a = post_json::<_, SyncPushResponse<Value>>(
        app.clone(),
        SYNC_PUSH_PATH,
        Some("tenant_a"),
        &SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [to_value(create_a)]),
    )
    .await;
    assert_eq!(pushed_a.accepted[0], mutation_id);

    // tenant_b uses the SAME mutation id for an unrelated write.
    // Without per-tenant log scoping this would be rejected as a duplicate;
    // with `with_scope_fn(tenant)` the accepted-log key is
    // `(tenant, mutation_id)` so it accepts cleanly.
    let create_b = CrudMutationPayload::create(
        "shared".to_string(),
        CustomerDraft {
            name: "From tenant B".to_string(),
        },
    )
    .into_sync_draft()
    .unwrap()
    .with_id(mutation_id.clone());
    let pushed_b = post_json::<_, SyncPushResponse<Value>>(
        app,
        SYNC_PUSH_PATH,
        Some("tenant_b"),
        &SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [to_value(create_b)]),
    )
    .await;
    assert_eq!(pushed_b.accepted[0], mutation_id);
    assert!(pushed_b.rejected.is_empty());
    assert!(pushed_b.conflicts.is_empty());

    // Each tenant's row stays in its own partition.
    let stored = customers.by_tenant.lock().unwrap();
    assert_eq!(
        stored.get("tenant_a").unwrap().get("shared").unwrap().name,
        "From tenant A"
    );
    assert_eq!(
        stored.get("tenant_b").unwrap().get("shared").unwrap().name,
        "From tenant B"
    );
}

#[tokio::test]
async fn requests_missing_tenant_header_are_rejected_at_source() {
    let customers = TenantCustomers::default();
    customers.seed(
        "tenant_a",
        Customer {
            id: "cust_a1".to_string(),
            name: "Tenant A only".to_string(),
            version: 1,
        },
    );
    let app = router(customers);

    let envelope = post_envelope::<_, SyncPullResponse<Value>>(
        app,
        SYNC_PULL_PATH,
        None, // no x-tenant-id header
        &SyncPullRequest::new(SyncStreamName::new(STREAM).unwrap()),
    )
    .await;
    // First catch the leaked-success case explicitly so a regression
    // shows up as "anonymous /pull leaked …" rather than a generic
    // variant mismatch — the security signal is louder.
    if let Ok(ref response) = envelope {
        panic!("anonymous /pull leaked a successful response: {response:?}");
    }
    // The source-side guard returns `SyncError::unauthorized("missing
    // tenant")`, which the server mapper turns into
    // `ServerError::Unauthorized`. Match on the variant directly — a
    // regression to BadRequest / App / Forbidden carrying the word
    // "unauthorized" in its message would otherwise slip through.
    //
    // The message body is intentionally NOT asserted on a specific
    // substring: the wire contract is the variant + a non-empty
    // message, not the literal text of any one source's error.
    match envelope {
        Err(pocopine_core::ServerError::Unauthorized(msg)) => {
            assert!(!msg.is_empty(), "unauthorized message is empty");
        }
        other => panic!("expected ServerError::Unauthorized, got: {other:?}"),
    }
}

fn router(customers: TenantCustomers) -> Router {
    pocopine_server::__reset_for_test();
    let customers = resource(STREAM, customers)
        .unwrap()
        .id(|row: &Customer| row.id.clone())
        .version(|row: &Customer| row.version)
        .mutation_log(MemoryCrudMutationLog::<Customer>::with_scope_fn(
            TenantCustomers::tenant_of,
        ));
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

async fn post_json<T, R>(router: Router, uri: &str, tenant: Option<&str>, payload: &T) -> R
where
    T: Serialize,
    R: DeserializeOwned,
{
    let response = send(router, uri, tenant, payload).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine_core::ServerResult<R> = serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
}

async fn post_envelope<T, R>(
    router: Router,
    uri: &str,
    tenant: Option<&str>,
    payload: &T,
) -> pocopine_core::ServerResult<R>
where
    T: Serialize,
    R: DeserializeOwned,
{
    let response = send(router, uri, tenant, payload).await;
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn send<T: Serialize>(
    router: Router,
    uri: &str,
    tenant: Option<&str>,
    payload: &T,
) -> pocopine_server::axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(tenant) = tenant {
        builder = builder.header("x-tenant-id", tenant);
    }
    router
        .oneshot(
            builder
                .body(Body::from(serde_json::to_string(payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}
