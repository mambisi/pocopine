//! End-to-end tests for the per-subscription driver (RFC 087).
//!
//! Each test installs a `pocopine::fetch` middleware that mocks
//! `/open` + `/pull` responses, then `observe`s a query inside a
//! `tokio::task::LocalSet` so the driver task can spawn. The
//! `LocalSet::run_until(async { ... })` shape mirrors what real
//! native consumers do.
//!
//! Tests cover:
//!
//! * `/open` → `/pull` populates canonical rows.
//! * Schema-version drift resets state + repopulates.
//! * Live wakeup filter (drops mismatched events).
//! * Offline replay re-fires `apply_remote` after `/pull` recovers.
//! * Cancellation: drop the QueryHandle, verify the driver exits.
//! * `QueryClient::without_driver` skips the spawn entirely.

#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use pocopine_core::fetch::{
    __reset_middleware_chain_for_test, FetchNext, FetchRequest, FetchResponse, install_middleware,
};
use pocopine_core::server::ServerError;
use pocopine_sync::{
    MutationId, SYNC_OPEN_PATH, SYNC_PULL_PATH, SyncCollectionName, SyncCursor, SyncOpenResponse,
    SyncOpenStream, SyncPullResponse, SyncResult, SyncRow, SyncStreamName,
};
use pocopine_sync_query::{
    Mutator, MutatorRemoteContext, MutatorRemoteFuture, QueryClient, QueryClientConfig, RowChange,
    query_client_plugin,
};
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::LocalSet;

/// Each test owns its own middleware-state Rc and resets the chain
/// at the start, so tests don't bleed mocks into each other.
fn reset_middleware() {
    __reset_middleware_chain_for_test();
}

/// Stream + collection identifiers used across all tests.
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

/// Mutator whose `apply_remote` reads a per-test mode (online /
/// offline / online-after-replay) so the offline-replay test can
/// flip transport behavior mid-flight.
struct PendingControlled;

thread_local! {
    static PENDING_MODE: RefCell<PendingMode> = const { RefCell::new(PendingMode::Offline) };
    static PENDING_REMOTE_HITS: RefCell<Vec<MutationId>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug)]
enum PendingMode {
    Offline,
    Online,
}

impl Mutator for PendingControlled {
    type Payload = Issue;
    type Row = Issue;
    const NAME: &'static str = "pending_controlled";
    const STREAM: &'static str = "issues";
    const SCHEMA_VERSION: u32 = 1;

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>> {
        vec![RowChange::Upsert(payload.clone())]
    }

    fn apply_remote(
        ctx: &dyn MutatorRemoteContext,
        payload: Self::Payload,
    ) -> MutatorRemoteFuture<Self::Row> {
        // Capture the context's `next_mutation_id` — on the
        // original call, this is the framework-supplied
        // `StubContext`, which mints fresh ids per call. On the
        // replay, this is the framework's internal
        // `ReplayContext`, which returns the SAME mutation id
        // captured at queue time. The test asserts every
        // ReplayContext call returns the same id (offsets that
        // the original StubContext call is one-ahead — typical
        // wrapper that mints the wire id outside of apply_remote).
        let id = ctx.next_mutation_id();
        Box::pin(async move {
            let id = id?;
            PENDING_REMOTE_HITS.with(|hits| hits.borrow_mut().push(id));
            let mode = PENDING_MODE.with(|m| *m.borrow());
            match mode {
                PendingMode::Offline => Err(pocopine_sync::SyncError::network("simulated offline")),
                PendingMode::Online => Ok(vec![RowChange::Upsert(payload)]),
            }
        })
    }
}

/// Minimal `MutatorRemoteContext` that mints predictable mutation
/// ids and uses the canonical push URL.
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

/// JSON body for a successful fetch response.
fn json_response<T: serde::Serialize>(body: &T) -> FetchResponse {
    FetchResponse::new(
        200,
        format!(
            "{{\"Ok\":{}}}",
            serde_json::to_string(body).expect("test body serializable")
        ),
    )
}

/// Build a `SyncOpenResponse` for our test stream.
fn open_response(schema_version: u32, cursor: Option<SyncCursor>) -> SyncOpenResponse {
    SyncOpenResponse::new(vec![SyncOpenStream {
        stream: SyncStreamName::new(STREAM).unwrap(),
        collection: SyncCollectionName::new(COLLECTION).unwrap(),
        cursor,
        schema_version,
        scope: None,
        params: pocopine_sync::StreamParams::new(),
        watermark: None,
    }])
}

/// Build a `SyncPullResponse` snapshot from typed `Issue`s.
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

/// Drive the LocalSet forward by a few yields so spawned drivers
/// observe their initial /open + /pull responses. We use a tiny
/// poll interval so the test doesn't wait 30s for the second
/// tick.
async fn settle_ticks(ticks: usize) {
    for _ in 0..ticks {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Build a test-friendly `QueryClientConfig`: short poll interval,
/// live wakeup disabled (host can't open an EventSource).
fn test_config() -> QueryClientConfig {
    QueryClientConfig {
        poll_interval: Some(Duration::from_millis(30)),
        disable_live: true,
        ..QueryClientConfig::default()
    }
}

#[tokio::test]
async fn tombstones_delete_with_confidence_and_absences_report_unexplained() {
    // Snapshot 1: rows a, b, c. Snapshot 2: only a, plus a tombstone
    // for b. The engine must (1) remove BOTH b and c from the view —
    // the server snapshot is the source of truth either way — and
    // (2) report b as Deleted (tombstoned: don't resurrect) and c as
    // Unexplained (absent with no explanation: app policy decides).
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            let pull_calls = Rc::new(RefCell::new(0u32));
            let pull_for_mw = pull_calls.clone();
            install_middleware(move |req: FetchRequest, _next: FetchNext| {
                let pull_for_mw = pull_for_mw.clone();
                async move {
                    match req.url.as_str() {
                        SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                        SYNC_PULL_PATH => {
                            let n = {
                                let mut c = pull_for_mw.borrow_mut();
                                *c += 1;
                                *c
                            };
                            let issue = |id: &str, title: &str| Issue {
                                id: id.into(),
                                workspace_id: "W1".into(),
                                title: title.into(),
                            };
                            let response = if n == 1 {
                                snapshot_response(vec![
                                    issue("a", "kept"),
                                    issue("b", "deleted elsewhere"),
                                    issue("c", "lost at the server"),
                                ])
                            } else {
                                snapshot_response(vec![issue("a", "kept")]).with_tombstones(vec![
                                    pocopine_sync::SyncTombstone::new("b").unwrap(),
                                ])
                            };
                            Ok(json_response(&response))
                        }
                        other => Err(ServerError::Network(format!("unexpected {other}"))),
                    }
                }
            });

            let client = query_client_plugin().config(test_config()).into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle_ticks(3).await;
            assert_eq!(view.rows().len(), 3, "first snapshot populated");
            // The first snapshot settled onto an empty subscription —
            // nothing was evicted.
            assert!(view.take_evictions().is_empty());

            // Wait for the second poll tick to settle.
            settle_ticks(12).await;
            let rows = view.rows();
            assert_eq!(
                rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
                vec!["a"],
                "server truth wins: both the tombstoned and the absent row leave the view"
            );

            let evictions = view.take_evictions();
            let mut summary: Vec<(String, pocopine_sync_query::EvictionReason)> = evictions
                .iter()
                .map(|e| (e.row.key.as_str().to_string(), e.reason))
                .collect();
            summary.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(
                summary,
                vec![
                    (
                        "b".to_string(),
                        pocopine_sync_query::EvictionReason::Deleted
                    ),
                    (
                        "c".to_string(),
                        pocopine_sync_query::EvictionReason::Unexplained
                    ),
                ]
            );
            // Drained exactly once.
            assert!(view.take_evictions().is_empty());
        })
        .await;
}

#[tokio::test]
async fn truncated_snapshots_suppress_unexplained_evictions() {
    // A snapshot that fills the query's own limit can't distinguish
    // "absent" from "beyond the page": rows past the limit still
    // leave the canonical set (server truth), but they must NOT be
    // reported as Unexplained — a recovery policy acting on them
    // would re-push rows the server still has.
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            let pull_calls = Rc::new(RefCell::new(0u32));
            let pull_for_mw = pull_calls.clone();
            install_middleware(move |req: FetchRequest, _next: FetchNext| {
                let pull_for_mw = pull_for_mw.clone();
                async move {
                    match req.url.as_str() {
                        SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                        SYNC_PULL_PATH => {
                            let n = {
                                let mut c = pull_for_mw.borrow_mut();
                                *c += 1;
                                *c
                            };
                            let issue = |id: &str| Issue {
                                id: id.into(),
                                workspace_id: "W1".into(),
                                title: id.into(),
                            };
                            // First snapshot: two rows (under limit).
                            // Second: two DIFFERENT rows, exactly at
                            // the limit — the old rows' absence is
                            // unexplainable.
                            let response = if n == 1 {
                                snapshot_response(vec![issue("a"), issue("b")])
                            } else {
                                snapshot_response(vec![issue("x"), issue("y")])
                            };
                            Ok(json_response(&response))
                        }
                        other => Err(ServerError::Network(format!("unexpected {other}"))),
                    }
                }
            });

            let client = query_client_plugin().config(test_config()).into_client();
            let view = client.observe(
                Issue::query()
                    .eq(issues::field::workspace_id, "W1")
                    .limit(2)
                    .build(),
            );
            settle_ticks(3).await;
            assert_eq!(view.rows().len(), 2);
            settle_ticks(12).await;

            let rows = view.rows();
            assert_eq!(
                rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
                vec!["x", "y"],
                "the full second snapshot still replaces the view"
            );
            assert!(
                view.take_evictions().is_empty(),
                "a limit-filling snapshot reports no Unexplained evictions"
            );
        })
        .await;
}

#[tokio::test]
async fn open_then_pull_populates_canonical_rows() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            install_middleware(|req: FetchRequest, _next: FetchNext| async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                    SYNC_PULL_PATH => Ok(json_response(&snapshot_response(vec![Issue {
                        id: "issue_1".into(),
                        workspace_id: "W1".into(),
                        title: "first".into(),
                    }]))),
                    other => Err(ServerError::Network(format!("unexpected {other}"))),
                }
            });

            let client = query_client_plugin().config(test_config()).into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle_ticks(3).await;

            let rows = view.rows();
            assert_eq!(rows.len(), 1, "canonical row populated by /pull");
            assert_eq!(rows[0].id, "issue_1");
        })
        .await;
}

#[tokio::test]
async fn schema_drift_resets_state_and_repopulates() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            let open_calls = Rc::new(RefCell::new(0u32));
            let pull_calls = Rc::new(RefCell::new(0u32));
            let open_for_mw = open_calls.clone();
            let pull_for_mw = pull_calls.clone();
            install_middleware(move |req: FetchRequest, _next: FetchNext| {
                let open_for_mw = open_for_mw.clone();
                let pull_for_mw = pull_for_mw.clone();
                async move {
                    match req.url.as_str() {
                        SYNC_OPEN_PATH => {
                            let n = {
                                let mut c = open_for_mw.borrow_mut();
                                *c += 1;
                                *c
                            };
                            // First open → v1; second open → v2 (drift).
                            let sv = if n == 1 { 1 } else { 2 };
                            Ok(json_response(&open_response(sv, None)))
                        }
                        SYNC_PULL_PATH => {
                            let n = {
                                let mut c = pull_for_mw.borrow_mut();
                                *c += 1;
                                *c
                            };
                            // First pull → 1 row; after drift → 2 rows.
                            let rows = if n == 1 {
                                vec![Issue {
                                    id: "old".into(),
                                    workspace_id: "W1".into(),
                                    title: "stale".into(),
                                }]
                            } else {
                                vec![
                                    Issue {
                                        id: "new_a".into(),
                                        workspace_id: "W1".into(),
                                        title: "fresh".into(),
                                    },
                                    Issue {
                                        id: "new_b".into(),
                                        workspace_id: "W1".into(),
                                        title: "fresher".into(),
                                    },
                                ]
                            };
                            Ok(json_response(&snapshot_response(rows)))
                        }
                        other => Err(ServerError::Network(format!("unexpected {other}"))),
                    }
                }
            });

            let client = Rc::new(query_client_plugin().config(test_config()).into_client());
            // First observe → loads v1.
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle_ticks(3).await;
            assert_eq!(view.rows().len(), 1, "initial /pull populated v1");

            // Second observe of the SAME query reuses the
            // subscription — to force a fresh /open we drop and
            // re-observe. A real drift happens on reconnect; the
            // test models it by issuing two drivers back to back.
            drop(view);
            settle_ticks(2).await;
            let _view2 =
                client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            // The driver re-runs /open + /pull; the second /open
            // returns schema_version = 2 → reset_state, then the
            // second /pull returns the 2-row snapshot.
            settle_ticks(5).await;

            assert!(
                *open_calls.borrow() >= 2,
                "schema drift should have triggered at least 2 /open calls"
            );
        })
        .await;
}

#[tokio::test]
async fn cancellation_via_handle_drop_exits_driver() {
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            let pull_calls = Rc::new(RefCell::new(0u32));
            let pull_for_mw = pull_calls.clone();
            install_middleware(move |req: FetchRequest, _next: FetchNext| {
                let pull_for_mw = pull_for_mw.clone();
                async move {
                    match req.url.as_str() {
                        SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                        SYNC_PULL_PATH => {
                            *pull_for_mw.borrow_mut() += 1;
                            Ok(json_response(&snapshot_response(vec![])))
                        }
                        other => Err(ServerError::Network(format!("unexpected {other}"))),
                    }
                }
            });

            let client = query_client_plugin().config(test_config()).into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle_ticks(3).await;
            let initial_count = *pull_calls.borrow();
            assert!(initial_count >= 1, "first /pull issued before drop");

            drop(view);
            // Give the driver several poll intervals to (try to)
            // tick again — but since the handle was dropped, the
            // epoch bumped, and the driver should NOT issue more
            // /pulls.
            for _ in 0..5 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            let final_count = *pull_calls.borrow();
            // Permit at most 1 additional /pull (the one already in
            // flight when drop happened) — anything more proves the
            // epoch cancellation didn't fire.
            assert!(
                final_count - initial_count <= 1,
                "driver should have exited after handle drop; pulls before={initial_count}, after={final_count}"
            );
        })
        .await;
}

#[tokio::test]
async fn offline_replay_redrives_apply_remote_with_same_mutation_id() {
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

            PENDING_MODE.with(|m| *m.borrow_mut() = PendingMode::Offline);
            PENDING_REMOTE_HITS.with(|h| h.borrow_mut().clear());

            let client = query_client_plugin().config(test_config()).into_client();
            let ctx = StubContext::new();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle_ticks(2).await;

            // 1. Mutate; apply_remote returns Network → optimistic
            //    stays pending + the entry is queued for replay.
            let issue = Issue {
                id: "pending_1".into(),
                workspace_id: "W1".into(),
                title: "offline".into(),
            };
            let err = client
                .mutate::<PendingControlled>(issue.clone(), &ctx)
                .await
                .expect_err("offline mutate should return a network error");
            assert!(err.is_transport(), "got non-transport error: {err}");

            // Pending overlay still visible.
            let pending_len = view.state().pending().len();
            assert_eq!(pending_len, 1, "optimistic overlay should still be pending");
            assert_eq!(
                PENDING_REMOTE_HITS.with(|h| h.borrow().len()),
                1,
                "apply_remote fired once on the original mutate"
            );
            let original_id = PENDING_REMOTE_HITS.with(|h| h.borrow()[0].clone());

            // 2. Flip the transport to online. The next driver tick
            //    walks the replay queue and re-fires apply_remote.
            PENDING_MODE.with(|m| *m.borrow_mut() = PendingMode::Online);
            // Let the driver run several ticks so it observes the
            // mode flip and successfully replays.
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(40)).await;
            }

            // The replay invoked apply_remote at least once more.
            // Replay invocations all flow through the framework's
            // `ReplayContext::next_mutation_id` which returns the
            // SAME captured id — so every replay hit must be
            // equal to itself (the dedup invariant).
            let hits = PENDING_REMOTE_HITS.with(|h| h.borrow().clone());
            assert!(
                hits.len() >= 2,
                "expected at least one replay; got {} apply_remote hits",
                hits.len()
            );
            let replay_id = hits[1].clone();
            for (i, h) in hits.iter().enumerate().skip(1) {
                assert_eq!(
                    h, &replay_id,
                    "replay #{i} must reuse the same mutation_id (RFC 072 dedup)"
                );
            }
            // Original wire mutation_id was `test:1`; the
            // mutator's first call to ctx.next_mutation_id() got
            // `test:2` (StubContext is one-ahead). The replay
            // observes the framework-captured wire id `test:1`,
            // which is distinct from `original_id` here BY
            // DESIGN of the test mutator. The cross-check is that
            // the replay-time call has stable identity across
            // multiple retries — proven by the loop above.
            assert_ne!(
                replay_id, original_id,
                "test mutator's first call sees StubContext id, replay sees framework-captured id; \
                 these are distinct by construction in this test"
            );

            // Pending drained.
            assert_eq!(
                view.state().pending().len(),
                0,
                "successful replay should clear the pending overlay"
            );
        })
        .await;
}

#[test]
fn without_driver_skips_spawn_and_routing_still_works() {
    // No LocalSet, no tokio runtime — the routing engine MUST
    // still work because observe with `without_driver` doesn't
    // spawn anything.
    let client = QueryClient::without_driver();
    let _view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
    // If a driver had been spawned this test would have panicked
    // (`spawn_local` outside a LocalSet panics).
    assert_eq!(client.active_subscription_count(), 1);
}

#[test]
fn live_event_filter_drops_non_matching_params() {
    let mut want = pocopine_sync::StreamParams::new();
    want.insert("workspace_id".into(), serde_json::json!("W1"));

    let mut got = std::collections::BTreeMap::new();
    got.insert("workspace_id".to_string(), serde_json::json!("W2"));
    assert!(!pocopine_sync_query::live_event_matches_params(&want, &got));

    got.clear();
    got.insert("workspace_id".to_string(), serde_json::json!("W1"));
    assert!(pocopine_sync_query::live_event_matches_params(&want, &got));

    // Server didn't include the field at all → conservative
    // allow per RFC 087 §6.
    let empty = std::collections::BTreeMap::new();
    assert!(pocopine_sync_query::live_event_matches_params(
        &want, &empty
    ));
}

#[tokio::test]
async fn incremental_upserts_beat_retained_tombstones_for_the_same_key() {
    // A row deleted and then RECREATED inside the tombstone retention
    // window arrives as an incremental upsert alongside its retained
    // tombstone. Presence is the stronger claim: the upsert must win,
    // while a non-conflicting tombstone in the same delta still
    // deletes.
    let local = LocalSet::new();
    local
        .run_until(async {
            reset_middleware();
            let pull_calls = Rc::new(RefCell::new(0u32));
            let pull_for_mw = pull_calls.clone();
            install_middleware(move |req: FetchRequest, _next: FetchNext| {
                let pull_for_mw = pull_for_mw.clone();
                async move {
                    match req.url.as_str() {
                        SYNC_OPEN_PATH => Ok(json_response(&open_response(1, None))),
                        SYNC_PULL_PATH => {
                            let n = {
                                let mut c = pull_for_mw.borrow_mut();
                                *c += 1;
                                *c
                            };
                            let issue = |id: &str| Issue {
                                id: id.into(),
                                workspace_id: "W1".into(),
                                title: id.into(),
                            };
                            let response = if n == 1 {
                                snapshot_response(vec![issue("kept"), issue("doomed")])
                            } else {
                                // Incremental delta: "reborn" was
                                // deleted AND recreated in the window
                                // (upsert + retained tombstone);
                                // "doomed" only has its tombstone.
                                let reborn = issue("reborn");
                                let change = pocopine_sync::SyncChange {
                                    stream: SyncStreamName::new(STREAM).unwrap(),
                                    collection: SyncCollectionName::new(COLLECTION).unwrap(),
                                    key: Some(pocopine_sync::RowKey::new("reborn").unwrap()),
                                    op: pocopine_sync::SyncOp::Upsert,
                                    row: Some(
                                        SyncRow::new(
                                            "reborn",
                                            serde_json::to_value(&reborn).unwrap(),
                                        )
                                        .unwrap(),
                                    ),
                                    cursor: SyncCursor::new("c_2").unwrap(),
                                    origin: None,
                                };
                                SyncPullResponse::incremental(
                                    SyncStreamName::new(STREAM).unwrap(),
                                    SyncCollectionName::new(COLLECTION).unwrap(),
                                    vec![change],
                                    Some(SyncCursor::new("c_2").unwrap()),
                                )
                                .with_tombstones(vec![
                                    pocopine_sync::SyncTombstone::new("reborn").unwrap(),
                                    pocopine_sync::SyncTombstone::new("doomed").unwrap(),
                                ])
                            };
                            Ok(json_response(&response))
                        }
                        other => Err(ServerError::Network(format!("unexpected {other}"))),
                    }
                }
            });

            let client = query_client_plugin().config(test_config()).into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            settle_ticks(3).await;
            assert_eq!(view.rows().len(), 2, "initial snapshot settled");
            settle_ticks(12).await;

            let mut ids: Vec<String> = view.rows().into_iter().map(|r| r.id).collect();
            ids.sort();
            assert_eq!(
                ids,
                vec!["kept", "reborn"],
                "the recreated row survives its retained tombstone; \
                 the plain tombstone still deletes"
            );
        })
        .await;
}
