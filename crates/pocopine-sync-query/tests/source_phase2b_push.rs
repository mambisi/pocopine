#![cfg(not(target_arch = "wasm32"))]

//! RFC 090 Phase 2b: end-to-end push lifecycle through `SourceResource`.
//!
//! Proves the new `.mutation_log(...)` builder + push handler:
//!
//! - Without `.mutation_log()`: push returns `unsupported`.
//! - With `MemoryMutationLog::new()`: a `MutationPayload::create(...)` posts
//!   through `/sync/v1/push`, the source's `create` runs once, the
//!   accepted mutation is logged, and the canonical row appears in
//!   the response.
//! - Idempotency: a second push of the same `mutation_id` with the
//!   same payload returns `accepted` WITHOUT re-running
//!   `Source::create`.
//! - Mismatched replay: a push of the same `mutation_id` with a
//!   DIFFERENT payload is rejected.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use pocopine_core::server::RequestContext;
use pocopine_server::Server;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_sync::{
    ClientMutation, ClientMutationDraft, MutationId, RowVersion, SYNC_PUSH_PATH, SyncOp,
    SyncPushRequest, SyncPushResponse, SyncResult, SyncServer, SyncStreamName, sync_server_plugin,
};
use pocopine_sync_query::Query;
use pocopine_sync_query::source::{
    DeleteResult, Source, SourceFuture, SourceStream, WriteResult, source as build_source,
};
use pocopine_sync_query::write::{MemoryMutationLog, MutationPayload};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

const STREAM: &str = "issues";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Issue {
    id: String,
    workspace_id: String,
    title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct IssueDraft {
    workspace_id: String,
    title: String,
}

#[derive(Clone, Default)]
struct IssuesSource {
    rows: Arc<Mutex<BTreeMap<String, Issue>>>,
    create_calls: Arc<Mutex<u32>>,
}

impl Source for IssuesSource {
    type Id = String;
    type Row = Issue;
    type Draft = IssueDraft;
    type Context = ();

    fn extract_context<'a>(
        &'a self,
        _ctx: RequestContext,
    ) -> SourceFuture<'a, SyncResult<Self::Context>> {
        Box::pin(async { Ok(()) })
    }

    fn list_stream<'a>(
        &'a self,
        _ctx: (),
        _query: &'a Query<Self::Row>,
    ) -> SourceStream<'a, Self::Row> {
        let rows: Vec<Self::Row> = self.rows.lock().unwrap().values().cloned().collect();
        Box::pin(futures::stream::iter(rows.into_iter().map(Ok)))
    }

    fn get<'a>(
        &'a self,
        _ctx: (),
        id: Self::Id,
    ) -> SourceFuture<'a, SyncResult<Option<Self::Row>>> {
        let row = self.rows.lock().unwrap().get(&id).cloned();
        Box::pin(async move { Ok(row) })
    }

    fn create<'a>(
        &'a self,
        _ctx: (),
        _meta: pocopine_sync_query::WriteMeta,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SourceFuture<'a, SyncResult<Self::Row>> {
        *self.create_calls.lock().unwrap() += 1;
        let issue = Issue {
            id: id.clone(),
            workspace_id: draft.workspace_id,
            title: draft.title,
        };
        self.rows.lock().unwrap().insert(id, issue.clone());
        Box::pin(async move { Ok(issue) })
    }

    fn update<'a>(
        &'a self,
        _ctx: (),
        _meta: pocopine_sync_query::WriteMeta,
        _id: Self::Id,
        _draft: Self::Draft,
        _expected_version: Option<RowVersion>,
    ) -> SourceFuture<'a, SyncResult<WriteResult<Self::Row>>> {
        Box::pin(async move { Err(pocopine_sync::SyncError::unsupported("stub: update")) })
    }

    fn delete<'a>(
        &'a self,
        _ctx: (),
        _meta: pocopine_sync_query::WriteMeta,
        _id: Self::Id,
        _expected_version: Option<RowVersion>,
    ) -> SourceFuture<'a, SyncResult<DeleteResult<Self::Row>>> {
        Box::pin(async move { Err(pocopine_sync::SyncError::unsupported("stub: delete")) })
    }
}

fn build_app(source: IssuesSource, with_log: bool) -> pocopine_server::axum::Router {
    pocopine_server::__reset_for_test();
    let mut builder = build_source(STREAM, source)
        .expect("stream name")
        .id(|r: &Issue| r.id.clone());
    if with_log {
        builder = builder.mutation_log(MemoryMutationLog::<Issue>::new());
    }
    let sync = SyncServer::builder().public_stream(builder).build();
    Server::new(pocopine_server::axum::Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .expect("server finalizes")
}

async fn push(
    app: &pocopine_server::axum::Router,
    mutation_id: &str,
    payload: MutationPayload<String, IssueDraft>,
) -> Result<SyncPushResponse<Value>, String> {
    let key = pocopine_sync::RowKey::new(payload.id().clone()).unwrap();
    let mutation = ClientMutationDraft::new(payload.sync_op(), payload)
        .row_key(key)
        .with_id(MutationId::new(mutation_id).unwrap());
    // Erase to ClientMutation<Value> for the wire (the push handler
    // is generic over payload).
    let typed_value = serde_json::to_value(&mutation.payload).unwrap();
    let mutation_value: ClientMutation<Value> = ClientMutation {
        id: mutation.id,
        key: mutation.key,
        op: mutation.op,
        base_version: mutation.base_version,
        payload: typed_value,
        migration_outcome: None,
    };

    let request = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [mutation_value]);
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
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine_core::ServerResult<SyncPushResponse<Value>> =
        serde_json::from_slice(&bytes).unwrap();
    if status != StatusCode::OK {
        return Err(format!("status {status}: {outer:?}"));
    }
    outer.map_err(|e| format!("{e:?}"))
}

#[tokio::test]
async fn push_without_explicit_mutation_log_uses_in_memory_default() {
    // T2.2: omitting `.mutation_log(...)` no longer hard-rejects
    // push. The builder defaults to `MemoryMutationLog::new()` and
    // the first push emits a one-shot `tracing::warn!` noting that
    // the implicit log won't survive restart. We assert the
    // behaviour (push succeeds, source ran) here; tracing-side
    // assertion lives in the observability test suite where a
    // collector is installed.
    let source = IssuesSource::default();
    let app = build_app(source.clone(), /* with_log */ false);

    let result = push(
        &app,
        "m_first",
        MutationPayload::create(
            "i1".into(),
            IssueDraft {
                workspace_id: "W1".into(),
                title: "Auth bug".into(),
            },
        ),
    )
    .await;
    assert!(
        result.is_ok(),
        "push must succeed under the default in-memory log; got {result:?}"
    );
    assert_eq!(
        *source.create_calls.lock().unwrap(),
        1,
        "source.create must run exactly once"
    );

    // Replay the same mutation id and confirm idempotency still
    // holds via the default log — i.e. it's a real log, not a no-op.
    let replay = push(
        &app,
        "m_first",
        MutationPayload::create(
            "i1".into(),
            IssueDraft {
                workspace_id: "W1".into(),
                title: "Auth bug".into(),
            },
        ),
    )
    .await;
    assert!(
        replay.is_ok(),
        "replay must succeed (short-circuit on prior)"
    );
    assert_eq!(
        *source.create_calls.lock().unwrap(),
        1,
        "source.create must NOT run twice — default log dedupes"
    );
}

#[tokio::test]
async fn push_with_log_dispatches_create_and_returns_canonical_row() {
    let source = IssuesSource::default();
    let create_calls = source.create_calls.clone();
    let app = build_app(source, /* with_log */ true);

    let response = push(
        &app,
        "m_first",
        MutationPayload::create(
            "i1".into(),
            IssueDraft {
                workspace_id: "W1".into(),
                title: "Auth bug".into(),
            },
        ),
    )
    .await
    .expect("push succeeds");

    assert_eq!(response.accepted.len(), 1);
    assert_eq!(response.accepted[0].as_str(), "m_first");
    assert!(response.rejected.is_empty());
    assert!(response.conflicts.is_empty());
    assert_eq!(response.rows.len(), 1);
    assert_eq!(
        response.rows[0].value.get("title").and_then(|v| v.as_str()),
        Some("Auth bug")
    );
    assert_eq!(
        *create_calls.lock().unwrap(),
        1,
        "Source::create should run exactly once"
    );
}

#[tokio::test]
async fn idempotent_replay_skips_source_create() {
    let source = IssuesSource::default();
    let create_calls = source.create_calls.clone();
    let app = build_app(source, /* with_log */ true);

    let payload = MutationPayload::create(
        "i1".into(),
        IssueDraft {
            workspace_id: "W1".into(),
            title: "Auth bug".into(),
        },
    );

    let first = push(&app, "m_replay", payload.clone()).await.unwrap();
    assert_eq!(first.accepted[0].as_str(), "m_replay");

    let second = push(&app, "m_replay", payload).await.unwrap();
    assert_eq!(second.accepted[0].as_str(), "m_replay");

    assert_eq!(
        *create_calls.lock().unwrap(),
        1,
        "replay must hit the idempotency log, not Source::create"
    );
}

#[tokio::test]
async fn mismatched_replay_is_rejected() {
    let source = IssuesSource::default();
    let app = build_app(source, /* with_log */ true);

    let original = MutationPayload::create(
        "i1".into(),
        IssueDraft {
            workspace_id: "W1".into(),
            title: "Auth bug".into(),
        },
    );
    let _ = push(&app, "m_mismatch", original).await.unwrap();

    // Same mutation_id, DIFFERENT payload contents.
    let mutated = MutationPayload::create(
        "i1".into(),
        IssueDraft {
            workspace_id: "W1".into(),
            title: "Different title".into(),
        },
    );
    let result = push(&app, "m_mismatch", mutated).await.unwrap();
    assert!(result.accepted.is_empty());
    assert_eq!(result.rejected.len(), 1);
    assert!(
        result.rejected[0]
            .reason
            .contains("already accepted with different contents")
    );
}

#[tokio::test]
async fn create_with_base_version_is_rejected() {
    // RFC 090 Phase 2b push handler: create does NOT accept a base
    // row version. Mirrors CRUD's existing semantic.
    let source = IssuesSource::default();
    let app = build_app(source, /* with_log */ true);

    let payload: MutationPayload<String, IssueDraft> = MutationPayload::create(
        "i1".into(),
        IssueDraft {
            workspace_id: "W1".into(),
            title: "Auth bug".into(),
        },
    );
    let key = pocopine_sync::RowKey::new(payload.id().clone()).unwrap();
    let mutation = ClientMutationDraft::new(SyncOp::Upsert, payload)
        .row_key(key)
        .base_row_version(Some(RowVersion::new("1").unwrap()))
        .with_id(MutationId::new("m_bv").unwrap());
    let mutation_value: ClientMutation<Value> = ClientMutation {
        id: mutation.id,
        key: mutation.key,
        op: mutation.op,
        base_version: mutation.base_version,
        payload: serde_json::to_value(&mutation.payload).unwrap(),
        migration_outcome: None,
    };
    let request = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [mutation_value]);
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
    let response = outer.unwrap();
    assert_eq!(response.rejected.len(), 1);
    assert!(
        response.rejected[0]
            .reason
            .contains("create does not accept a base row version")
    );
}
