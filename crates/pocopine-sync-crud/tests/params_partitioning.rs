#![cfg(not(target_arch = "wasm32"))]

//! End-to-end test for RFC 088 §C / CRUD `.params_of` integration.
//!
//! This is the test that proves CRUD + Query compose: a CRUD source
//! (typed Draft + idempotent writes) gets Query-grade live wakeup
//! precision (per-`(stream, params_hash)` topics) by attaching the
//! macro-generated `row_to_params_typed` via `.params_of`. See
//! `docs/sync-crud-query-composition.md` for the bigger picture.
//!
//! Asserts that a push affecting workspace `W1` wakes only the
//! `W1`-scoped per-params subscriber — the `W2` subscriber stays
//! silent. The bare-stream subscriber still wakes (back-compat path).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::BodyExt;
use pocopine_auth::RequestContext;
use pocopine_events::{MemoryEventBackend, SharedEventBackend};
use pocopine_live::{build_live_stream_url, routes, LiveHub, LIVE_STREAM_PATH};
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_server::axum::Router;
use pocopine_server::Server;
use pocopine_sync::{
    stream_params_hash, sync_server_plugin, sync_stream_params_tag, sync_stream_tag,
    ClientMutation, MutationId, RowVersion, StreamParams, SyncError, SyncPushRequest,
    SyncPushResponse, SyncServer, SyncStreamName, SYNC_PUSH_PATH,
};
use pocopine_sync_crud::{
    async_trait, resource, CrudMutationPayload, CrudRemoveResult, CrudSource, CrudWriteResult,
    MemoryCrudMutationLog,
};
use pocopine_sync_query::query_resource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

const STREAM: &str = "issues";

// The macro emits an `issues` module with `row_to_params_typed`
// alongside the predicate + DSL items. `.params_of(issues::row_to_params_typed)`
// plugs it into CRUD without any manual `row_to_params` override.
#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    #[query_param(required)]
    pub workspace_id: String,
    #[query_param]
    pub title: String,
    pub version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct IssueDraft {
    workspace_id: String,
    title: String,
}

#[derive(Clone, Default)]
struct IssuesSource {
    rows: Arc<Mutex<BTreeMap<String, Issue>>>,
}

#[async_trait]
impl CrudSource for IssuesSource {
    type Id = String;
    type Row = Issue;
    type Draft = IssueDraft;

    async fn list(
        &self,
        _ctx: RequestContext,
        limit: usize,
    ) -> pocopine_sync::SyncResult<Vec<Self::Row>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows.values().take(limit).cloned().collect())
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
        let issue = Issue {
            id: id.clone(),
            workspace_id: draft.workspace_id,
            title: draft.title,
            version: 1,
        };
        self.rows.lock().unwrap().insert(id, issue.clone());
        Ok(issue)
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
            .ok_or_else(|| SyncError::backend("missing issue"))?;
        row.workspace_id = draft.workspace_id;
        row.title = draft.title;
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

fn build_app() -> (Router, SyncServer) {
    pocopine_server::__reset_for_test();
    let backend: SharedEventBackend = Arc::new(MemoryEventBackend::new());
    let sync = SyncServer::builder()
        .events(backend.clone())
        .public_stream(
            resource(STREAM, IssuesSource::default())
                .expect("stream name")
                .id(|r: &Issue| r.id.clone())
                .mutation_log(MemoryCrudMutationLog::<Issue>::new())
                // The whole point of the test: the typed projector
                // turns row.workspace_id into the partition. The
                // macro guarantees agreement with the client-side
                // `partition_for_topic` hash (same fields, same
                // canonical JSON encoding, same FNV-1a constants).
                .params_of(issues::row_to_params_typed),
        )
        .build();

    let topic_prefixes = sync.live_topic_prefixes();
    let default_topics = sync.live_topics().expect("default live topics");
    let app = Server::new(routes(
        LiveHub::new(backend)
            .allow_topic_prefixes(topic_prefixes)
            .default_topics(default_topics),
    ))
    .plugin(sync_server_plugin(sync.clone()))
    .try_finalize()
    .expect("server finalizes");

    (app, sync)
}

#[tokio::test]
async fn push_into_w1_wakes_only_w1_per_params_subscriber() {
    let (app, _sync) = build_app();

    let w1_params: StreamParams = [("workspace_id".to_string(), serde_json::json!("W1"))]
        .into_iter()
        .collect();
    let w2_params: StreamParams = [("workspace_id".to_string(), serde_json::json!("W2"))]
        .into_iter()
        .collect();
    let w1_hash = stream_params_hash(STREAM, &w1_params);
    let w2_hash = stream_params_hash(STREAM, &w2_params);
    assert_ne!(
        w1_hash, w2_hash,
        "distinct workspaces hash to distinct partitions"
    );

    let mut w1_body = open_live(&app, &sync_stream_params_tag(STREAM, w1_hash)).await;
    let mut w2_body = open_live(&app, &sync_stream_params_tag(STREAM, w2_hash)).await;
    let mut bare_body = open_live(&app, &sync_stream_tag(STREAM)).await;

    expect_ready(&mut w1_body).await;
    expect_ready(&mut w2_body).await;
    expect_ready(&mut bare_body).await;

    let pushed = push_issue(
        &app,
        "i_w1_first",
        IssueDraft {
            workspace_id: "W1".into(),
            title: "Auth bug".into(),
        },
    )
    .await;
    assert!(
        pushed.rejected.is_empty(),
        "push rejected unexpectedly: {:?}",
        pushed.rejected
    );
    assert!(
        pushed.conflicts.is_empty(),
        "push conflicted unexpectedly: {:?}",
        pushed.conflicts
    );
    assert_eq!(pushed.accepted.len(), 1);
    assert_eq!(pushed.accepted[0].as_str(), "m:i_w1_first");

    let w1_frame = read_sse_frame(&mut w1_body).await.expect("W1 should wake");
    assert!(
        w1_frame.contains(&format!("sync:stream:{STREAM}:{w1_hash:016x}")),
        "W1 frame should carry the per-params tag, got: {w1_frame}"
    );

    let bare_frame = read_sse_frame(&mut bare_body)
        .await
        .expect("bare topic should wake");
    assert!(
        bare_frame.contains(&format!("sync:stream:{STREAM}")),
        "bare frame should carry the bare tag, got: {bare_frame}"
    );

    // The critical assertion: W2 must NOT receive a wakeup. We give
    // the publish path 200ms — same order of magnitude as the W1
    // delivery, which arrives well inside this.
    let w2_silent = tokio::time::timeout(Duration::from_millis(200), w2_body.frame()).await;
    assert!(
        w2_silent.is_err(),
        "W2 must NOT receive a wakeup for a W1-scoped row, but got a frame within 200ms"
    );
}

async fn open_live(app: &Router, tag: &str) -> Body {
    let url = build_live_stream_url(LIVE_STREAM_PATH, &[], &[format!("query:{tag}")], None)
        .expect("live URL");
    let response = app
        .clone()
        .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "SSE handshake failed for {tag}"
    );
    response.into_body()
}

async fn expect_ready(body: &mut Body) {
    let ready = read_sse_frame(body)
        .await
        .expect("first SSE frame is `ready`");
    assert!(
        ready.contains("event: ready"),
        "expected ready frame, got: {ready}"
    );
}

async fn push_issue(app: &Router, id: &str, draft: IssueDraft) -> SyncPushResponse<Value> {
    let typed = CrudMutationPayload::create(id.to_string(), draft)
        .into_sync_draft()
        .unwrap()
        .with_id(MutationId::new(format!("m:{id}")).unwrap());
    let mutation: ClientMutation<Value> = ClientMutation {
        id: typed.id,
        key: typed.key,
        op: typed.op,
        base_version: typed.base_version,
        payload: serde_json::to_value(typed.payload).unwrap(),
        migration_outcome: None,
    };
    let request = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [mutation]);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(SYNC_PUSH_PATH)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine_core::ServerResult<SyncPushResponse<Value>> =
        serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
}

async fn read_sse_frame(body: &mut Body) -> Option<String> {
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .ok()??
        .ok()?;
    let data = frame.into_data().ok()?;
    Some(String::from_utf8_lossy(&data).into_owned())
}
