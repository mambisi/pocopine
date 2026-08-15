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
    __reset_middleware_chain_for_test, FetchNext, FetchRequest, FetchResponse, install_middleware,
};
use pocopine_core::server::ServerError;
use pocopine_sync::{
    MemoryLocalStore, MutationId, SYNC_OPEN_PATH, SYNC_PULL_PATH, SYNC_PUSH_PATH, StreamParams,
    SyncCollectionName, SyncCursor, SyncLocalStore, SyncOpenResponse, SyncOpenStream,
    SyncPullResponse, SyncResult, SyncRow, SyncStreamName, local_stream_key,
};
use pocopine_sync_query::{
    Mutator, MutatorRemoteContext, MutatorRemoteFuture, QueryClientConfig, RowChange,
    query_client_plugin,
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
        scope: None,
        params: StreamParams::new(),
        watermark: None,
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
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
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
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
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
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
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

// ─── P2 (SPORC hardening): every persisted pending is replayable ────

thread_local! {
    static PUSH_BODIES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// A durable pending wearing the reserved `__crud` envelope (the
/// shape the Mutator-less `push`/`push_typed` overlays persist under)
/// replays as a RAW WIRE mutation on the next boot: the server sees
/// the original self-describing payload, the overlay retires, and the
/// durable entry clears. Pre-P2 this exact seed was the immortal
/// ghost of issue #292 — persisted, unreplayable, re-hydrated
/// forever.
#[tokio::test]
async fn crud_enveloped_pending_replays_raw_and_clears_durably() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            PUSH_BODIES.with(|b| b.borrow_mut().clear());
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![]))),
                    pocopine_sync::SYNC_PUSH_PATH => {
                        PUSH_BODIES.with(|b| b.borrow_mut().push(req.body.clone()));
                        let mut response = pocopine_sync::SyncPushResponse::<Value>::new(
                            SyncStreamName::new(STREAM).unwrap(),
                        );
                        response
                            .accepted
                            .push(MutationId::new("device_seed:9").unwrap());
                        Ok(json_response(&response))
                    }
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            // Seed the compartment the way a mid-flight persist
            // would have: server truth applied (rows: []) plus a
            // stuck __crud pending whose overlay paints a ghost.
            let store = Rc::new(MemoryLocalStore::new());
            let stream = SyncStreamName::new(STREAM).unwrap();
            let params = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W1"));
                p
            };
            let compartment = local_stream_key(&stream, &params);
            let ghost = Issue {
                id: "ghost_1".into(),
                workspace_id: "W1".into(),
                title: "deleted long ago".into(),
            };
            let bare_payload = serde_json::json!({
                "op": "create",
                "id": "ghost_1",
                "draft": { "workspace_id": "W1", "title": "deleted long ago" },
            });
            let envelope = serde_json::json!({
                "__mutator": "__crud",
                "__payload": bare_payload,
            });
            let mutation = pocopine_sync::ClientMutation::new(
                MutationId::new("device_seed:9").unwrap(),
                pocopine_sync::SyncOp::Upsert,
                envelope,
            )
            .key("ghost_1")
            .unwrap();
            let pending = pocopine_sync::LocalPendingMutation::new(mutation).with_optimistic_row(
                Some(
                    SyncRow::new("ghost_1", serde_json::to_value(&ghost).unwrap())
                        .unwrap()
                        .pending(true),
                ),
            );
            store
                .enqueue_pending_mutation(&compartment, pending)
                .await
                .unwrap();

            // Boot. Hydrate paints the ghost; the replay tick must
            // resolve the reserved route, re-push the BARE payload,
            // and retire everything.
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }

            // The wire push carried the SELF-DESCRIBING payload, not
            // the persistence envelope.
            let bodies = PUSH_BODIES.with(|b| b.borrow().clone());
            assert_eq!(bodies.len(), 1, "exactly one raw-wire replay push");
            assert!(
                bodies[0].contains("\"op\":\"create\"") && !bodies[0].contains("__mutator"),
                "replay pushes the bare wire payload: {}",
                bodies[0]
            );

            // Ghost gone from the view, overlay retired, durable
            // entry cleared — nothing left for the sweep.
            assert!(view.rows().is_empty(), "ghost must not render");
            assert_eq!(view.state().pending().len(), 0);
            let durable = store.pending_mutations(&compartment).await.unwrap();
            assert!(durable.is_empty(), "durable pending cleared on accept");
        })
        .await;
}

/// A mutate() pending's `{__mutator: NAME}` envelope survives the
/// post-pull `persist_snapshot` re-enqueue. Pre-P2, that re-enqueue
/// wrote the overlay's BARE wire payload over the enveloped durable
/// entry — so a reload after any settled pull dropped the queued
/// mutation on the floor ("missing mutator name envelope"), silently
/// losing an offline write.
#[tokio::test]
async fn mutate_envelope_survives_mid_flight_persists() {
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
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            client.register_mutator::<FlakyCreate>();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle(2).await;

            let ctx = StubContext::new();
            let payload = Issue {
                id: "issue_offline".into(),
                workspace_id: "W1".into(),
                title: "queued".into(),
            };
            let _ = client
                .mutate::<FlakyCreate>(payload, &ctx)
                .await
                .expect_err("offline mutate returns transport error");
            assert_eq!(view.state().pending().len(), 1);

            // Let SEVERAL poll ticks land while the pending is live —
            // each settled pull runs persist_snapshot, which
            // re-enqueues the overlay's mutation over the durable
            // entry. The envelope must survive every one of them.
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(45)).await;
            }
            let stream = SyncStreamName::new(STREAM).unwrap();
            let params = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W1"));
                p
            };
            let compartment = local_stream_key(&stream, &params);
            let durable = store.pending_mutations(&compartment).await.unwrap();
            assert_eq!(durable.len(), 1, "pending survives while offline");
            assert_eq!(
                durable[0].payload.get("__mutator"),
                Some(&serde_json::json!("flaky_create")),
                "the mutator-name envelope must survive persist_snapshot \
                 re-enqueues — a bare payload here is a lost offline write \
                 on the next reload"
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
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();

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

// ─── Test: external (e.g. WebSocket-delivered) changes route + persist ──

#[tokio::test]
async fn apply_external_changes_routes_into_view_and_persists() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            // Empty /open + /pull: the subscription opens with no rows, so the only
            // row that can appear is the externally-applied one.
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![]))),
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            // Long poll so a periodic /pull snapshot can't wipe the external row
            // before we assert (snapshot pulls are authoritative).
            let config = QueryClientConfig {
                poll_interval: Some(Duration::from_secs(3600)),
                disable_live: true,
                ..QueryClientConfig::default()
            }
            .with_local_store(store_handle);
            let client = query_client_plugin().config(config).into_client();

            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle(3).await; // initial /open + empty /pull
            assert_eq!(view.rows().len(), 0, "no rows before the external change");

            // Inject a WS-style external delta (row already carries its canonical id).
            let stream = SyncStreamName::new(STREAM).unwrap();
            let row = Issue {
                id: "issue_ws".into(),
                workspace_id: "W1".into(),
                title: "delivered over WS".into(),
            };
            client
                .apply_external_changes(&stream, vec![RowChange::Upsert(row.clone())])
                .await;

            // 1. Routed into the live observing view.
            let rows = view.rows();
            assert_eq!(
                rows.len(),
                1,
                "external change routed into the observing view"
            );
            assert_eq!(rows[0].id, "issue_ws");

            // 2. Persisted to the durable store under the query's params-scoped
            //    compartment — so it survives an offline reload (the realtime path
            //    is otherwise in-memory only).
            let params = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W1"));
                p
            };
            let compartment = local_stream_key(&stream, &params);
            let persisted = store.hydrate_stream(&compartment).await.unwrap();
            assert_eq!(
                persisted.rows.len(),
                1,
                "external change persisted to the local store"
            );
            assert_eq!(persisted.rows[0].key.as_str(), "issue_ws");

            // 3. A non-matching predicate departure is a no-op for this view.
            let other = Issue {
                id: "issue_other_ws".into(),
                workspace_id: "W2".into(),
                title: "different workspace".into(),
            };
            client
                .apply_external_changes(&stream, vec![RowChange::Upsert(other)])
                .await;
            assert_eq!(
                view.rows().len(),
                1,
                "a change for a non-matching param doesn't enter the view"
            );
        })
        .await;
}

// ─── Test: an external (WS) change survives an OFFLINE reload ────────

#[tokio::test]
async fn external_change_survives_offline_reload() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let store = Rc::new(MemoryLocalStore::new());
            let stream = SyncStreamName::new(STREAM).unwrap();
            let row = Issue {
                id: "issue_ws".into(),
                workspace_id: "W1".into(),
                title: "delivered over WS".into(),
            };
            let query = || Issue::query().eq(issues::field::workspace_id, "W1").build();
            let config = || QueryClientConfig {
                poll_interval: Some(Duration::from_secs(3600)),
                disable_live: true,
                ..QueryClientConfig::default()
            };

            // Session 1 (online): a WS delta arrives → routed + persisted to the store.
            {
                reset_middleware();
                install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                    match req.url.as_str() {
                        SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                        SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![]))),
                        other => Err(ServerError::Network(format!("unexpected {other}"))),
                    }
                });
                let s: Rc<dyn SyncLocalStore> = store.clone();
                let c1 = query_client_plugin()
                    .config(config().with_local_store(s))
                    .into_client();
                let _v1 = c1.observe(query());
                settle(3).await;
                c1.apply_external_changes(&stream, vec![RowChange::Upsert(row.clone())])
                    .await;
                settle(1).await;
            }

            // Session 2 (reload while OFFLINE): every sync call fails, so the durable
            // cache is the only source — the WS delta must still be visible.
            {
                reset_middleware();
                install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                    Err(ServerError::Network(format!("offline: {}", req.url)))
                });
                let s: Rc<dyn SyncLocalStore> = store.clone();
                let c2 = query_client_plugin()
                    .config(config().with_local_store(s))
                    .into_client();
                let v2 = c2.observe(query());
                settle(4).await; // hydrate from cache; /open + /pull fail (offline)
                let rows = v2.rows();
                assert_eq!(
                    rows.len(),
                    1,
                    "the WS-delivered change survived an offline reload (served from cache)"
                );
                assert_eq!(rows[0].id, "issue_ws");
                assert_eq!(rows[0].title, "delivered over WS");
            }
        })
        .await;
}

// ─── Scope guard: cross-principal clobber prevention ────────────────

thread_local! {
    // (scope, rows) the mock server currently answers with — flipped
    // mid-test to simulate a session expiring to a different
    // principal or a user switch between reloads.
    static SERVER_PRINCIPAL: RefCell<(String, Vec<Issue>)> =
        const { RefCell::new((String::new(), Vec::new())) };
}

fn set_server_principal(scope: &str, rows: Vec<Issue>) {
    SERVER_PRINCIPAL.with(|p| *p.borrow_mut() = (scope.to_string(), rows));
}

fn install_scoped_middleware() {
    reset_middleware();
    install_middleware(|req: FetchRequest, _next: FetchNext| async move {
        let (scope, rows) = SERVER_PRINCIPAL.with(|p| p.borrow().clone());
        // Empty principal = the server answers UNSCOPED (an
        // anonymous session on a stream that only scopes
        // authenticated principals, or a scoping rollback).
        let scope = (!scope.is_empty()).then(|| pocopine_sync::SyncScope::new(scope).unwrap());
        match req.url.as_str() {
            SYNC_OPEN_PATH => {
                let mut response = open_response(1, None);
                response.streams[0].scope = scope;
                Ok(json_response(&response))
            }
            SYNC_PULL_PATH => Ok(json_response(&snapshot_response(rows).with_scope(scope))),
            other => Err(ServerError::Network(format!("unexpected {other}"))),
        }
    });
}

fn w1_issue(id: &str) -> Issue {
    Issue {
        id: id.into(),
        workspace_id: "W1".into(),
        title: id.into(),
    }
}

fn w1_query() -> pocopine_sync_query::Query<Issue> {
    Issue::query().eq(issues::field::workspace_id, "W1").build()
}

fn w1_compartment() -> SyncStreamName {
    let stream = SyncStreamName::new(STREAM).unwrap();
    let mut params = StreamParams::new();
    params.insert("workspace_id".into(), serde_json::json!("W1"));
    local_stream_key(&stream, &params)
}

#[tokio::test]
async fn scope_change_wipes_local_state_and_resyncs() {
    // THE rule: when the responding principal changes (session
    // expired to a guest, user switch), the local cache is someone
    // else's — clear everything and re-sync. The server is the
    // source of truth; committed rows rebuild from it when the
    // original session returns.
    let local = LocalSet::new();
    local
        .run_until(async {
            install_scoped_middleware();
            set_server_principal("user:alice", vec![w1_issue("a1"), w1_issue("a2")]);

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            let view = client.observe(w1_query());
            settle(4).await;
            assert_eq!(view.rows().len(), 2, "alice's snapshot settled");

            // Session expires: the server now answers as guest with
            // an empty view. Alice's local state is cleared and the
            // guest truth settles.
            set_server_principal("guest", Vec::new());
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            assert_eq!(
                view.rows().len(),
                0,
                "scope change wiped the previous principal's view"
            );
            let persisted = store.hydrate_stream(&w1_compartment()).await.unwrap();
            assert!(
                persisted.rows.is_empty(),
                "the durable compartment was cleared and re-persisted as the new truth"
            );
            assert_eq!(
                persisted.scope,
                Some(pocopine_sync::SyncScope::new("guest").unwrap()),
                "the compartment is stamped with the current principal"
            );

            // Alice's session returns: wipe the guest state, re-sync
            // her rows FROM THE SERVER — nothing depends on the local
            // cache surviving.
            set_server_principal(
                "user:alice",
                vec![w1_issue("a1"), w1_issue("a2"), w1_issue("a3")],
            );
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            assert_eq!(view.rows().len(), 3, "alice re-synced from server truth");
            let persisted = store.hydrate_stream(&w1_compartment()).await.unwrap();
            assert_eq!(persisted.rows.len(), 3);
            assert_eq!(
                persisted.scope,
                Some(pocopine_sync::SyncScope::new("user:alice").unwrap())
            );
        })
        .await;
}

#[tokio::test]
async fn queued_mutations_are_discarded_on_principal_change() {
    // Unpushed offline mutations belong to the session that made
    // them. When the principal changes, they are DISCARDED along
    // with the rest of the local state — never pushed under the new
    // principal, and not resurrected when the original returns
    // (clear everything and re-sync; the server is the source of
    // truth for what exists).
    let local = LocalSet::new();
    local
        .run_until(async {
            install_scoped_middleware();
            set_server_principal("user:alice", vec![w1_issue("a1")]);
            FLAKY_MODE.with(|m| *m.borrow_mut() = FlakyMode::Offline);
            FLAKY_HITS.with(|h| h.borrow_mut().clear());

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            client.register_mutator::<FlakyCreate>();
            let view = client.observe(w1_query());
            settle(4).await;

            // Alice queues an offline mutation.
            let ctx = StubContext::new();
            client
                .mutate::<FlakyCreate>(
                    Issue {
                        id: "a_queued".into(),
                        workspace_id: "W1".into(),
                        title: "alice offline".into(),
                    },
                    &ctx,
                )
                .await
                .expect_err("offline mutate returns transport error");
            assert_eq!(view.state().pending().len(), 1);
            let hits_after_mutate = FLAKY_HITS.with(|h| h.borrow().len());

            // Principal changes to guest AND the network returns —
            // the queued mutation must be discarded, not pushed.
            set_server_principal("guest", Vec::new());
            FLAKY_MODE.with(|m| *m.borrow_mut() = FlakyMode::Online);
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            assert_eq!(
                FLAKY_HITS.with(|h| h.borrow().len()),
                hits_after_mutate,
                "alice's queued mutation must never push under the guest session"
            );
            assert_eq!(
                view.state().pending().len(),
                0,
                "the pending overlay was cleared with the rest of alice's state"
            );
            let pending = store.pending_mutations(&w1_compartment()).await.unwrap();
            assert!(pending.is_empty(), "the durable pending was wiped");

            // Alice returns: her view re-syncs from the server; the
            // discarded mutation stays discarded.
            set_server_principal("user:alice", vec![w1_issue("a1")]);
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            assert_eq!(
                FLAKY_HITS.with(|h| h.borrow().len()),
                hits_after_mutate,
                "the discarded mutation is not resurrected"
            );
            assert_eq!(view.rows().len(), 1, "server truth re-synced");
        })
        .await;
}

#[tokio::test]
async fn principal_round_trip_resyncs_from_the_server() {
    // alice -> bob -> alice on the same device and query: each
    // change wipes the compartment and re-syncs the new principal's
    // truth from the server. One compartment, always owned by the
    // CURRENT principal — no sibling caches, no stale state.
    let local = LocalSet::new();
    local
        .run_until(async {
            install_scoped_middleware();
            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();

            set_server_principal("user:alice", vec![w1_issue("a1")]);
            let view = client.observe(w1_query());
            settle(4).await;
            assert_eq!(view.rows().len(), 1);

            set_server_principal("user:bob", vec![w1_issue("b1"), w1_issue("b2")]);
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            let mut ids: Vec<String> = view.rows().into_iter().map(|r| r.id).collect();
            ids.sort();
            assert_eq!(ids, vec!["b1", "b2"], "bob's truth replaced alice's");
            let persisted = store.hydrate_stream(&w1_compartment()).await.unwrap();
            assert_eq!(persisted.rows.len(), 2);
            assert_eq!(
                persisted.scope,
                Some(pocopine_sync::SyncScope::new("user:bob").unwrap())
            );

            set_server_principal("user:alice", vec![w1_issue("a1")]);
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            let ids: Vec<String> = view.rows().into_iter().map(|r| r.id).collect();
            assert_eq!(ids, vec!["a1"], "alice re-synced from the server");
            let persisted = store.hydrate_stream(&w1_compartment()).await.unwrap();
            assert_eq!(persisted.rows.len(), 1);
            assert_eq!(
                persisted.scope,
                Some(pocopine_sync::SyncScope::new("user:alice").unwrap())
            );
        })
        .await;
}

#[tokio::test]
async fn unstamped_compartment_adopts_the_first_advertised_scope() {
    // Migration path: a compartment persisted by a pre-scope server
    // (no stamp) meets a server that started scoping. The client
    // adopts silently — no wipe, no redirect — and the next persist
    // stamps the compartment.
    let local = LocalSet::new();
    local
        .run_until(async {
            // Session 1: an UNSCOPED server persists the compartment.
            reset_middleware();
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => {
                        Ok(json_response(&snapshot_response(vec![w1_issue("legacy")])))
                    }
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });
            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client1 = query_client_plugin()
                .config(test_config_with_store(store_handle.clone()))
                .into_client();
            let view1 = client1.observe(w1_query());
            settle(4).await;
            assert_eq!(view1.rows().len(), 1);
            let persisted = store.hydrate_stream(&w1_compartment()).await.unwrap();
            assert!(
                persisted.scope.is_none(),
                "pre-scope compartment is unstamped"
            );
            drop(view1);
            drop(client1);
            settle(2).await;

            // Session 2: the server now scopes its responses.
            install_scoped_middleware();
            set_server_principal("user:alice", vec![w1_issue("legacy"), w1_issue("fresh")]);
            let client2 = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            let view2 = client2.observe(w1_query());
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            assert_eq!(view2.rows().len(), 2, "adoption settles normally, no wipe");
            let persisted = store.hydrate_stream(&w1_compartment()).await.unwrap();
            assert_eq!(
                persisted.scope,
                Some(pocopine_sync::SyncScope::new("user:alice").unwrap()),
                "the PRIMARY compartment got stamped on the next persist"
            );
            assert_eq!(persisted.rows.len(), 2);
        })
        .await;
}

#[tokio::test]
async fn external_deletes_record_evictions_on_affected_views() {
    // Codex R5: a server-confirmed delete routed OUTSIDE a view's own
    // pull (external change, or another subscription's tombstone)
    // must still surface as a Deleted eviction on the view it removed
    // a row from — otherwise that view's recovery policy can never
    // learn the delete propagated.
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![
                        w1_issue("keep"),
                        w1_issue("doomed"),
                    ]))),
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });
            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            let view = client.observe(w1_query());
            settle(4).await;
            assert_eq!(view.rows().len(), 2);
            let _ = view.take_evictions();

            client
                .apply_external_changes::<Issue>(
                    &SyncStreamName::new(STREAM).unwrap(),
                    vec![RowChange::Delete(
                        pocopine_sync::RowKey::new("doomed").unwrap(),
                    )],
                )
                .await;

            assert_eq!(view.rows().len(), 1, "the external delete propagated");
            let evictions = view.take_evictions();
            assert_eq!(evictions.len(), 1);
            assert_eq!(evictions[0].row.key.as_str(), "doomed");
            assert_eq!(
                evictions[0].reason,
                pocopine_sync_query::EvictionReason::Deleted
            );
        })
        .await;
}

#[tokio::test]
async fn offline_first_mutations_work_on_a_hydrated_stamped_view() {
    // Codex R7: a reload that starts OFFLINE hydrates alice's stamped
    // compartment but never observes a session scope. Her mutation
    // must still render on her own view and persist into her own
    // compartment — never-observed is permissive, only a POSITIVELY
    // different session scope gates mutation routing.
    let local = LocalSet::new();
    local
        .run_until(async {
            // Session 1 (online): alice settles and stamps the
            // compartment.
            install_scoped_middleware();
            set_server_principal("user:alice", vec![w1_issue("a1")]);
            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client1 = query_client_plugin()
                .config(test_config_with_store(store_handle.clone()))
                .into_client();
            let view1 = client1.observe(w1_query());
            settle(4).await;
            assert_eq!(view1.rows().len(), 1);
            drop(view1);
            drop(client1);
            settle(2).await;

            // Session 2: fully offline — every request fails.
            reset_middleware();
            install_middleware(|_req: FetchRequest, _next: FetchNext| async move {
                Err(ServerError::Network("offline".into()))
            });
            FLAKY_MODE.with(|m| *m.borrow_mut() = FlakyMode::Offline);
            let client2 = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            client2.register_mutator::<FlakyCreate>();
            let view2 = client2.observe(w1_query());
            settle(4).await;
            assert_eq!(view2.rows().len(), 1, "hydrate painted alice's cache");

            let ctx = StubContext::new();
            client2
                .mutate::<FlakyCreate>(
                    Issue {
                        id: "a_offline_first".into(),
                        workspace_id: "W1".into(),
                        title: "offline edit".into(),
                    },
                    &ctx,
                )
                .await
                .expect_err("offline mutate returns transport error");

            assert!(
                view2.rows().iter().any(|r| r.id == "a_offline_first"),
                "the offline edit renders on the hydrated stamped view"
            );
            let pending = store.pending_mutations(&w1_compartment()).await.unwrap();
            assert_eq!(
                pending.len(),
                1,
                "the offline edit persists into the view's own compartment, not the bare fallback"
            );
        })
        .await;
}

#[tokio::test]
async fn scope_drift_clears_sibling_subscriptions_on_the_same_stream() {
    // Two live queries on one stream: the drift detected by either
    // driver must clear BOTH — the refetched response fans out
    // stream-wide, and a sibling still holding the previous
    // principal's rows would otherwise mix the two until its own
    // next tick.
    let local = LocalSet::new();
    local
        .run_until(async {
            install_scoped_middleware();
            set_server_principal("user:alice", vec![w1_issue("a1"), w1_issue("a2")]);

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            let view_a = client.observe(w1_query());
            let view_b = client.observe(
                Issue::query()
                    .eq(issues::field::workspace_id, "W1")
                    .limit(10)
                    .build(),
            );
            settle(4).await;
            assert_eq!(view_a.rows().len(), 2);
            assert_eq!(view_b.rows().len(), 2);

            // Principal changes; whichever driver ticks first fences
            // BOTH subscriptions before its refetch settles.
            set_server_principal("user:bob", vec![w1_issue("b1")]);
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            let ids_a: Vec<String> = view_a.rows().into_iter().map(|r| r.id).collect();
            let ids_b: Vec<String> = view_b.rows().into_iter().map(|r| r.id).collect();
            assert_eq!(ids_a, vec!["b1"], "no alice leftovers in view A");
            assert_eq!(ids_b, vec!["b1"], "no alice leftovers in view B");
        })
        .await;
}

#[tokio::test]
async fn scope_drift_wipes_the_bare_stream_pending_fallback() {
    // A mutation whose predicate matched NO active subscription
    // parks its durable pending in the bare-stream compartment;
    // hydrate merges that queue on every driver spawn. A principal
    // change must wipe it too — otherwise the old principal's
    // queued mutation replays under whoever reloads next.
    let local = LocalSet::new();
    local
        .run_until(async {
            install_scoped_middleware();
            set_server_principal("user:alice", vec![w1_issue("a1")]);
            FLAKY_MODE.with(|m| *m.borrow_mut() = FlakyMode::Offline);

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle))
                .into_client();
            client.register_mutator::<FlakyCreate>();
            let _view = client.observe(w1_query());
            settle(4).await;

            // A W2 row matches no active subscription — the pending
            // falls back to the bare-stream compartment.
            let ctx = StubContext::new();
            client
                .mutate::<FlakyCreate>(
                    Issue {
                        id: "w2_orphan".into(),
                        workspace_id: "W2".into(),
                        title: "no matching view".into(),
                    },
                    &ctx,
                )
                .await
                .expect_err("offline mutate returns transport error");
            let bare = SyncStreamName::new(STREAM).unwrap();
            assert_eq!(
                store.pending_mutations(&bare).await.unwrap().len(),
                1,
                "the orphan pending parked in the bare compartment"
            );

            // Principal changes: the drift wipe covers the bare
            // fallback too.
            set_server_principal("user:bob", vec![w1_issue("b1")]);
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            assert!(
                store.pending_mutations(&bare).await.unwrap().is_empty(),
                "alice's orphan pending must not survive the principal change"
            );
        })
        .await;
}

// ─── Test: un-replayable durable pendings die cleanly ──────────────
//
// Regression for the immortal-ghost-row bug. A pending persisted
// WITHOUT the mutator-name envelope (the shape `push_typed` overlays
// get when a mid-flight persist captures them) can never replay. It
// used to stick forever: the drop path dequeued under the wrong Row
// type (silent no-op), so the optimistic row painted every session,
// and persist_snapshot only ever ADDED durable pendings, so the entry
// re-hydrated on every boot. Now the hydrated drop rolls the overlay
// back type-erased, and the post-pull persist sweeps the durable
// entry the moment memory no longer holds it.
#[tokio::test]
async fn unreplayable_hydrated_pending_clears_overlay_and_durable_entry() {
    use pocopine_sync::{ClientMutation, LocalPendingMutation, RowKey};

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

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();

            // Seed the compartment with a durable pending whose payload
            // has NO {__mutator, __payload} envelope — exactly what a
            // `push_typed` overlay looks like when a concurrent pull's
            // persist captured it mid-flight.
            let stream = SyncStreamName::new(STREAM).unwrap();
            let params = {
                let mut p = StreamParams::new();
                p.insert("workspace_id".into(), serde_json::json!("W1"));
                p
            };
            let compartment = local_stream_key(&stream, &params);
            let ghost = Issue {
                id: "issue_ghost".into(),
                workspace_id: "W1".into(),
                title: "immortal?".into(),
            };
            let mutation_id = MutationId::uuid();
            let mut wire = ClientMutation::new(
                mutation_id.clone(),
                pocopine_sync::SyncOp::Upsert,
                serde_json::to_value(&ghost).unwrap(),
            );
            wire.key = RowKey::new("issue_ghost").ok();
            store
                .enqueue_pending_mutation(
                    &compartment,
                    LocalPendingMutation::new(wire).with_optimistic_row(Some(SyncRow {
                        key: RowKey::new("issue_ghost").unwrap(),
                        version: None,
                        value: serde_json::to_value(&ghost).unwrap(),
                        pending: true,
                        conflict: false,
                    })),
                )
                .await
                .unwrap();

            // Boot a session over that store. No mutator registered —
            // the hydrated replay can only drop the entry.
            let client = query_client_plugin()
                .config(test_config_with_store(store_handle.clone()))
                .into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            // Enough ticks for hydrate → open → pull → replay-drop →
            // next poll's persist sweep (poll every 50ms).
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }

            assert_eq!(
                view.state().pending().len(),
                0,
                "dropped hydrated replay must roll back its overlay (no ghost row)"
            );
            assert!(
                view.rows().is_empty(),
                "the ghost's optimistic row must not render"
            );
            assert!(
                store
                    .pending_mutations(&compartment)
                    .await
                    .unwrap()
                    .is_empty(),
                "the persist sweep must remove the durable entry so it cannot re-hydrate"
            );
        })
        .await;
}

// ─── The accepted-push echo becomes canonical ───────────────────────

/// An ACCEPTED `/push` response echoes the authoritative row — the server
/// re-stamps timestamps (and any server-minted fields) on create, so the
/// echo's version-bearing fields differ from the optimistic draft. The
/// client must dequeue the optimistic overlay and adopt the echo as
/// canonical immediately. Before this fix the overlay — which wins the
/// `rows()` merge — shadowed canonical for the rest of the session
/// (`/pull` routing never dequeues pendings), so every follow-up
/// versioned update sent the stale client-stamped version and conflicted
/// as "base version is stale" until a reload.
#[tokio::test]
async fn accepted_push_adopts_the_echoed_authoritative_row() {
    thread_local! {
        static PUSHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    fn echoed_issue() -> Issue {
        Issue {
            id: "issue_1".into(),
            workspace_id: "W1".into(),
            title: "server-stamped".into(),
        }
    }

    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    // Stateful on purpose: pre-push the server has nothing;
                    // post-push it serves the created row — so the 50ms poll
                    // can never wipe the echo out from under the assertions.
                    SYNC_PULL_PATH => {
                        let rows = if PUSHED.with(|p| p.get()) {
                            vec![echoed_issue()]
                        } else {
                            Vec::new()
                        };
                        Ok(json_response(&snapshot_response(rows)))
                    }
                    SYNC_PUSH_PATH => {
                        PUSHED.with(|p| p.set(true));
                        let mut response: pocopine_sync::SyncPushResponse<Value> =
                            pocopine_sync::SyncPushResponse::new(
                                SyncStreamName::new(STREAM).unwrap(),
                            );
                        response.accepted.push(MutationId::new("m_create").unwrap());
                        let echoed = echoed_issue();
                        response.rows.push(
                            SyncRow::new(echoed.id.clone(), serde_json::to_value(&echoed).unwrap())
                                .unwrap(),
                        );
                        Ok(json_response(&response))
                    }
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            let client = query_client_plugin().into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle(2).await;

            let draft = Issue {
                id: "issue_1".into(),
                workspace_id: "W1".into(),
                title: "client-draft".into(),
            };
            client
                .push(
                    SyncStreamName::new(STREAM).unwrap(),
                    MutationId::new("m_create").unwrap(),
                    draft.clone(),
                    RowChange::Upsert(draft),
                    SYNC_PUSH_PATH,
                )
                .await
                .expect("push accepted");

            let rows = view.rows();
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].title, "server-stamped",
                "the view must serve the ECHOED authoritative row, not the optimistic draft"
            );
            assert_eq!(
                view.state().pending().len(),
                0,
                "the accepted overlay must be dequeued, not left shadowing canonical"
            );
        })
        .await;
}
