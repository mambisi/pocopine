#![cfg(not(target_arch = "wasm32"))]

//! The resurrection gauntlet — issue #292 encoded as falsifiable
//! properties, run end-to-end over the real client (driver, durable
//! store, replay queue) against a feed-capable mock server.
//!
//! | test | property |
//! |------|----------|
//! | G1   | the original immortal ghost stays dead across three boots |
//! | G3   | a delete within retention arrives as an explicit feed op |
//! | G4   | a delete beyond retention forces a LOUD resync; zero `Unexplained` |
//! | G5   | an absence-based self-heal finds NOTHING to resurrect |
//! | echo | a lost push ack is healed by the feed echo, exactly once |
//! | digest | a corrupted snapshot is refused, previous state kept |
//!
//! (G2 — the wrong-Row-type drop — is pinned by the erased-dequeue
//! unit test in `client.rs` and the unreplayable-pending test in
//! `persistence.rs`.)
//!
//! Every settle is followed by the Local Coherence check: the
//! durable pending ids must equal the in-memory pending ids — the
//! invariant whose silent violation made #292 possible.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use pocopine_core::fetch::{
    __reset_middleware_chain_for_test, FetchNext, FetchRequest, FetchResponse, install_middleware,
};
use pocopine_core::server::ServerError;
use pocopine_sync::{
    MemoryLocalStore, MutationId, RowKey, SYNC_OPEN_PATH, SYNC_PULL_PATH, SYNC_PUSH_PATH,
    StreamParams, SyncChange, SyncCollectionName, SyncCursor, SyncLocalStore, SyncOp,
    SyncOpenResponse, SyncOpenStream, SyncPullRequest, SyncPullResponse, SyncPushRequest,
    SyncPushResponse, SyncResult, SyncResyncReason, SyncRow, SyncStreamName, local_stream_key,
    snapshot_digest,
};
use pocopine_sync_query::{
    EvictionReason, Mutator, MutatorRemoteContext, MutatorRemoteFuture, QueryClientConfig,
    QueryView, RowChange, query_client_plugin,
};
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::LocalSet;

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

fn issue(id: &str, title: &str) -> Issue {
    Issue {
        id: id.into(),
        workspace_id: "W1".into(),
        title: title.into(),
    }
}

// ─── The mock server: rows + an ordered feed with GC ────────────────

#[derive(Default)]
struct MockServer {
    rows: BTreeMap<String, Issue>,
    /// (seq, op, key, origin) — the ordered change history.
    feed: Vec<(u64, SyncOp, String, Option<MutationId>)>,
    next_seq: u64,
    /// Entries with seq <= gc_floor are forgotten.
    gc_floor: u64,
    /// Mutation ids the push handler has accepted (idempotency log).
    accepted: Vec<MutationId>,
    /// Keys ever pushed to the server by a client (G5's property
    /// reads this: no deleted key may ever re-arrive).
    pushed_keys: Vec<String>,
    /// When true, /push applies server-side but the RESPONSE is
    /// dropped (the lost-ack window).
    drop_push_response: bool,
    /// When Some, /pull snapshots carry this digest INSTEAD of the
    /// correct one (the corruption simulation).
    lie_digest: Option<String>,
}

thread_local! {
    static SERVER: RefCell<MockServer> = RefCell::new(MockServer::default());
}

fn server_reset() {
    SERVER.with(|s| {
        *s.borrow_mut() = MockServer {
            next_seq: 1,
            ..MockServer::default()
        }
    });
}

fn server_upsert(row: Issue, origin: Option<MutationId>) {
    SERVER.with(|s| {
        let mut s = s.borrow_mut();
        let seq = s.next_seq;
        s.next_seq += 1;
        s.rows.insert(row.id.clone(), row.clone());
        s.feed.push((seq, SyncOp::Upsert, row.id, origin));
    });
}

fn server_delete(id: &str, origin: Option<MutationId>) {
    SERVER.with(|s| {
        let mut s = s.borrow_mut();
        let seq = s.next_seq;
        s.next_seq += 1;
        s.rows.remove(id);
        s.feed.push((seq, SyncOp::Delete, id.to_string(), origin));
    });
}

fn server_gc_through(seq: u64) {
    SERVER.with(|s| {
        let mut s = s.borrow_mut();
        s.gc_floor = s.gc_floor.max(seq);
        let floor = s.gc_floor;
        s.feed.retain(|(n, ..)| *n > floor);
    });
}

fn stream_name() -> SyncStreamName {
    SyncStreamName::new(STREAM).unwrap()
}

fn collection_name() -> SyncCollectionName {
    SyncCollectionName::new(COLLECTION).unwrap()
}

fn wire_row(row: &Issue) -> SyncRow<Value> {
    SyncRow::new(row.id.clone(), serde_json::to_value(row).unwrap()).unwrap()
}

/// Answer a /pull honoring the request cursor against the mock
/// server's feed — the same decision tree the real adapter runs.
fn handle_pull(body: &str) -> SyncPullResponse<Value> {
    let request: SyncPullRequest = serde_json::from_str(body).expect("pull request decodes");
    SERVER.with(|s| {
        let s = s.borrow();
        let head = SyncCursor::new((s.next_seq - 1).to_string()).unwrap();
        let watermark = SyncCursor::new(s.gc_floor.to_string()).unwrap();
        let snapshot = |resync: bool| {
            let rows: Vec<SyncRow<Value>> = s.rows.values().map(wire_row).collect();
            let digest = s.lie_digest.clone().unwrap_or_else(|| {
                snapshot_digest(rows.iter().map(|r| (&r.key, r.version.as_ref())))
            });
            let mut response = SyncPullResponse::snapshot(
                stream_name(),
                collection_name(),
                rows,
                Some(head.clone()),
            )
            .with_watermark(Some(watermark.clone()))
            .with_digest(Some(digest));
            if resync {
                response = response.with_resync(SyncResyncReason::CursorTruncated);
            }
            response
        };
        let Some(cursor) = request.cursor.as_ref() else {
            return snapshot(false);
        };
        let Ok(since) = cursor.as_str().parse::<u64>() else {
            return snapshot(true);
        };
        if since < s.gc_floor {
            return snapshot(true);
        }
        let changes: Vec<SyncChange<Value>> = s
            .feed
            .iter()
            .filter(|(seq, ..)| *seq > since)
            .map(|(seq, op, key, origin)| SyncChange {
                stream: stream_name(),
                collection: collection_name(),
                key: Some(RowKey::new(key.clone()).unwrap()),
                op: *op,
                row: match op {
                    SyncOp::Upsert => s.rows.get(key).map(wire_row),
                    _ => None,
                },
                cursor: SyncCursor::new(seq.to_string()).unwrap(),
                origin: origin.clone(),
            })
            .collect();
        SyncPullResponse::incremental(stream_name(), collection_name(), changes, Some(head))
            .with_watermark(Some(watermark))
    })
}

/// Apply a /push server-side (upserts + feed append with origin) and
/// answer accepted — unless `drop_push_response` is armed, in which
/// case the write STILL APPLIES but the response is a network error
/// (the lost-ack window).
fn handle_push(body: &str) -> Result<SyncPushResponse<Value>, ServerError> {
    let request: SyncPushRequest<Value> =
        serde_json::from_str(body).expect("push request decodes");
    let mut response = SyncPushResponse::new(stream_name());
    let drop_response = SERVER.with(|s| s.borrow().drop_push_response);
    for mutation in request.mutations {
        let already = SERVER.with(|s| s.borrow().accepted.contains(&mutation.id));
        if !already {
            let key = mutation
                .key
                .clone()
                .map(|k| k.as_str().to_string())
                .unwrap_or_default();
            SERVER.with(|s| s.borrow_mut().pushed_keys.push(key.clone()));
            match mutation.op {
                SyncOp::Delete => server_delete(&key, Some(mutation.id.clone())),
                _ => {
                    let row: Issue = serde_json::from_value(mutation.payload.clone())
                        .expect("push payload is an Issue row");
                    server_upsert(row, Some(mutation.id.clone()));
                }
            }
            SERVER.with(|s| s.borrow_mut().accepted.push(mutation.id.clone()));
        }
        response.accepted.push(mutation.id);
    }
    if drop_response {
        return Err(ServerError::Network("simulated lost push response".into()));
    }
    Ok(response)
}

fn install_mock_server() {
    __reset_middleware_chain_for_test();
    install_middleware(|req: FetchRequest, _next: FetchNext| async move {
        match req.url.as_str() {
            SYNC_OPEN_PATH => Ok(json_response(&SyncOpenResponse::new(vec![SyncOpenStream {
                stream: stream_name(),
                collection: collection_name(),
                cursor: None,
                schema_version: 1,
                scope: None,
                params: StreamParams::new(),
                watermark: None,
            }]))),
            SYNC_PULL_PATH => Ok(json_response(&handle_pull(&req.body))),
            SYNC_PUSH_PATH => handle_push(&req.body).map(|r| json_response(&r)),
            other => Err(ServerError::Network(format!("unexpected {other}"))),
        }
    });
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

// ─── Client-session helpers ─────────────────────────────────────────

fn config(store: Rc<dyn SyncLocalStore>) -> QueryClientConfig {
    QueryClientConfig {
        poll_interval: Some(Duration::from_millis(50)),
        disable_live: true,
        ..QueryClientConfig::default()
    }
    .with_local_store(store)
}

fn compartment() -> SyncStreamName {
    let mut params = StreamParams::new();
    params.insert("workspace_id".into(), serde_json::json!("W1"));
    local_stream_key(&stream_name(), &params)
}

async fn ticks(n: usize) {
    for _ in 0..n {
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// The Local Coherence invariant's durable half: the store's pending
/// set must equal the in-memory pending set. Its silent violation is
/// what made #292's ghost immortal — assert it after every settle.
async fn assert_coherence(view: &QueryView<Issue>, store: &MemoryLocalStore, context: &str) {
    let mut in_memory: Vec<String> = view
        .state()
        .pending()
        .iter()
        .map(|p| p.mutation_id.as_str().to_string())
        .collect();
    in_memory.sort();
    let mut durable: Vec<String> = store
        .pending_mutations(&compartment())
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id.as_str().to_string())
        .collect();
    durable.sort();
    assert_eq!(
        durable, in_memory,
        "[{context}] durable pending ids must equal in-memory pending ids"
    );
}

/// A deliberately naive absence-based self-heal: re-push every
/// `Unexplained` eviction. The gauntlet's property is that after the
/// SPORC hardening this function NEVER finds a deleted row to
/// resurrect — deletes are explicit ops (G3) or the resync is loud
/// (G4), so `Unexplained` never contains them.
fn naive_self_heal(view: &QueryView<Issue>) -> Vec<String> {
    view.take_evictions()
        .into_iter()
        .filter(|e| e.reason == EvictionReason::Unexplained)
        .map(|e| e.row.key.as_str().to_string())
        .collect()
}

// ─── G1: the original immortal ghost, three boots ───────────────────

#[tokio::test]
async fn g1_envelope_less_ghost_stays_dead_across_three_boots() {
    let local = LocalSet::new();
    local
        .run_until(async {
            server_reset();
            install_mock_server();

            // Seed the EXACT #292 residue: server truth applied
            // (rows: []) plus a stuck pending whose payload has NO
            // envelope — un-replayable by construction — painting a
            // ghost via its optimistic row.
            let store = Rc::new(MemoryLocalStore::new());
            let ghost = issue("ghost_1", "deleted long ago");
            let mutation = pocopine_sync::ClientMutation::new(
                MutationId::new("device_old:3").unwrap(),
                SyncOp::Upsert,
                serde_json::to_value(&ghost).unwrap(),
            )
            .key("ghost_1")
            .unwrap();
            let pending = pocopine_sync::LocalPendingMutation::new(mutation).with_optimistic_row(
                Some(wire_row(&ghost).pending(true)),
            );
            store
                .enqueue_pending_mutation(&compartment(), pending)
                .await
                .unwrap();

            for boot in 1..=3 {
                let store_handle: Rc<dyn SyncLocalStore> = store.clone();
                let client = query_client_plugin()
                    .config(config(store_handle))
                    .into_client();
                let view =
                    client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
                ticks(8).await;

                assert!(
                    view.rows().iter().all(|r| r.id != "ghost_1"),
                    "boot {boot}: the ghost must not render"
                );
                assert_eq!(
                    view.state().pending().len(),
                    0,
                    "boot {boot}: no pending overlay survives"
                );
                let durable = store.pending_mutations(&compartment()).await.unwrap();
                assert!(
                    durable.is_empty(),
                    "boot {boot}: the durable residue is gone — nothing to re-hydrate"
                );
                assert_coherence(&view, &store, &format!("G1 boot {boot}")).await;

                drop(view);
                drop(client);
                ticks(2).await;
            }

            // And the server never saw a resurrection push.
            let pushed = SERVER.with(|s| s.borrow().pushed_keys.clone());
            assert!(
                pushed.is_empty(),
                "no client push may resurrect the ghost: {pushed:?}"
            );
        })
        .await;
}

// ─── G3 + G5: delete within retention is an explicit op ─────────────

#[tokio::test]
async fn g3_delete_within_retention_arrives_as_feed_op_and_heals_nothing() {
    let local = LocalSet::new();
    local
        .run_until(async {
            server_reset();
            install_mock_server();
            server_upsert(issue("a", "keep me"), None);
            server_upsert(issue("b", "delete me"), None);

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(config(store_handle))
                .into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            ticks(6).await;
            assert_eq!(view.rows().len(), 2, "initial snapshot synced");
            assert!(naive_self_heal(&view).is_empty());

            // The server deletes "b" while we're subscribed; the next
            // cursored poll must deliver it as an EXPLICIT op.
            server_delete("b", None);
            ticks(6).await;

            let ids: Vec<String> = view.rows().into_iter().map(|r| r.id).collect();
            assert_eq!(ids, vec!["a".to_string()], "the delete applied");
            // The durable snapshot agrees (no ghost on next boot).
            let persisted = store.hydrate_stream(&compartment()).await.unwrap();
            let persisted_ids: Vec<&str> =
                persisted.rows.iter().map(|r| r.key.as_str()).collect();
            assert_eq!(persisted_ids, vec!["a"]);

            // The eviction is classified Deleted — the naive heal
            // finds NOTHING Unexplained to resurrect.
            let evictions = view.take_evictions();
            assert!(
                evictions
                    .iter()
                    .any(|e| e.row.key.as_str() == "b" && e.reason == EvictionReason::Deleted),
                "the delete is a positive Deleted eviction"
            );
            assert!(
                evictions
                    .iter()
                    .all(|e| e.reason != EvictionReason::Unexplained),
                "zero Unexplained evictions in feed mode"
            );
            assert_coherence(&view, &store, "G3").await;
            let pushed = SERVER.with(|s| s.borrow().pushed_keys.clone());
            assert!(pushed.is_empty(), "nothing re-pushed: {pushed:?}");
        })
        .await;
}

// ─── G4 + G5: delete beyond retention is LOUD, never ambiguous ──────

#[tokio::test]
async fn g4_delete_beyond_retention_forces_loud_resync_with_zero_unexplained() {
    let local = LocalSet::new();
    local
        .run_until(async {
            server_reset();
            install_mock_server();
            server_upsert(issue("a", "keep me"), None);
            server_upsert(issue("b", "delete me while offline"), None);

            let store = Rc::new(MemoryLocalStore::new());

            // Session 1: sync both rows, then go offline.
            {
                let store_handle: Rc<dyn SyncLocalStore> = store.clone();
                let client = query_client_plugin()
                    .config(config(store_handle))
                    .into_client();
                let view =
                    client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
                ticks(6).await;
                assert_eq!(view.rows().len(), 2);
                drop(view);
                drop(client);
                ticks(2).await;
            }

            // While offline: the delete happens AND the feed is GC'd
            // past it — this client can never learn of it
            // incrementally.
            server_delete("b", None);
            let head = SERVER.with(|s| s.borrow().next_seq - 1);
            server_gc_through(head);

            // Session 2: reboot. The cursored pull answers TooOld →
            // loud snapshot resync.
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(config(store_handle))
                .into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            ticks(8).await;

            let ids: Vec<String> = view.rows().into_iter().map(|r| r.id).collect();
            assert_eq!(ids, vec!["a".to_string()], "the resync snapshot is current truth");

            // The absence of "b" is KNOWN-stale, never Unexplained —
            // so the naive heal has nothing to resurrect.
            let evictions = view.take_evictions();
            assert!(
                evictions
                    .iter()
                    .any(|e| e.row.key.as_str() == "b" && e.reason == EvictionReason::StaleResync),
                "the offline-past-retention absence classifies StaleResync, got: {:?}",
                evictions.iter().map(|e| (e.row.key.as_str(), e.reason)).collect::<Vec<_>>()
            );
            assert!(
                evictions
                    .iter()
                    .all(|e| e.reason != EvictionReason::Unexplained),
                "zero Unexplained evictions on a loud resync"
            );
            assert_coherence(&view, &store, "G4").await;
            let pushed = SERVER.with(|s| s.borrow().pushed_keys.clone());
            assert!(
                pushed.is_empty(),
                "the deleted row was never re-pushed: {pushed:?}"
            );
        })
        .await;
}

// ─── Lost ack: the feed echo retires the pending exactly once ───────

thread_local! {
    static ECHO_HITS: RefCell<Vec<MutationId>> = const { RefCell::new(Vec::new()) };
}

/// Mutator whose remote apply goes through the REAL /push wire — so
/// the mock server's lost-ack switch applies to it.
struct WireCreate;

impl Mutator for WireCreate {
    type Payload = Issue;
    type Row = Issue;
    const NAME: &'static str = "wire_create";
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
        let url = ctx.push_url().to_string();
        Box::pin(async move {
            let id = id?;
            ECHO_HITS.with(|h| h.borrow_mut().push(id.clone()));
            let mutation = pocopine_sync::ClientMutation::new(
                id,
                SyncOp::Upsert,
                serde_json::to_value(&payload)
                    .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?,
            )
            .key(payload.id.clone())
            .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;
            let request = SyncPushRequest::new(stream_name(), [mutation]);
            let response = pocopine_core::fetch::call::<
                SyncPushRequest<Value>,
                SyncPushResponse<Value>,
            >(&url, &request)
            .await
            .map_err(|e| pocopine_sync::SyncError::network(format!("push failed: {e}")))?;
            let _ = response;
            Ok(vec![RowChange::Upsert(payload)])
        })
    }
}

/// Per-mutation context: returns a STABLE id so the client core and
/// the mutator's own apply_remote agree on the mutation identity
/// (the framework's ReplayCtx has the same fixed-id contract).
struct SeqContext {
    id: u64,
}

impl MutatorRemoteContext for SeqContext {
    fn push_url(&self) -> &str {
        "/__pocopine/sync/v1/push"
    }
    fn next_mutation_id(&self) -> SyncResult<MutationId> {
        MutationId::new(format!("device_echo:{}", self.id))
    }
}

#[tokio::test]
async fn lost_push_ack_is_healed_by_the_feed_echo_exactly_once() {
    let local = LocalSet::new();
    local
        .run_until(async {
            server_reset();
            install_mock_server();
            server_upsert(issue("a", "existing"), None);
            ECHO_HITS.with(|h| h.borrow_mut().clear());

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(config(store_handle))
                .into_client();
            client.register_mutator::<WireCreate>();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            ticks(6).await;
            assert_eq!(view.rows().len(), 1);

            // Arm the lost-ack window: the server APPLIES the write
            // (row + feed entry with origin) but the response is a
            // network error.
            SERVER.with(|s| s.borrow_mut().drop_push_response = true);
            let ctx = SeqContext { id: 1 };
            let err = client
                .mutate::<WireCreate>(issue("x1", "pushed into the void"), &ctx)
                .await
                .expect_err("the ack was lost — mutate reports transport failure");
            assert!(err.is_transport());
            assert_eq!(
                view.state().pending().len(),
                1,
                "the overlay stays queued for replay (transport path)"
            );
            // Server-side the write LANDED.
            assert!(SERVER.with(|s| s.borrow().rows.contains_key("x1")));

            // Disarm; the next cursored poll carries the FEED ECHO
            // (origin = our mutation id) which retires the pending —
            // even though no push ack ever arrived.
            SERVER.with(|s| s.borrow_mut().drop_push_response = false);
            ticks(8).await;

            assert_eq!(
                view.state().pending().len(),
                0,
                "the feed echo retired the pending"
            );
            let ids: Vec<String> = view.rows().into_iter().map(|r| r.id).collect();
            assert!(ids.iter().any(|id| id == "x1"), "the canonical row renders: {ids:?}");
            assert_coherence(&view, &store, "echo").await;

            // Exactly-once server-side: the replay tick may re-push
            // the same mutation id, but the idempotency log absorbs
            // it — the row was applied exactly once.
            let applied = SERVER.with(|s| {
                s.borrow()
                    .feed
                    .iter()
                    .filter(|(_, _, key, _)| key == "x1")
                    .count()
            });
            assert_eq!(applied, 1, "the write applied exactly once");
        })
        .await;
}

// ─── Digest corruption: fail-stop, keep the previous truth ──────────

#[tokio::test]
async fn corrupted_snapshot_is_refused_and_previous_state_kept() {
    let local = LocalSet::new();
    local
        .run_until(async {
            server_reset();
            install_mock_server();
            server_upsert(issue("a", "truth"), None);
            server_upsert(issue("b", "also truth"), None);

            let store = Rc::new(MemoryLocalStore::new());
            let store_handle: Rc<dyn SyncLocalStore> = store.clone();
            let client = query_client_plugin()
                .config(config(store_handle))
                .into_client();
            let view = client.observe(Issue::query().eq(issues::field::workspace_id, "W1").build());
            ticks(6).await;
            assert_eq!(view.rows().len(), 2, "clean initial sync");

            // A middleman starts rewriting bodies: the digest no
            // longer matches the rows. Reset the client's cursor
            // path by wiping the feed coverage so polls re-snapshot.
            SERVER.with(|s| {
                s.borrow_mut().lie_digest = Some("sha256:corrupt".to_string());
                s.borrow_mut().rows.remove("b"); // the "truncated" body
            });
            let head = SERVER.with(|s| s.borrow().next_seq - 1);
            // Floor must EXCEED the client's cursor (== head) to force
            // the snapshot path; floor == head still serves an empty
            // incremental page.
            server_gc_through(head + 1);
            ticks(6).await;

            // Fail-stop: the corrupt snapshot did NOT settle — the
            // view still shows the last good truth.
            assert_eq!(
                view.rows().len(),
                2,
                "corrupt snapshot refused; previous state kept"
            );
            assert!(
                !view.state().error.is_empty(),
                "the refusal is loud (error surfaced on the view)"
            );

            // The middleman goes away; the next poll settles clean.
            SERVER.with(|s| s.borrow_mut().lie_digest = None);
            ticks(6).await;
            let ids: Vec<String> = view.rows().into_iter().map(|r| r.id).collect();
            assert_eq!(ids, vec!["a".to_string()], "clean snapshot settles normally");
        })
        .await;
}
