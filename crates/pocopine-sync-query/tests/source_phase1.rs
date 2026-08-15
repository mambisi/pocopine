#![cfg(not(target_arch = "wasm32"))]

//! Phase 1 of RFC 090: end-to-end integration test for the new
//! `Source` trait + `SyncStreamSource` adapter.
//!
//! Proves the **Query-aware list** unlock: a Source impl can
//! introspect `&Query<Row>` and translate `query.params()` into a
//! filter applied BEFORE rows leave storage. CRUD's `list(ctx,
//! limit)` couldn't do this — every workspace's rows came back
//! and the predicate filtered client-side.
//!
//! Two workspaces (`W1` / `W2`) live in the source. A pull request
//! with `params.workspace_id = "W1"` returns only `W1` rows. A
//! pull with `workspace_id = "W2"` returns only `W2` rows. An
//! unfiltered pull (no params) returns everything.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use pocopine_core::server::RequestContext;
use pocopine_server::Server;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_sync::{
    SYNC_PULL_PATH, SyncPullMode, SyncPullRequest, SyncPullResponse, SyncResult, SyncServer,
    SyncStreamName, sync_server_plugin,
};
use pocopine_sync_query::Query;
use pocopine_sync_query::source::{
    DeleteResult, Source, SourceFuture, SourceStream, WriteResult, query_from_pull_request,
    source as build_source,
};
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

/// Test instrumentation — records every (query.params,
/// query.limit) pair the trait method observed. The integration
/// test asserts the source SAW the filter, not just that the
/// output happened to be right.
type ObservedCalls = Arc<Mutex<Vec<(BTreeMap<String, Value>, Option<u32>)>>>;

/// A Source impl that demonstrates the Query-aware filter pattern.
/// In a real app this would translate `query.params()` into SQL
/// (`WHERE workspace_id = ? AND status IN (?)`); here we filter
/// an in-memory `Vec<Issue>` so the test stays small.
#[derive(Clone)]
struct IssuesSource {
    rows: Arc<Mutex<Vec<Issue>>>,
    /// (row key, workspace_id) pairs the source retains as deletion
    /// records — the backing table for `list_tombstones`.
    deleted: Arc<Mutex<Vec<(String, String)>>>,
    observed: ObservedCalls,
}

impl IssuesSource {
    fn seeded() -> Self {
        let rows = vec![
            Issue {
                id: "i_w1_a".into(),
                workspace_id: "W1".into(),
                title: "Auth bug".into(),
            },
            Issue {
                id: "i_w1_b".into(),
                workspace_id: "W1".into(),
                title: "Routing".into(),
            },
            Issue {
                id: "i_w2_a".into(),
                workspace_id: "W2".into(),
                title: "Different tenant".into(),
            },
        ];
        Self {
            rows: Arc::new(Mutex::new(rows)),
            deleted: Arc::new(Mutex::new(Vec::new())),
            observed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_deleted(self, key: &str, workspace: &str) -> Self {
        self.deleted
            .lock()
            .unwrap()
            .push((key.to_string(), workspace.to_string()));
        self
    }
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
        query: &'a Query<Self::Row>,
    ) -> SourceStream<'a, Self::Row> {
        // Record what the source saw — instrumentation for the
        // integration test, not part of the production pattern.
        self.observed
            .lock()
            .unwrap()
            .push((query.params().clone(), query.limit()));

        let workspace_filter = query
            .params()
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let limit = query.limit().unwrap_or(u32::MAX) as usize;

        let rows = self.rows.lock().unwrap().clone();
        let filtered: Vec<Self::Row> = rows
            .into_iter()
            .filter(|r| {
                workspace_filter
                    .as_deref()
                    .is_none_or(|w| r.workspace_id == w)
            })
            .take(limit)
            .collect();
        Box::pin(futures::stream::iter(filtered.into_iter().map(Ok)))
    }

    fn list_tombstones<'a>(
        &'a self,
        _ctx: (),
        query: &'a Query<Self::Row>,
    ) -> SourceFuture<'a, SyncResult<Vec<pocopine_sync::SyncTombstone>>> {
        // Deletion records honor the same tenant filter as
        // `list_stream` — a W1 subscription never learns about W2's
        // deletes.
        let workspace_filter = query
            .params()
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let deleted = self.deleted.lock().unwrap().clone();
        Box::pin(async move {
            deleted
                .into_iter()
                .filter(|(_, w)| workspace_filter.as_deref().is_none_or(|f| w == f))
                .map(|(key, _)| pocopine_sync::SyncTombstone::new(key))
                .collect()
        })
    }

    fn scope(&self, _ctx: &()) -> SyncResult<Option<pocopine_sync::SyncScope>> {
        Ok(Some(pocopine_sync::SyncScope::new("tenant:test")?))
    }

    fn get<'a>(
        &'a self,
        _ctx: (),
        id: Self::Id,
    ) -> SourceFuture<'a, SyncResult<Option<Self::Row>>> {
        let rows = self.rows.lock().unwrap().clone();
        Box::pin(async move { Ok(rows.into_iter().find(|r| r.id == id)) })
    }

    fn create<'a>(
        &'a self,
        _ctx: (),
        _meta: pocopine_sync_query::WriteMeta,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SourceFuture<'a, SyncResult<Self::Row>> {
        let rows = self.rows.clone();
        Box::pin(async move {
            let issue = Issue {
                id: id.clone(),
                workspace_id: draft.workspace_id,
                title: draft.title,
            };
            rows.lock().unwrap().push(issue.clone());
            Ok(issue)
        })
    }

    fn update<'a>(
        &'a self,
        _ctx: (),
        _meta: pocopine_sync_query::WriteMeta,
        _id: Self::Id,
        _draft: Self::Draft,
        _expected_version: Option<pocopine_sync::RowVersion>,
    ) -> SourceFuture<'a, SyncResult<WriteResult<Self::Row>>> {
        Box::pin(async move { Err(pocopine_sync::SyncError::unsupported("Phase 1 stub: save")) })
    }

    fn delete<'a>(
        &'a self,
        _ctx: (),
        _meta: pocopine_sync_query::WriteMeta,
        _id: Self::Id,
        _expected_version: Option<pocopine_sync::RowVersion>,
    ) -> SourceFuture<'a, SyncResult<DeleteResult<Self::Row>>> {
        Box::pin(async move {
            Err(pocopine_sync::SyncError::unsupported(
                "Phase 1 stub: remove",
            ))
        })
    }
}

fn build_app(source: IssuesSource) -> pocopine_server::axum::Router {
    pocopine_server::__reset_for_test();
    let sync = SyncServer::builder()
        .public_stream(
            build_source(STREAM, source)
                .expect("stream name")
                .id(|r: &Issue| r.id.clone()),
        )
        .build();
    Server::new(pocopine_server::axum::Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .expect("server finalizes")
}

async fn pull(
    app: &pocopine_server::axum::Router,
    workspace: Option<&str>,
) -> SyncPullResponse<Value> {
    let mut request = SyncPullRequest::new(SyncStreamName::new(STREAM).unwrap());
    if let Some(w) = workspace {
        request
            .params
            .insert("workspace_id".to_string(), serde_json::json!(w));
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(SYNC_PULL_PATH)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine_core::ServerResult<SyncPullResponse<Value>> =
        serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
}

#[tokio::test]
async fn snapshot_pull_carries_tenant_filtered_tombstones_and_scope() {
    // The adapter attaches `Source::list_tombstones` to every
    // snapshot response (filtered by the same query the rows were)
    // and the pull handler stamps `Source::scope` — the two halves
    // of "a server delete propagates with confidence, and only to
    // the principal it belongs to".
    let source = IssuesSource::seeded()
        .with_deleted("i_w1_gone", "W1")
        .with_deleted("i_w2_gone", "W2");
    let app = build_app(source);

    let w1 = pull(&app, Some("W1")).await;
    let keys: Vec<&str> = w1.tombstones.iter().map(|t| t.key.as_str()).collect();
    assert_eq!(keys, vec!["i_w1_gone"], "W2's delete must not leak to W1");
    assert_eq!(
        w1.scope,
        Some(pocopine_sync::SyncScope::new("tenant:test").unwrap()),
        "pull handler stamps the typed Source::scope"
    );

    let all = pull(&app, None).await;
    assert_eq!(
        all.tombstones.len(),
        2,
        "unfiltered pull sees every tombstone"
    );
}

#[tokio::test]
async fn cap_filled_snapshots_advertise_has_more() {
    // Codex R1 P2: the adapter clamps even unlimited queries to
    // max_snapshot_rows, and the client can't see that implicit cap.
    // A page that fills the effective limit must say `has_more` so
    // client-side eviction reporting doesn't misread pagination as
    // loss.
    pocopine_server::__reset_for_test();
    let sync = SyncServer::builder()
        .public_stream(
            build_source(STREAM, IssuesSource::seeded())
                .expect("stream name")
                .max_snapshot_rows(2)
                .expect("cap")
                .id(|r: &Issue| r.id.clone()),
        )
        .build();
    let app = Server::new(pocopine_server::axum::Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .expect("server finalizes");

    // Three seeded rows, cap 2: the unfiltered page fills the cap.
    let all = pull(&app, None).await;
    assert_eq!(all.rows.len(), 2);
    assert!(all.has_more, "a cap-filled page may be truncated");

    // One W2 row, cap 2: genuinely complete.
    let w2 = pull(&app, Some("W2")).await;
    assert_eq!(w2.rows.len(), 1);
    assert!(!w2.has_more, "an under-cap page is a full view");
}

#[tokio::test]
async fn workspace_filter_pushes_into_source_not_client() {
    // The headline Phase 1 test: a /pull with params.workspace_id =
    // "W1" routes through the SyncStreamSource adapter, which
    // reconstructs a typed Query<Issue> and hands it to
    // Source::list. The source sees the filter and returns only W1
    // rows. CRUD's CrudSource::list(ctx, limit) had no path to
    // observe the workspace_id — every workspace's rows came back
    // and the macro's predicate filtered client-side.
    let source = IssuesSource::seeded();
    let observed = source.observed.clone();
    let app = build_app(source);

    let response = pull(&app, Some("W1")).await;
    assert_eq!(response.mode, SyncPullMode::Snapshot);
    assert_eq!(response.rows.len(), 2);
    for row in &response.rows {
        assert_eq!(
            row.value.get("workspace_id").and_then(|v| v.as_str()),
            Some("W1"),
            "Source should not have returned non-W1 rows"
        );
    }

    // Cross-check that the source's `list` actually OBSERVED the
    // workspace filter — distinguishes "filter ran in storage"
    // from "filter ran in the test harness."
    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(
        observed[0].0.get("workspace_id").and_then(|v| v.as_str()),
        Some("W1"),
        "Source should have seen workspace_id=W1 in query.params()"
    );
}

#[tokio::test]
async fn distinct_workspaces_yield_distinct_filters() {
    // Two pulls with different workspaces hit the source with
    // different `query.params()` and get disjoint result sets.
    // This is the multi-tenant contract Phase 1 makes possible.
    let source = IssuesSource::seeded();
    let app = build_app(source);

    let w1 = pull(&app, Some("W1")).await;
    let w2 = pull(&app, Some("W2")).await;

    assert_eq!(w1.rows.len(), 2);
    assert_eq!(w2.rows.len(), 1);
    assert_eq!(
        w2.rows[0]
            .value
            .get("workspace_id")
            .and_then(|v| v.as_str()),
        Some("W2")
    );

    // Sanity: no row from W2 appears in W1's response (would
    // signal the filter regressed to "ignored").
    for row in &w1.rows {
        assert_ne!(
            row.value.get("workspace_id").and_then(|v| v.as_str()),
            Some("W2")
        );
    }
}

#[tokio::test]
async fn unfiltered_pull_returns_everything() {
    // A /pull with no params reconstructs a Query with no filter.
    // The source's pattern-match returns all rows — preserves the
    // CRUD-style "list(ctx, limit)" semantics when callers don't
    // care about filtering.
    let source = IssuesSource::seeded();
    let app = build_app(source);

    let all = pull(&app, None).await;
    assert_eq!(all.rows.len(), 3);
}

#[tokio::test]
async fn version_extractor_populates_sync_row_version() {
    // Code-review finding #1: without `.version()`, pulled rows ship
    // with version=None and Phase 2's `Source::save` base_version
    // check becomes meaningless. This test wires a version extractor
    // and asserts each pulled row carries the extracted version on
    // the wire.
    use pocopine_sync::RowVersion;

    pocopine_server::__reset_for_test();
    let sync = SyncServer::builder()
        .public_stream(
            build_source(STREAM, IssuesSource::seeded())
                .expect("stream name")
                .id(|r: &Issue| r.id.clone())
                // Synthetic version derived from the title length —
                // not meaningful semantically, just deterministic so
                // the assertion below can predict the expected
                // string.
                .version_field(|r: &Issue| {
                    Ok(Some(
                        RowVersion::new(r.title.len().to_string()).expect("valid version"),
                    ))
                }),
        )
        .build();
    let app = Server::new(pocopine_server::axum::Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .expect("server finalizes");

    let response = pull(&app, Some("W1")).await;
    assert_eq!(response.rows.len(), 2);
    for row in &response.rows {
        let title = row.value.get("title").and_then(|v| v.as_str()).unwrap();
        let version = row
            .version
            .as_ref()
            .expect("version extractor ran")
            .as_str();
        assert_eq!(version, title.len().to_string());
    }
}

#[tokio::test]
async fn limit_clamped_to_max_snapshot_rows_before_calling_source() {
    // Code-review finding #3: a buggy Source::list that ignores
    // query.limit() could allocate unbounded memory. Defense in
    // depth: the adapter clamps query.limit() to max_snapshot_rows
    // BEFORE calling list. This test asserts the source SEES the
    // clamped value (and so a well-behaved source can't even ask
    // for more than the cap).
    pocopine_server::__reset_for_test();
    let source = IssuesSource::seeded();
    let observed = source.observed.clone();
    let sync = SyncServer::builder()
        .public_stream(
            build_source(STREAM, source)
                .expect("stream name")
                .max_snapshot_rows(2)
                .expect("nonzero max")
                .id(|r: &Issue| r.id.clone()),
        )
        .build();
    let app = Server::new(pocopine_server::axum::Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .expect("server finalizes");

    // Pull with no explicit wire-limit → the adapter substitutes
    // the configured cap.
    let _ = pull(&app, None).await;
    let observed = observed.lock().unwrap();
    assert_eq!(
        observed.last().expect("source.list ran").1,
        Some(2),
        "adapter must clamp query.limit() to the configured cap"
    );
}

#[test]
fn query_reconstruction_smoke_test() {
    // Pure unit-level check on the wire→Query function — covers
    // the regression path where the SyncPullRequest field shape
    // changes (e.g. `limit: u32` → `Option<u32>`) and the
    // reconstructor silently drops the filter.
    let mut request = SyncPullRequest::new(SyncStreamName::new("issues").unwrap());
    request
        .params
        .insert("workspace_id".to_string(), serde_json::json!("W1"));
    request.limit = 25;
    let q: Query<Issue> = query_from_pull_request(&request);
    assert_eq!(
        q.params().get("workspace_id").unwrap(),
        &serde_json::json!("W1")
    );
    assert_eq!(q.limit(), Some(25));
}
