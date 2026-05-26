#![cfg(not(target_arch = "wasm32"))]
// `registry_lock` deliberately holds a `MutexGuard` across `.await`
// to serialize tests that share the process-global plugin registry
// (`pocopine_server::__reset_for_test`). Tests are linearized; the
// async `tokio::sync::Mutex` would change behaviour for no benefit.
#![allow(clippy::await_holding_lock)]

//! End-to-end test for the Batch 3 schema-migration adapter.
//!
//! Covers both halves of the contract:
//!
//! * `.schema_version(N)` alone (no `.migrate_with(...)`) — a stale
//!   push with `request.schema_version < N` is rejected per-mutation
//!   with a `SyncError::SchemaMigration`-shaped reason, while
//!   matching-version pushes pass through.
//! * `.schema_version(N).migrate_with(fn)` — the framework calls
//!   the registered fn before delegating to `source.push`, so a
//!   stale payload that the migrator can transform ends up in
//!   `accepted` with the new shape.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

/// Process-global plugin registry serialization. See the matching
/// helpers in `pocopine-server/tests/server_plugin.rs` and
/// `server_request_events.rs` for the reasoning.
fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

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
    sync_server_plugin, ClientMutation, MutationId, RowVersion, SyncPushRequest, SyncPushResponse,
    SyncStreamName, SYNC_PUSH_PATH,
};
use pocopine_sync_crud::{
    async_trait, resource, CrudMutationPayload, CrudRemoveResult, CrudSource, CrudWriteResult,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

const STREAM: &str = "customers";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CustomerV2 {
    id: String,
    name: String,
    /// New v2 field: prior versions had no `email`. The migrator below
    /// defaults it to an empty string when migrating a v1 draft up.
    email: String,
    version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CustomerDraftV2 {
    name: String,
    email: String,
}

#[derive(Clone, Default)]
struct Customers {
    rows: Arc<Mutex<BTreeMap<String, CustomerV2>>>,
}

#[async_trait]
impl CrudSource for Customers {
    type Id = String;
    type Row = CustomerV2;
    type Draft = CustomerDraftV2;

    async fn list(
        &self,
        _ctx: RequestContext,
        limit: usize,
    ) -> pocopine_sync::SyncResult<Vec<Self::Row>> {
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
        _ctx: RequestContext,
        id: Self::Id,
    ) -> pocopine_sync::SyncResult<Option<Self::Row>> {
        Ok(self.rows.lock().unwrap().get(&id).cloned())
    }

    async fn create(
        &self,
        _ctx: RequestContext,
        id: Self::Id,
        draft: Self::Draft,
    ) -> pocopine_sync::SyncResult<Self::Row> {
        let row = CustomerV2 {
            id: id.clone(),
            name: draft.name,
            email: draft.email,
            version: 1,
        };
        self.rows.lock().unwrap().insert(id, row.clone());
        Ok(row)
    }

    async fn save(
        &self,
        _ctx: RequestContext,
        id: Self::Id,
        draft: Self::Draft,
        _base_version: Option<RowVersion>,
    ) -> pocopine_sync::SyncResult<CrudWriteResult<Self::Row>> {
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .get_mut(&id)
            .ok_or_else(|| pocopine_sync::SyncError::backend(format!("missing customer {id}")))?;
        row.name = draft.name;
        row.email = draft.email;
        row.version = row.version.saturating_add(1);
        Ok(CrudWriteResult::applied(row.clone()))
    }

    async fn remove(
        &self,
        _ctx: RequestContext,
        id: Self::Id,
        _base_version: Option<RowVersion>,
    ) -> pocopine_sync::SyncResult<CrudRemoveResult<Self::Row>> {
        self.rows.lock().unwrap().remove(&id);
        Ok(CrudRemoveResult::applied())
    }
}

/// Migrator: a v1 draft has only `name`; v2 needs `name` AND `email`.
/// The migrator defaults the missing field to an empty string.
fn migrate_v1_to_v2(from: u32, to: u32, mut value: Value) -> pocopine_sync::SyncResult<Value> {
    if from != 1 || to != 2 {
        return Err(pocopine_sync::SyncError::schema_migration(STREAM, from, to));
    }
    // The wire payload is the CrudMutationPayload enum tagged
    // `{ "op": "create" | "save" | "remove", "payload": { "id": ..., "draft": <draft> } }`.
    // For create/save we widen the inner draft with a default email.
    let Some(op) = value
        .get("op")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
    else {
        return Err(pocopine_sync::SyncError::backend(
            "migrate_v1_to_v2: payload missing `op`",
        ));
    };
    match op.as_str() {
        "create" | "save" => {
            if let Some(payload) = value.get_mut("payload") {
                if let Some(draft) = payload.get_mut("draft") {
                    if let Some(obj) = draft.as_object_mut() {
                        obj.entry("email".to_string())
                            .or_insert_with(|| Value::String(String::new()));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(value)
}

#[tokio::test]
async fn stale_schema_push_without_migrator_rejects_per_mutation() {
    let _lock = registry_lock();
    let customers = Customers::default();
    let app = router_without_migrator(customers.clone());

    // Client at v1 pushing a create — server is at v2 with no migrator.
    let create = CrudMutationPayload::create(
        "c1".to_string(),
        // Wire shape: arbitrary `serde_json::Value` payload. We send
        // ONLY `name` (the v1 shape) — the server can't deserialize
        // into v2's CustomerDraftV2 (which requires `email`).
        serde_json::json!({ "name": "Alice" }),
    )
    .into_sync_draft()
    .unwrap()
    .with_id(MutationId::new("device:1").unwrap());
    let req = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [to_value(create)])
        .with_schema_version(1);
    let pushed = post_json::<_, SyncPushResponse<Value>>(app, SYNC_PUSH_PATH, &req).await;

    assert!(pushed.accepted.is_empty(), "must not accept stale push");
    assert_eq!(pushed.rejected.len(), 1, "expected 1 rejected");
    let reason = &pushed.rejected[0].reason;
    assert!(
        reason.contains("schema migration") && reason.contains("v1") && reason.contains("v2"),
        "expected SchemaMigration reason, got: {reason}"
    );
    assert!(
        customers.rows.lock().unwrap().is_empty(),
        "source must NOT have processed the stale mutation"
    );
}

#[tokio::test]
async fn stale_schema_push_with_migrator_passes_through() {
    let _lock = registry_lock();
    let customers = Customers::default();
    let app = router_with_migrator(customers.clone());

    // Same v1 client payload — but server has registered migrate_v1_to_v2.
    let create =
        CrudMutationPayload::create("c1".to_string(), serde_json::json!({ "name": "Alice" }))
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device:1").unwrap());
    let req = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [to_value(create)])
        .with_schema_version(1);
    let pushed = post_json::<_, SyncPushResponse<Value>>(app, SYNC_PUSH_PATH, &req).await;

    assert_eq!(pushed.accepted.len(), 1, "expected accept after migrate");
    assert!(pushed.rejected.is_empty(), "must NOT reject after migrate");
    assert_eq!(pushed.rows.len(), 1);
    assert_eq!(pushed.rows[0].value["name"], "Alice");
    // The migrator filled in the default empty email.
    assert_eq!(pushed.rows[0].value["email"], "");
    assert_eq!(
        customers.rows.lock().unwrap().get("c1").unwrap().name,
        "Alice"
    );
}

#[tokio::test]
async fn matching_schema_push_skips_migrator_entirely() {
    let _lock = registry_lock();
    let customers = Customers::default();
    let app = router_with_migrator(customers.clone());

    // Client at v2 — request_schema_version == source.schema_version, so
    // the migrator is never consulted. Push goes straight through.
    let create = CrudMutationPayload::create(
        "c1".to_string(),
        serde_json::json!({ "name": "Alice", "email": "alice@example.com" }),
    )
    .into_sync_draft()
    .unwrap()
    .with_id(MutationId::new("device:1").unwrap());
    let req = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [to_value(create)])
        .with_schema_version(2);
    let pushed = post_json::<_, SyncPushResponse<Value>>(app, SYNC_PUSH_PATH, &req).await;

    assert_eq!(pushed.accepted.len(), 1);
    assert_eq!(pushed.rows[0].value["email"], "alice@example.com");
}

/// Repro for code-review finding #3: pre-rejected mutations must
/// reach the client even when `source.push` returns an error for the
/// surviving mutations. We don't have a transient-error-injecting
/// source on hand, so we test the simpler invariant: with NO
/// survivors (all mutations pre-rejected and source.push receives an
/// empty batch), the response carries the pre-rejected entries.
#[tokio::test]
async fn all_stale_no_migrator_still_surfaces_rejections() {
    let _lock = registry_lock();
    let customers = Customers::default();
    let app = router_without_migrator(customers.clone());

    let m1 = CrudMutationPayload::create("c1".to_string(), serde_json::json!({ "name": "Alice" }))
        .into_sync_draft()
        .unwrap()
        .with_id(MutationId::new("device:1").unwrap());
    let m2 = CrudMutationPayload::create("c2".to_string(), serde_json::json!({ "name": "Bob" }))
        .into_sync_draft()
        .unwrap()
        .with_id(MutationId::new("device:2").unwrap());
    let req = SyncPushRequest::new(
        SyncStreamName::new(STREAM).unwrap(),
        [to_value(m1), to_value(m2)],
    )
    .with_schema_version(1);
    let pushed = post_json::<_, SyncPushResponse<Value>>(app, SYNC_PUSH_PATH, &req).await;

    assert!(pushed.accepted.is_empty());
    assert_eq!(pushed.rejected.len(), 2);
    assert!(customers.rows.lock().unwrap().is_empty());
}

/// Repro for Codex finding #4: a NEWER client pushing to an OLDER
/// server (rolling deploy direction) is rejected wholesale at the
/// wire layer with `BadRequest`. The framework MUST NOT pass the
/// newer-shape payload through to `source.push`, because serde's
/// default deserializer silently drops unknown fields — which could
/// cause field loss when the v1 source happily accepts a v2 payload.
#[tokio::test]
async fn newer_client_push_rejected_at_wire() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    // Server at v1 (NO `.schema_version(...)` call) with no migrator.
    let customers = Customers::default();
    let resource = resource(STREAM, customers.clone())
        .unwrap()
        .id(|row: &CustomerV2| row.id.clone())
        .version(|row: &CustomerV2| row.version)
        .memory_mutation_log();
    let sync = pocopine_sync::SyncServer::builder()
        .public_stream(resource)
        .build();
    let app = Server::new(Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .unwrap();

    let create = CrudMutationPayload::create(
        "c1".to_string(),
        serde_json::json!({ "name": "Alice", "email": "alice@example.com" }),
    )
    .into_sync_draft()
    .unwrap()
    .with_id(MutationId::new("device:1").unwrap());
    let req = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [to_value(create)])
        .with_schema_version(2);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(SYNC_PUSH_PATH)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&req).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine_core::ServerResult<SyncPushResponse<Value>> =
        serde_json::from_slice(&bytes).unwrap();
    match outer {
        Err(pocopine_core::ServerError::BadRequest(msg)) => {
            assert!(
                msg.contains("schema migration") && msg.contains("v2") && msg.contains("v1"),
                "expected newer-client-rejection, got: {msg}"
            );
        }
        other => panic!("expected BadRequest, got: {other:?}"),
    }
    assert!(
        customers.rows.lock().unwrap().is_empty(),
        "v1 source must not have processed the v2 payload"
    );
}

/// Repro for Codex finding #5: a retry of a previously-accepted
/// mutation succeeds even when the framework's migrator now fails
/// on the same inputs. Achieved by: first push accepts via migrator,
/// second push (same mutation_id, same wire payload) hits a router
/// whose migrator now ALWAYS errors. The CRUD source's idempotency
/// log consults the original `mutation.payload` BEFORE consuming the
/// migration outcome, so the retry is accepted.
#[tokio::test]
async fn idempotent_retry_survives_migrator_change() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    // Shared in-process state across two routers so the idempotency
    // log persists between attempts.
    let customers = Customers::default();
    let log = pocopine_sync_crud::MemoryCrudMutationLog::<CustomerV2>::new();

    // First push: v2 server with a working migrator.
    let router_a = {
        let customers = resource(STREAM, customers.clone())
            .unwrap()
            .schema_version(2)
            .unwrap()
            .migrate_with(migrate_v1_to_v2)
            .id(|row: &CustomerV2| row.id.clone())
            .version(|row: &CustomerV2| row.version)
            .mutation_log(log.clone());
        let sync = pocopine_sync::SyncServer::builder()
            .public_stream(customers)
            .build();
        Server::new(Router::new())
            .plugin(sync_server_plugin(sync))
            .try_finalize()
            .unwrap()
    };

    let create =
        CrudMutationPayload::create("c1".to_string(), serde_json::json!({ "name": "Alice" }))
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device:1").unwrap());
    let wire = to_value(create);
    let req = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [wire.clone()])
        .with_schema_version(1);
    let pushed = post_json::<_, SyncPushResponse<Value>>(router_a, SYNC_PUSH_PATH, &req).await;
    assert_eq!(pushed.accepted.len(), 1, "first push must accept");

    // Second push (retry from same client) against a router whose
    // migrator ALWAYS rejects. Same mutation_id, same wire payload.
    pocopine_server::__reset_for_test();
    let router_b = {
        let customers = resource(STREAM, customers.clone())
            .unwrap()
            .schema_version(2)
            .unwrap()
            .migrate_with(|from, to, _value| {
                Err(pocopine_sync::SyncError::backend(format!(
                    "migrator removed (from v{from} to v{to})"
                )))
            })
            .id(|row: &CustomerV2| row.id.clone())
            .version(|row: &CustomerV2| row.version)
            .mutation_log(log);
        let sync = pocopine_sync::SyncServer::builder()
            .public_stream(customers)
            .build();
        Server::new(Router::new())
            .plugin(sync_server_plugin(sync))
            .try_finalize()
            .unwrap()
    };
    let req_retry =
        SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [wire]).with_schema_version(1);
    let pushed =
        post_json::<_, SyncPushResponse<Value>>(router_b, SYNC_PUSH_PATH, &req_retry).await;

    assert_eq!(
        pushed.accepted.len(),
        1,
        "retry of already-accepted mutation must succeed despite failing migrator"
    );
    assert!(
        pushed.rejected.is_empty(),
        "retry must NOT surface the migrator failure, got: {:?}",
        pushed.rejected
    );
}

/// Repro for code-review finding #7: a push with explicit
/// `schema_version: 0` is rejected with `BadRequest` at the server
/// before reaching `migrate_payload`.
#[tokio::test]
async fn schema_version_zero_is_rejected_at_wire() {
    let _lock = registry_lock();
    let customers = Customers::default();
    let app = router_without_migrator(customers);
    let body = serde_json::json!({
        "protocol": pocopine_sync::SYNC_PROTOCOL_V1,
        "stream": STREAM,
        "mutations": [],
        "schema_version": 0,
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(SYNC_PUSH_PATH)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    // Server returns 200 OK with a `ServerResult::Err` payload because
    // sync handlers wrap their result in `Json(ServerResult<T>)`. The
    // BadRequest is inside the JSON body, not the HTTP status.
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine_core::ServerResult<SyncPushResponse<Value>> =
        serde_json::from_slice(&bytes).unwrap();
    match outer {
        Err(pocopine_core::ServerError::BadRequest(msg)) => {
            assert!(
                msg.contains("schema_version"),
                "expected BadRequest about schema_version, got: {msg}"
            );
        }
        other => panic!("expected BadRequest, got: {other:?}"),
    }
}

fn router_without_migrator(customers: Customers) -> Router {
    pocopine_server::__reset_for_test();
    let customers = resource(STREAM, customers)
        .unwrap()
        .schema_version(2)
        .unwrap()
        .id(|row: &CustomerV2| row.id.clone())
        .version(|row: &CustomerV2| row.version)
        .memory_mutation_log();
    let sync = pocopine_sync::SyncServer::builder()
        .public_stream(customers)
        .build();
    Server::new(Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .unwrap()
}

fn router_with_migrator(customers: Customers) -> Router {
    pocopine_server::__reset_for_test();
    let customers = resource(STREAM, customers)
        .unwrap()
        .schema_version(2)
        .unwrap()
        .migrate_with(migrate_v1_to_v2)
        .id(|row: &CustomerV2| row.id.clone())
        .version(|row: &CustomerV2| row.version)
        .memory_mutation_log();
    let sync = pocopine_sync::SyncServer::builder()
        .public_stream(customers)
        .build();
    Server::new(Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .unwrap()
}

fn to_value(mutation: ClientMutation<CrudMutationPayload<String, Value>>) -> ClientMutation<Value> {
    ClientMutation {
        id: mutation.id,
        key: mutation.key,
        op: mutation.op,
        base_version: mutation.base_version,
        payload: serde_json::to_value(mutation.payload).unwrap(),
        migration_outcome: None,
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
