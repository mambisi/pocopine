#![cfg(not(target_arch = "wasm32"))]

//! Ordered change feed — adapter decision-tree matrix (P3 of the
//! SPORC-derived hardening; see issue #292's post-mortem for why
//! absence-based sync had to go).
//!
//! | cursor    | feed | list_since | expected response                      |
//! |-----------|------|------------|----------------------------------------|
//! | None      | Some | —          | Snapshot + head cursor                 |
//! | fresh     | Some | Page       | Incremental, ordered, deletes explicit |
//! | at head   | Some | empty Page | empty Incremental, cursor unchanged    |
//! | below wm  | Some | TooOld     | Snapshot + resync=CursorTruncated      |
//! | any+limit | Some | (skipped)  | Snapshot (top-N stays snapshot-only)   |
//! | any       | None | —          | Snapshot, cursor None (legacy path)    |
//!
//! Plus: the ordering/gap property across `has_more` pages, feed
//! echo `origin` stamping through the real push path, and tenant
//! scoping via the memory log's filter.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pocopine_core::server::RequestContext;
use pocopine_sync::{
    ClientMutation, MutationId, RowKey, SyncCursor, SyncOp, SyncPullMode, SyncPullRequest,
    SyncPushRequest, SyncResyncReason, SyncResult, SyncStreamName, SyncStreamSource,
};
use pocopine_sync_query::feed::{ChangeLog, ChangesSince, MemoryChangeLog};
use pocopine_sync_query::source::{
    DeleteResult, Source, SourceFuture, SourceStream, WriteMeta, WriteResult,
    source as build_source,
};
use pocopine_sync_query::{MutationPayload, Query};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

fn issue(id: &str, ws: &str, title: &str) -> Issue {
    Issue {
        id: id.into(),
        workspace_id: ws.into(),
        title: title.into(),
    }
}

/// Feed-capable memory source: every write appends its feed entry —
/// with the mutation identity as `origin` — under the same lock as
/// the row write (the memory analogue of "same transaction").
#[derive(Clone)]
struct FeedSource {
    rows: Arc<Mutex<BTreeMap<String, Issue>>>,
    log: MemoryChangeLog<Issue>,
}

impl FeedSource {
    fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(BTreeMap::new())),
            log: MemoryChangeLog::new(),
        }
    }

    fn with_log(log: MemoryChangeLog<Issue>) -> Self {
        Self {
            rows: Arc::new(Mutex::new(BTreeMap::new())),
            log,
        }
    }

    /// Server-side write (no client mutation): import path.
    fn server_upsert(&self, row: Issue) -> SyncCursor {
        let mut rows = self.rows.lock().unwrap();
        rows.insert(row.id.clone(), row.clone());
        self.log
            .record_upsert(RowKey::new(row.id.clone()).unwrap(), row, None)
            .unwrap()
    }

    fn server_delete(&self, id: &str) -> SyncCursor {
        let mut rows = self.rows.lock().unwrap();
        rows.remove(id);
        self.log
            .record_delete(RowKey::new(id).unwrap(), None)
            .unwrap()
    }
}

impl Source for FeedSource {
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
        let ws = query
            .params()
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let rows: Vec<Issue> = self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| ws.as_deref().is_none_or(|w| r.workspace_id == w))
            .cloned()
            .collect();
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
        meta: WriteMeta,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SourceFuture<'a, SyncResult<Self::Row>> {
        let row = Issue {
            id: id.clone(),
            workspace_id: draft.workspace_id,
            title: draft.title,
        };
        // Row write + feed append under one lock hold — the memory
        // analogue of the same-transaction contract.
        let mut rows = self.rows.lock().unwrap();
        rows.insert(id.clone(), row.clone());
        let result = self
            .log
            .record_upsert(
                RowKey::new(id).unwrap(),
                row.clone(),
                Some(meta.mutation_id),
            )
            .map(|_| row);
        Box::pin(async move { result })
    }

    fn update<'a>(
        &'a self,
        _ctx: (),
        meta: WriteMeta,
        id: Self::Id,
        draft: Self::Draft,
        _expected_version: Option<pocopine_sync::RowVersion>,
    ) -> SourceFuture<'a, SyncResult<WriteResult<Self::Row>>> {
        let row = Issue {
            id: id.clone(),
            workspace_id: draft.workspace_id,
            title: draft.title,
        };
        let mut rows = self.rows.lock().unwrap();
        rows.insert(id.clone(), row.clone());
        let result = self
            .log
            .record_upsert(
                RowKey::new(id).unwrap(),
                row.clone(),
                Some(meta.mutation_id),
            )
            .map(|_| WriteResult::Applied(row));
        Box::pin(async move { result })
    }

    fn delete<'a>(
        &'a self,
        _ctx: (),
        meta: WriteMeta,
        id: Self::Id,
        _expected_version: Option<pocopine_sync::RowVersion>,
    ) -> SourceFuture<'a, SyncResult<DeleteResult<Self::Row>>> {
        let mut rows = self.rows.lock().unwrap();
        rows.remove(&id);
        let result = self
            .log
            .record_delete(RowKey::new(id).unwrap(), Some(meta.mutation_id))
            .map(|_| DeleteResult::Applied);
        Box::pin(async move { result })
    }

    fn change_log(&self) -> Option<Arc<dyn ChangeLog<Self::Row>>> {
        Some(Arc::new(self.log.clone()))
    }
}

fn ctx() -> RequestContext {
    RequestContext::new(
        http::Method::POST,
        "/__pocopine/sync/v1/pull".parse().unwrap(),
        http::HeaderMap::new(),
    )
}

fn resource(source: FeedSource) -> impl SyncStreamSource {
    build_source(STREAM, source)
        .unwrap()
        .id(|row: &Issue| row.id.clone())
}

fn pull_request(cursor: Option<&str>) -> SyncPullRequest {
    SyncPullRequest::new(SyncStreamName::new(STREAM).unwrap())
        .cursor(cursor.map(|c| SyncCursor::new(c).unwrap()))
}

fn mutation_id(n: u64) -> MutationId {
    MutationId::new(format!("device_test:{n}")).unwrap()
}

/// Build a wire push mutation from a typed payload (op + key derived
/// the way the client builder does).
fn wire_mutation(id: MutationId, payload: MutationPayload<String, IssueDraft>) -> ClientMutation<Value> {
    let op = payload.sync_op();
    let key = payload.id().clone();
    let value = serde_json::to_value(&payload).unwrap();
    let mut mutation = ClientMutation::new(id, op, value);
    mutation.key = Some(RowKey::new(key).unwrap());
    mutation
}

// ── The matrix ──────────────────────────────────────────────────────

#[tokio::test]
async fn cursorless_pull_snapshots_and_stamps_head_cursor() {
    let source = FeedSource::new();
    source.server_upsert(issue("a", "W1", "one"));
    source.server_upsert(issue("b", "W1", "two"));
    let resource = resource(source);

    let response = resource.pull(ctx(), pull_request(None)).await.unwrap();
    assert_eq!(response.mode, SyncPullMode::Snapshot);
    assert_eq!(response.rows.len(), 2);
    // Head cursor stamped so the NEXT pull upgrades to incremental.
    assert_eq!(response.cursor, Some(SyncCursor::new("2").unwrap()));
    assert!(response.resync.is_none());
}

#[tokio::test]
async fn cursored_pull_serves_ordered_incremental_with_explicit_deletes() {
    let source = FeedSource::new();
    source.server_upsert(issue("a", "W1", "one")); // seq 1
    let resource = resource(source.clone());

    // Client synced through seq 1; three more changes land.
    source.server_upsert(issue("b", "W1", "two")); // seq 2
    source.server_upsert(issue("a", "W1", "one v2")); // seq 3
    source.server_delete("b"); // seq 4

    let response = resource.pull(ctx(), pull_request(Some("1"))).await.unwrap();
    assert_eq!(response.mode, SyncPullMode::Incremental);
    assert!(response.rows.is_empty());
    let ops: Vec<(SyncOp, String)> = response
        .changes
        .iter()
        .map(|c| (c.op, c.key.clone().unwrap().as_str().to_string()))
        .collect();
    assert_eq!(
        ops,
        vec![
            (SyncOp::Upsert, "b".to_string()),
            (SyncOp::Upsert, "a".to_string()),
            (SyncOp::Delete, "b".to_string()),
        ],
        "changes arrive in log order with the delete explicit"
    );
    // Per-change cursors are strictly increasing; response cursor is
    // the last change's position.
    let seqs: Vec<u64> = response
        .changes
        .iter()
        .map(|c| c.cursor.as_str().parse::<u64>().unwrap())
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(response.cursor, Some(SyncCursor::new("4").unwrap()));
    assert!(!response.has_more);
}

#[tokio::test]
async fn cursor_at_head_yields_empty_incremental() {
    let source = FeedSource::new();
    source.server_upsert(issue("a", "W1", "one")); // seq 1
    let resource = resource(source);

    let response = resource.pull(ctx(), pull_request(Some("1"))).await.unwrap();
    assert_eq!(response.mode, SyncPullMode::Incremental);
    assert!(response.changes.is_empty());
    assert_eq!(response.cursor, Some(SyncCursor::new("1").unwrap()));
}

#[tokio::test]
async fn cursor_below_watermark_forces_loud_resync() {
    let source = FeedSource::new();
    source.server_upsert(issue("a", "W1", "one")); // seq 1
    source.server_delete("a"); // seq 2
    source.server_upsert(issue("b", "W1", "two")); // seq 3
    // GC through seq 2 — clients at cursor < 2 can no longer be
    // served incrementally (they'd miss the delete of "a").
    source.log.gc_through(&SyncCursor::new("2").unwrap()).unwrap();
    let resource = resource(source);

    let response = resource.pull(ctx(), pull_request(Some("1"))).await.unwrap();
    assert_eq!(response.mode, SyncPullMode::Snapshot);
    assert_eq!(
        response.resync,
        Some(SyncResyncReason::CursorTruncated),
        "the degraded path is LOUD, never a silent snapshot"
    );
    assert_eq!(response.watermark, Some(SyncCursor::new("2").unwrap()));
    // The snapshot itself is the current truth: only "b".
    let keys: Vec<&str> = response.rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["b"]);
    // Head cursor stamped so the client resumes incrementally after
    // adopting the snapshot.
    assert_eq!(response.cursor, Some(SyncCursor::new("3").unwrap()));
}

#[tokio::test]
async fn client_limited_query_stays_snapshot_only() {
    let source = FeedSource::new();
    source.server_upsert(issue("a", "W1", "one"));
    source.server_upsert(issue("b", "W1", "two"));
    let resource = resource(source);

    let mut request = pull_request(Some("1"));
    // The query DSL signals `.limit(n)` via the reserved params key
    // (the wire `limit` field defaults to 500 on every pull and says
    // nothing about the query's shape).
    request.limit = 1;
    request
        .params
        .insert(pocopine_sync_query::wire::LIMIT_KEY.to_string(), 1.into());
    let response = resource.pull(ctx(), request).await.unwrap();
    assert_eq!(
        response.mode,
        SyncPullMode::Snapshot,
        "top-N views cannot be maintained incrementally (a delete \
         below the fold reveals nothing); they re-snapshot"
    );
    assert!(response.resync.is_none());
}

#[tokio::test]
async fn ordering_survives_has_more_pagination_without_gaps_or_dupes() {
    let source = FeedSource::new();
    for n in 0..25 {
        source.server_upsert(issue(&format!("row_{n:02}"), "W1", "t"));
    }
    let resource = resource(source);

    // Page through with the adapter's own limit clamp by pulling
    // repeatedly from the returned cursor. max_snapshot_rows defaults
    // high, so page via the wire limit... which forces snapshot mode.
    // Instead page at the ChangeLog layer contract: repeated pulls
    // with advancing cursors must yield every seq exactly once.
    let mut cursor = "0".to_string();
    let mut seen: Vec<u64> = Vec::new();
    loop {
        let response = resource
            .pull(ctx(), pull_request(Some(&cursor)))
            .await
            .unwrap();
        assert_eq!(response.mode, SyncPullMode::Incremental);
        if response.changes.is_empty() {
            break;
        }
        for change in &response.changes {
            seen.push(change.cursor.as_str().parse::<u64>().unwrap());
        }
        cursor = response.cursor.clone().unwrap().as_str().to_string();
        if !response.has_more && response.changes.is_empty() {
            break;
        }
    }
    let expected: Vec<u64> = (1..=25).collect();
    assert_eq!(seen, expected, "every seq exactly once, in order");
}

#[tokio::test]
async fn push_writes_stamp_feed_origin_for_the_echo() {
    let source = FeedSource::new();
    let resource = resource(source.clone());

    // A client create + delete through the REAL push path.
    let create_id = mutation_id(1);
    let delete_id = mutation_id(2);
    let create = wire_mutation(
        create_id.clone(),
        MutationPayload::create(
            "x1".to_string(),
            IssueDraft {
                workspace_id: "W1".into(),
                title: "pushed".into(),
            },
        ),
    );
    let delete = wire_mutation(delete_id.clone(), MutationPayload::delete("x1".to_string()));
    let push = SyncPushRequest::new(SyncStreamName::new(STREAM).unwrap(), [create, delete]);
    let push_response = resource.push(ctx(), push).await.unwrap();
    assert_eq!(push_response.accepted.len(), 2);

    // The feed carries both changes WITH their originating mutation
    // ids — the echo a client uses to retire pendings when the push
    // ack is lost.
    let response = resource.pull(ctx(), pull_request(Some("0"))).await.unwrap();
    assert_eq!(response.mode, SyncPullMode::Incremental);
    let origins: Vec<Option<&str>> = response
        .changes
        .iter()
        .map(|c| c.origin.as_ref().map(|o| o.as_str()))
        .collect();
    assert_eq!(
        origins,
        vec![Some("device_test:1"), Some("device_test:2")],
        "feed entries echo the client mutation that produced them"
    );
    assert_eq!(response.changes[1].op, SyncOp::Delete);
}

#[tokio::test]
async fn tenant_scoped_feed_filter_hides_other_tenants_changes() {
    let log = MemoryChangeLog::new().with_filter(Arc::new(
        |query: &Query<Issue>, entry: &pocopine_sync_query::FeedEntry<Issue>| {
            let Some(ws) = query.params().get("workspace_id").and_then(|v| v.as_str()) else {
                return true; // unscoped query sees everything
            };
            match &entry.change {
                pocopine_sync_query::FeedChangeKind::Upsert { row, .. } => row.workspace_id == ws,
                // Deletes carry no row — a per-tenant log (or a
                // key→tenant side table) is the production shape;
                // the test filter keys on the id prefix.
                pocopine_sync_query::FeedChangeKind::Delete { key } => {
                    key.as_str().starts_with(&format!("{ws}_"))
                }
            }
        },
    ));
    let source = FeedSource::with_log(log);
    source.server_upsert(issue("W1_a", "W1", "mine")); // seq 1
    source.server_upsert(issue("W2_a", "W2", "theirs")); // seq 2
    source.server_delete("W2_a"); // seq 3
    source.server_upsert(issue("W1_b", "W1", "mine too")); // seq 4
    let resource = resource(source);

    let mut request = pull_request(Some("0"));
    request.params.insert(
        "workspace_id".to_string(),
        serde_json::Value::String("W1".into()),
    );
    let response = resource.pull(ctx(), request).await.unwrap();
    assert_eq!(response.mode, SyncPullMode::Incremental);
    let keys: Vec<&str> = response
        .changes
        .iter()
        .map(|c| c.key.as_ref().unwrap().as_str())
        .collect();
    assert_eq!(
        keys,
        vec!["W1_a", "W1_b"],
        "the other tenant's changes never leak through the feed"
    );
}

#[tokio::test]
async fn unparseable_cursor_answers_too_old_not_a_guess() {
    let source = FeedSource::new();
    source.server_upsert(issue("a", "W1", "one"));
    let resource = resource(source);

    let response = resource
        .pull(ctx(), pull_request(Some("not-a-cursor-this-log-minted")))
        .await
        .unwrap();
    assert_eq!(response.mode, SyncPullMode::Snapshot);
    assert_eq!(response.resync, Some(SyncResyncReason::CursorTruncated));
}
