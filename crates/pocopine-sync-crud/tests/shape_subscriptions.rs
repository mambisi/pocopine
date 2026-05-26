#![cfg(not(target_arch = "wasm32"))]
// `registry_lock` deliberately holds a `MutexGuard` across `.await`
// to serialize tests that share the process-global plugin registry.
#![allow(clippy::await_holding_lock)]

//! Macro `params(...)` integration tests for RFC 085 Batches 2 + 4.
//!
//! Batch 2 covered the typed `StreamParams` struct's serialization
//! and extraction in isolation. Batch 4 adds the end-to-end
//! server-side auto-validation: the macro now wires
//! `StreamParams::extract` into the resource's `validate_params`
//! hook automatically, so a wire `/open` request with bad params
//! is rejected with `BadRequest` before any `pull`/`push` runs.

use std::sync::{Mutex, MutexGuard, OnceLock};

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
    sync_server_plugin, RowVersion, StreamParams, SyncOpenRequest, SyncOpenResponse, SyncResult,
    SyncStreamName, SyncStreamSubscription, SYNC_OPEN_PATH,
};
use pocopine_sync_crud::{
    async_trait, params, resource, CrudRemoveResult, CrudSource, CrudWriteResult,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tower::ServiceExt;

fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
enum Status {
    Open,
    InProgress,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Issue {
    id: String,
    workspace_id: String,
    title: String,
    status: Status,
    version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct IssueDraft {
    workspace_id: String,
    title: String,
    status: Status,
}

#[derive(Clone, Default)]
struct Issues;

#[resource(
    name = "issues",
    schema_version = 1,
    params(
        workspace_id: String,
        assignee_id: Option<String>,
        status: params::InSet<Status>,
        title: params::Contains,
    ),
)]
#[async_trait]
impl CrudSource for Issues {
    type Id = String;
    type Row = Issue;
    type Draft = IssueDraft;

    async fn list(&self, _ctx: RequestContext, _limit: usize) -> SyncResult<Vec<Self::Row>> {
        Ok(Vec::new())
    }

    async fn get(&self, _ctx: RequestContext, _id: Self::Id) -> SyncResult<Option<Self::Row>> {
        Ok(None)
    }

    async fn create(
        &self,
        _ctx: RequestContext,
        _id: Self::Id,
        _draft: Self::Draft,
    ) -> SyncResult<Self::Row> {
        Err(pocopine_sync::SyncError::unsupported("test stub"))
    }

    async fn save(
        &self,
        _ctx: RequestContext,
        _id: Self::Id,
        _draft: Self::Draft,
        _base_version: Option<RowVersion>,
    ) -> SyncResult<CrudWriteResult<Self::Row>> {
        Err(pocopine_sync::SyncError::unsupported("test stub"))
    }

    async fn remove(
        &self,
        _ctx: RequestContext,
        _id: Self::Id,
        _base_version: Option<RowVersion>,
    ) -> SyncResult<CrudRemoveResult<Self::Row>> {
        Err(pocopine_sync::SyncError::unsupported("test stub"))
    }
}

#[test]
fn typed_stream_params_round_trips_through_wire() {
    let typed = issues::StreamParams {
        workspace_id: "W".to_string(),
        assignee_id: Some("alice".to_string()),
        status: params::InSet::new([Status::Open, Status::InProgress]).unwrap(),
        title: params::Contains::icontains("auth").unwrap(),
    };
    let wire = typed.serialize_params();

    // All non-None fields appear on the wire.
    assert!(wire.contains_key("workspace_id"));
    assert!(wire.contains_key("assignee_id"));
    assert!(wire.contains_key("status"));
    assert!(wire.contains_key("title"));

    let decoded = issues::StreamParams::extract(&wire).expect("decode");
    assert_eq!(decoded.workspace_id, "W");
    assert_eq!(decoded.assignee_id.as_deref(), Some("alice"));
    assert_eq!(decoded.status.values(), &[Status::Open, Status::InProgress]);
    assert_eq!(decoded.title.contains, "auth");
    assert!(!decoded.title.case_sensitive);
}

#[test]
fn typed_stream_params_omits_none_optional_field() {
    let typed = issues::StreamParams {
        workspace_id: "W".to_string(),
        assignee_id: None,
        status: params::InSet::new([Status::Open]).unwrap(),
        title: params::Contains::icontains("auth").unwrap(),
    };
    let wire = typed.serialize_params();

    // None-valued optionals are omitted from the wire shape.
    assert!(!wire.contains_key("assignee_id"));

    let decoded = issues::StreamParams::extract(&wire).expect("decode");
    assert!(decoded.assignee_id.is_none());
}

#[test]
fn extract_rejects_missing_required() {
    let empty = pocopine_sync::StreamParams::new();
    let err = issues::StreamParams::extract(&empty).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("missing required param") && msg.contains("workspace_id"),
        "unexpected: {msg}"
    );
}

#[test]
fn extract_rejects_unknown_param_key() {
    let mut wire = pocopine_sync::StreamParams::new();
    wire.insert(
        "workspace_id".to_string(),
        serde_json::Value::String("W".to_string()),
    );
    wire.insert("status".to_string(), serde_json::json!({ "in": ["open"] }));
    wire.insert(
        "title".to_string(),
        serde_json::json!({ "contains": "auth", "case_sensitive": false }),
    );
    wire.insert(
        "typo_key".to_string(),
        serde_json::Value::String("ignored".to_string()),
    );

    let err = issues::StreamParams::extract(&wire).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown param") && msg.contains("typo_key"),
        "unexpected: {msg}"
    );
}

#[test]
fn extract_rejects_wrong_shape_for_inset() {
    let mut wire = pocopine_sync::StreamParams::new();
    wire.insert(
        "workspace_id".to_string(),
        serde_json::Value::String("W".to_string()),
    );
    // status is declared as InSet<Status>, expects { "in": [...] }
    // but here we send a bare string — must reject.
    wire.insert(
        "status".to_string(),
        serde_json::Value::String("not-an-inset".to_string()),
    );
    wire.insert(
        "title".to_string(),
        serde_json::json!({ "contains": "auth", "case_sensitive": false }),
    );

    let err = issues::StreamParams::extract(&wire).unwrap_err();
    assert!(err.to_string().contains("params.status"));
}

// ============================================================
// Batch 4: end-to-end auto-validation via the macro-emitted
// `with_validate_params` hook on the CRUD resource builder.
// ============================================================

#[tokio::test]
async fn open_rejects_missing_required_param_via_auto_validation() {
    let _lock = registry_lock();
    let app = router();

    // No params at all — `workspace_id` is required, so `/open` returns
    // BadRequest from the auto-emitted validate_params hook BEFORE any
    // pull / push runs.
    let request = SyncOpenRequest::new([SyncStreamSubscription {
        stream: SyncStreamName::new("issues").unwrap(),
        params: StreamParams::new(),
    }]);
    let result = post_json_result::<_, SyncOpenResponse>(app, SYNC_OPEN_PATH, &request).await;
    match result {
        Err(pocopine_core::ServerError::BadRequest(msg)) => {
            assert!(
                msg.contains("missing required param") && msg.contains("workspace_id"),
                "unexpected: {msg}"
            );
        }
        other => panic!("expected BadRequest, got: {other:?}"),
    }
}

#[tokio::test]
async fn open_rejects_unknown_param_via_auto_validation() {
    let _lock = registry_lock();
    let app = router();

    let mut params = StreamParams::new();
    params.insert(
        "workspace_id".to_string(),
        serde_json::Value::String("W".to_string()),
    );
    params.insert(
        "typo_key".to_string(),
        serde_json::Value::String("ignored".to_string()),
    );
    let request = SyncOpenRequest::new([SyncStreamSubscription {
        stream: SyncStreamName::new("issues").unwrap(),
        params,
    }]);
    let result = post_json_result::<_, SyncOpenResponse>(app, SYNC_OPEN_PATH, &request).await;
    match result {
        Err(pocopine_core::ServerError::BadRequest(msg)) => {
            assert!(
                msg.contains("unknown param") && msg.contains("typo_key"),
                "unexpected: {msg}"
            );
        }
        other => panic!("expected BadRequest, got: {other:?}"),
    }
}

#[tokio::test]
async fn open_accepts_well_formed_params_via_auto_validation() {
    let _lock = registry_lock();
    let app = router();

    // All declared `params(...)` fields except Option<UserId> must
    // be set for the validator to accept.
    let mut params = StreamParams::new();
    params.insert(
        "workspace_id".to_string(),
        serde_json::Value::String("W".to_string()),
    );
    params.insert("status".to_string(), serde_json::json!({ "in": ["open"] }));
    params.insert(
        "title".to_string(),
        serde_json::json!({ "contains": "auth", "case_sensitive": false }),
    );
    let request = SyncOpenRequest::new([SyncStreamSubscription {
        stream: SyncStreamName::new("issues").unwrap(),
        params: params.clone(),
    }]);
    let response = post_json::<_, SyncOpenResponse>(app, SYNC_OPEN_PATH, &request).await;
    assert_eq!(response.streams.len(), 1);
    assert_eq!(response.streams[0].stream.as_str(), "issues");
    assert_eq!(response.streams[0].params, params);
}

fn router() -> Router {
    pocopine_server::__reset_for_test();
    let issues = issues::resource(Issues)
        .unwrap()
        .id(|row: &Issue| row.id.clone())
        .version(|row: &Issue| row.version)
        .memory_mutation_log();
    let sync = pocopine_sync::SyncServer::builder()
        .public_stream(issues)
        .build();
    Server::new(Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .unwrap()
}

async fn post_json<T, R>(router: Router, uri: &str, payload: &T) -> R
where
    T: Serialize,
    R: DeserializeOwned,
{
    post_json_result(router, uri, payload).await.unwrap()
}

async fn post_json_result<T, R>(
    router: Router,
    uri: &str,
    payload: &T,
) -> pocopine_core::ServerResult<R>
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
    serde_json::from_slice(&bytes).unwrap()
}
