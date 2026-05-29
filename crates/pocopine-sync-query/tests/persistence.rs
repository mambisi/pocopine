//! End-to-end persistence tests for `pocopine-sync-query` (RFC 088 §A).
//!
//! These exercise the durable-store integration end-to-end: hydrate
//! Phase 0, post-pull save_snapshot, schema-drift wipe, and pending
//! mutation replay on hydrate.
//!
//! Each test installs a `pocopine::fetch` middleware that mocks
//! `/open` + `/pull` + `/push`, then runs `client.observe(q)` inside a
//! `tokio::task::LocalSet` so the driver task can spawn. The shared
//! `MemoryLocalStore` between client instances simulates the same
//! browser tab being reloaded — second client picks up the first
//! client's persisted state.

#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use pocopine_core::fetch::{
    install_middleware, FetchNext, FetchRequest, FetchResponse, __reset_middleware_chain_for_test,
};
use pocopine_core::server::ServerError;
use pocopine_sync::{
    local_stream_key, MemoryLocalStore, MutationId, StreamParams, SyncCollectionName, SyncCursor,
    SyncLocalStore, SyncOpenResponse, SyncOpenStream, SyncPullResponse, SyncResult, SyncRow,
    SyncStreamName, SYNC_OPEN_PATH, SYNC_PULL_PATH,
};
use pocopine_sync_query::{
    query_client_plugin, Mutator, MutatorRemoteContext, MutatorRemoteFuture, QueryClient,
    QueryClientConfig, RowChange,
};
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::LocalSet;

fn reset_middleware() {
    __reset_middleware_chain_for_test();
}

const STREAM: &str = "issues";
const COLLECTION: &str = "issues";

#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    #[query_param(required)]
    pub workspace_id: String,
    pub title: String,
}

/// Mutator that always succeeds remotely (returns the same row as
/// canonical). Used by the pending-replay test.
struct CreateIssue;

impl Mutator for CreateIssue {
    type Payload = Issue;
    type Row = Issue;
    const NAME: &'static str = "create_issue";
    const STREAM: &'static str = "issues";
    const SCHEMA_VERSION: u32 = 1;

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>> {
        vec![RowChange::Upsert(payload.clone())]
    }

    fn apply_remote(
        _ctx: &dyn MutatorRemoteContext,
        payload: Self::Payload,
    ) -> MutatorRemoteFuture<Self::Row> {
        Box::pin(async move { Ok(vec![RowChange::Upsert(payload)]) })
    }
}

thread_local! {
    static FLAKY_MODE: RefCell<FlakyMode> = const { RefCell::new(FlakyMode::Offline) };
    static FLAKY_HITS: RefCell<Vec<MutationId>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug)]
enum FlakyMode {
    Offline,
    Online,
}

/// Mutator whose apply_remote can be flipped between Offline and
/// Online so a test can simulate "mutation queued offline, page
/// reloaded, then network came back".
struct FlakyCreate;

impl Mutator for FlakyCreate {
    type Payload = Issue;
    type Row = Issue;
    const NAME: &'static str = "flaky_create";
    const STREAM: &'static str = "issues";
    const SCHEMA_VERSION: u32 = 1;

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>> {
        vec![RowChange::Upsert(payload.clone())]
    }

    fn apply_remote(
        ctx: &dyn MutatorRemoteContext,
        payload: Self::Payload,
    ) -> MutatorRemoteFuture<Self::Row> {
        let id = ctx.next_mutation_id();
        Box::pin(async move {
            let id = id?;
            FLAKY_HITS.with(|h| h.borrow_mut().push(id));
            match FLAKY_MODE.with(|m| *m.borrow()) {
                FlakyMode::Offline => Err(pocopine_sync::SyncError::network("simulated offline")),
                FlakyMode::Online => Ok(vec![RowChange::Upsert(payload)]),
            }
        })
    }
}

struct StubContext {
    next_id: std::cell::Cell<u64>,
}

impl StubContext {
    fn new() -> Self {
        Self {
            next_id: std::cell::Cell::new(1),
        }
    }
}

impl MutatorRemoteContext for StubContext {
    fn push_url(&self) -> &str {
        "/__pocopine/sync/v1/push"
    }

    fn next_mutation_id(&self) -> SyncResult<MutationId> {
        let n = self.next_id.get();
        self.next_id.set(n + 1);
        MutationId::new(format!("test:{n}"))
    }
}

fn json_response<T: serde::Serialize>(body: &T) -> FetchResponse {
    FetchResponse {
        status: 200,
        body: format!(
            "{{\"Ok\":{}}}",
            serde_json::to_string(body).expect("test body serializable")
        ),
    }
}

fn open_response(schema_version: u32, cursor: Option<SyncCursor>) -> SyncOpenResponse {
    SyncOpenResponse::new(vec![SyncOpenStream {
        stream: SyncStreamName::new(STREAM).unwrap(),
        collection: SyncCollectionName::new(COLLECTION).unwrap(),
        cursor,
        schema_version,
        params: StreamParams::new(),
    }])
}

fn snapshot_response(rows: Vec<Issue>) -> SyncPullResponse<Value> {
    let wire_rows: Vec<SyncRow<Value>> = rows
        .into_iter()
        .map(|r| SyncRow::new(r.id.clone(), serde_json::to_value(&r).unwrap()).unwrap())
        .collect();
    SyncPullResponse::snapshot(
        SyncStreamName::new(STREAM).unwrap(),
        SyncCollectionName::new(COLLECTION).unwrap(),
        wire_rows,
        Some(SyncCursor::new("c_1").unwrap()),
    )
}

async fn settle(ticks: usize) {
    for _ in 0..ticks {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn test_config_with_store(store: Rc<dyn SyncLocalStore>) -> QueryClientConfig {
    QueryClientConfig {
        poll_interval: Some(Duration::from_millis(50)),
        disable_live: true,
        ..QueryClientConfig::default()
    }
    .with_local_store(store)
}

// ─── Test 1: hydrate-then-observe round-trip ────────────────────────

#[tokio::test]
async fn hydrate_populates_canonical_rows_then_pull_refreshes() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![Issue {
                        id: "issue_fresh".into(),
                        workspace_id: "W1".into(),
                        title: "from /pull".into(),
                    }]))),
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            // Seed the store with a row that pre-dates this session.
            let store = Rc::new(MemoryLocalStore::new());
            let params = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W1"));
                p
            };
            let stream = SyncStreamName::new(STREAM).unwrap();
            let compartment = local_stream_key(&stream, &params);
            let collection = SyncCollectionName::new(compartment.as_str()).unwrap();
            let cached = Issue {
                id: "issue_cached".into(),
                workspace_id: "W1".into(),
                title: "from cache".into(),
            };
            let row =
                SyncRow::new(cached.id.clone(), serde_json::to_value(&cached).unwrap()).unwrap();
            store
                .save_snapshot(
                    pocopine_sync::LocalSnapshotBatch::new(
                        compartment.clone(),
                        collection,
                        vec![row],
                        Some(SyncCursor::new("seed").unwrap()),
                    )
                    .with_application_schema_version(Some(1)),
                )
                .await
                .unwrap();

            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin().config(test_config_with_store(store_handle)).into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());

            // After settle, both cached + /pull rows should be visible.
            settle(3).await;
            let rows = view.rows();
            // /pull is a Snapshot which wipes this subscription's
            // canonical set then re-upserts /pull's rows. The
            // hydrated rows survive only until Snapshot wipe — that
            // is intentional: snapshot mode is authoritative.
            assert_eq!(
                rows.len(),
                1,
                "post-/pull rows reflect server snapshot, not hydrated cache"
            );
            assert_eq!(rows[0].id, "issue_fresh");

            // Confirm the store now reflects the /pull state.
            let persisted = store.hydrate_stream(&compartment).await.unwrap();
            assert_eq!(persisted.rows.len(), 1);
            assert_eq!(persisted.rows[0].key.as_str(), "issue_fresh");
        })
        .await;
}

#[tokio::test]
async fn hydrate_shows_cached_rows_before_pull_lands() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            // Slow /pull so the hydrated rows are visible for a tick
            // before the server response wipes them via Snapshot.
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        Ok(json_response(&open_response(1, None)))
                    }
                    SYNC_PULL_PATH => {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        Ok(json_response(&snapshot_response(vec![])))
                    }
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            let store = Rc::new(MemoryLocalStore::new());
            let params = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W1"));
                p
            };
            let stream = SyncStreamName::new(STREAM).unwrap();
            let compartment = local_stream_key(&stream, &params);
            let collection = SyncCollectionName::new(compartment.as_str()).unwrap();
            let cached = Issue {
                id: "issue_cached".into(),
                workspace_id: "W1".into(),
                title: "from cache".into(),
            };
            let row =
                SyncRow::new(cached.id.clone(), serde_json::to_value(&cached).unwrap()).unwrap();
            store
                .save_snapshot(
                    pocopine_sync::LocalSnapshotBatch::new(
                        compartment.clone(),
                        collection,
                        vec![row],
                        Some(SyncCursor::new("seed").unwrap()),
                    )
                    .with_application_schema_version(Some(1)),
                )
                .await
                .unwrap();

            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin().config(test_config_with_store(store_handle)).into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());

            // After 1-2 ticks the hydrate phase has populated the
            // state but the /open/pull responses haven't landed yet
            // (they sleep 30ms each).
            settle(2).await;
            let rows = view.rows();
            assert_eq!(rows.len(), 1, "hydrated rows should be visible pre-pull");
            assert_eq!(rows[0].id, "issue_cached");
        })
        .await;
}

// ─── Test 2: schema-drift wipe + rebuild ───────────────────────────

#[tokio::test]
async fn schema_drift_wipes_persisted_state_and_repopulates() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    // Server is now on v2 — drift relative to the
                    // store's seeded v1.
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(2, None))),
                    SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![Issue {
                        id: "issue_v2".into(),
                        workspace_id: "W1".into(),
                        title: "v2 row".into(),
                    }]))),
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            // Seed v1 state.
            let store = Rc::new(MemoryLocalStore::new());
            let stream = SyncStreamName::new(STREAM).unwrap();
            let params = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W1"));
                p
            };
            let compartment = local_stream_key(&stream, &params);
            let collection = SyncCollectionName::new(compartment.as_str()).unwrap();
            let stale = Issue {
                id: "issue_v1".into(),
                workspace_id: "W1".into(),
                title: "stale".into(),
            };
            let row =
                SyncRow::new(stale.id.clone(), serde_json::to_value(&stale).unwrap()).unwrap();
            store
                .save_snapshot(
                    pocopine_sync::LocalSnapshotBatch::new(
                        compartment.clone(),
                        collection,
                        vec![row],
                        Some(SyncCursor::new("seed_v1").unwrap()),
                    )
                    .with_application_schema_version(Some(1)),
                )
                .await
                .unwrap();
            // Pre-check: the seeded snapshot is present.
            let pre = store.hydrate_stream(&compartment).await.unwrap();
            assert_eq!(pre.rows.len(), 1);
            assert_eq!(pre.application_schema_version, Some(1));

            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin().config(test_config_with_store(store_handle)).into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle(5).await;

            let rows = view.rows();
            assert_eq!(rows.len(), 1, "v2 row replaced v1 snapshot");
            assert_eq!(rows[0].id, "issue_v2");
            assert_eq!(
                view.state().application_schema_version,
                Some(2),
                "in-memory schema_version advanced after drift"
            );

            // Persisted state reflects v2 now too.
            let post = store.hydrate_stream(&compartment).await.unwrap();
            assert_eq!(post.rows.len(), 1);
            assert_eq!(post.rows[0].key.as_str(), "issue_v2");
            assert_eq!(post.application_schema_version, Some(2));
        })
        .await;
}

// ─── Test 3: pending-mutation persistence + replay on hydrate ──────

#[tokio::test]
async fn pending_mutation_persists_and_replays_on_hydrate() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![]))),
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            FLAKY_MODE.with(|m| *m.borrow_mut() = FlakyMode::Offline);
            FLAKY_HITS.with(|h| h.borrow_mut().clear());

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();

            // ── Session 1 ────────────────────────────────────────
            let client1 = query_client_plugin().config(test_config_with_store(store_handle.clone())).into_client();
            client1.register_mutator::<FlakyCreate>();
            let view1 = client1.observe(
                Issue::query()
                    .eq(issues::field::workspace_id, "W1")
                    .build(),
            );
            settle(2).await;

            // Mutate offline. Pending overlay persists to the store.
            let ctx = StubContext::new();
            let payload = Issue {
                id: "issue_offline".into(),
                workspace_id: "W1".into(),
                title: "queued".into(),
            };
            let err = client1
                .mutate::<FlakyCreate>(payload.clone(), &ctx)
                .await
                .expect_err("offline mutate returns transport error");
            assert!(err.is_transport(), "got: {err}");
            assert_eq!(view1.state().pending().len(), 1);

            // Confirm persisted: bare-stream OR compartment contains
            // the pending mutation.
            let stream = SyncStreamName::new(STREAM).unwrap();
            let params = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W1"));
                p
            };
            let compartment = local_stream_key(&stream, &params);
            let persisted_pending = store.pending_mutations(&compartment).await.unwrap();
            assert_eq!(
                persisted_pending.len(),
                1,
                "pending mutation should persist in the subscription compartment"
            );
            // Confirm the persisted payload carries the mutator NAME
            // envelope (not the bare payload).
            let envelope = &persisted_pending[0].payload;
            assert_eq!(envelope.get("__mutator"), Some(&serde_json::json!("flaky_create")));

            // Tear down session 1.
            drop(view1);
            drop(client1);
            settle(2).await;

            // ── Session 2: simulated reload ──────────────────────
            // Flip the network to online before the new session
            // hydrates so the replay tick succeeds.
            FLAKY_MODE.with(|m| *m.borrow_mut() = FlakyMode::Online);
            let pre_hits = FLAKY_HITS.with(|h| h.borrow().len());

            let client2 = query_client_plugin().config(test_config_with_store(store_handle.clone())).into_client();
            client2.register_mutator::<FlakyCreate>();
            let view2 = client2.observe(
                Issue::query()
                    .eq(issues::field::workspace_id, "W1")
                    .build(),
            );
            // Give the driver multiple ticks: hydrate -> /open -> /pull
            // -> replay_pending. Each tick is ~50ms.
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            let post_hits = FLAKY_HITS.with(|h| h.borrow().len());
            assert!(
                post_hits > pre_hits,
                "replay should have re-fired apply_remote (hits before={pre_hits}, after={post_hits})"
            );

            // After successful replay the pending overlay clears.
            assert_eq!(
                view2.state().pending().len(),
                0,
                "successful hydrated replay clears the pending overlay"
            );
        })
        .await;
}

// ─── Test 4: multi-compartment isolation ──────────────────────────

#[tokio::test]
async fn distinct_params_persist_to_distinct_compartments() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            // Per-request middleware: return different /pull
            // snapshots for W_A vs W_B so the driver's
            // persist-after-pull writes distinct row sets per
            // compartment. We inspect the request body to pick.
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => {
                        let body = &req.body;
                        let is_a = body.contains("\"W_A\"");
                        let is_b = body.contains("\"W_B\"");
                        let rows = if is_a {
                            vec![Issue {
                                id: "row_only_in_A".into(),
                                workspace_id: "W_A".into(),
                                title: "A".into(),
                            }]
                        } else if is_b {
                            vec![Issue {
                                id: "row_only_in_B".into(),
                                workspace_id: "W_B".into(),
                                title: "B".into(),
                            }]
                        } else {
                            vec![]
                        };
                        Ok(json_response(&snapshot_response(rows)))
                    }
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin().config(test_config_with_store(store_handle)).into_client();

            let _view_a = client.observe(
                Issue::query()
                    .eq(issues::field::workspace_id, "W_A")
                    .build(),
            );
            let _view_b = client.observe(
                Issue::query()
                    .eq(issues::field::workspace_id, "W_B")
                    .build(),
            );
            settle(5).await;

            let stream = SyncStreamName::new(STREAM).unwrap();
            let params_a = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W_A"));
                p
            };
            let params_b = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W_B"));
                p
            };
            let compartment_a = local_stream_key(&stream, &params_a);
            let compartment_b = local_stream_key(&stream, &params_b);
            assert_ne!(
                compartment_a, compartment_b,
                "distinct params produce distinct compartments"
            );

            // Driver-persisted state (NOT manually seeded). The
            // driver should have observed /pull and persisted A's
            // row only in compartment A, B's row only in
            // compartment B.
            let hydrated_a = store.hydrate_stream(&compartment_a).await.unwrap();
            let hydrated_b = store.hydrate_stream(&compartment_b).await.unwrap();
            let a_keys: Vec<&str> = hydrated_a.rows.iter().map(|r| r.key.as_str()).collect();
            let b_keys: Vec<&str> = hydrated_b.rows.iter().map(|r| r.key.as_str()).collect();
            // Drive both directions: A's compartment must NOT
            // contain B's marker AND B's compartment must NOT
            // contain A's marker.
            assert!(
                a_keys.contains(&"row_only_in_A"),
                "compartment A holds its own driver-persisted row; got {:?}",
                a_keys
            );
            assert!(
                !a_keys.contains(&"row_only_in_B"),
                "compartment A must NOT see B's row; got {:?}",
                a_keys
            );
            assert!(
                b_keys.contains(&"row_only_in_B"),
                "compartment B holds its own driver-persisted row; got {:?}",
                b_keys
            );
            assert!(
                !b_keys.contains(&"row_only_in_A"),
                "compartment B must NOT see A's row; got {:?}",
                b_keys
            );
        })
        .await;
}

// ─── Backwards-compat: no local_store means no persistence calls ──

#[tokio::test]
async fn no_local_store_keeps_in_memory_only_behavior() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![Issue {
                        id: "ephemeral".into(),
                        workspace_id: "W1".into(),
                        title: "in-memory".into(),
                    }]))),
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            // No `with_local_store` — backwards-compat path.
            let client = query_client_plugin().into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle(3).await;
            assert_eq!(view.rows().len(), 1);
            // The mutate path with no local_store still works.
            let ctx = StubContext::new();
            let payload = Issue {
                id: "in_mem_create".into(),
                workspace_id: "W1".into(),
                title: "mutate".into(),
            };
            let _ = client.mutate::<CreateIssue>(payload, &ctx).await;
        })
        .await;
}
