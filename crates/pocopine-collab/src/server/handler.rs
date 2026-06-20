//! The server-side collab handler: a [`pocopine_realtime::SubprotocolHandler`]
//! that runs the Yjs sync handshake over the gateway (RFC 073 Part II).
//!
//! [`CollabSync`] holds one authoritative [`CollabDocument`] per topic and
//! reacts to each inbound collab message:
//!
//! - **SyncStep1** (a peer's state vector) → reply to that peer with the diff it
//!   is missing (SyncStep2) *and* the server's own state vector (SyncStep1), so
//!   the peer sends back what the server lacks. This is the two-way handshake.
//! - **SyncStep2 / Update** (state the server was missing / a live edit) → apply
//!   it to the authoritative document and broadcast it as an Update so every
//!   other subscriber — including peers on other processes — converges.
//!
//! Because CRDT merges are commutative and idempotent, the unordered
//! reply-vs-broadcast delivery the gateway provides is safe, and a client
//! applying its own echoed Update is a no-op.
//!
//! Write access is enforced via the gateway's write policy
//! ([`InboundData::can_write`]): a read-only connection may run SyncStep1 (read
//! down) but its Update / SyncStep2 messages are refused, and it is never
//! prompted with the server's SyncStep1. This is the realtime-layer counterpart
//! of [`CollabAccess`](crate::doc::CollabAccess).
//!
//! Every process **self-subscribes** to each topic's fan-out and folds peer
//! updates into its local document, so replicas behind a multi-process fan-out
//! (Redis) converge — not just the clients of one process. This closes the gap
//! where the document was only ever mutated by inbound frames on the local
//! connection. Durable load/compaction through
//! [`CollabStore`](super::store::CollabStore) is the next step.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use pocopine_events::Topic;
use pocopine_observe::LOG_TARGET;
use pocopine_realtime::{Fanout, InboundData, Reaction, SubprotocolHandler, TopicStream, WsError};
use tokio::task::JoinHandle;

use super::store::{CollabSnapshot, CollabStore};
use crate::protocol::CollabMessage;
use crate::sync::CollabDocument;

/// Save a checkpoint snapshot once this many fan-out updates have been folded
/// in. Far below the fan-out's retention window, so the snapshot cursor always
/// stays replayable and the durable base never lags into an eviction gap.
const CHECKPOINT_EVERY: u64 = 64;

/// Server-side CRDT collaboration over the realtime gateway.
///
/// Holds one authoritative document per topic, each behind its own `Mutex` (a
/// slow op on one topic never blocks another). Crucially, [`CollabSync`] owns
/// the SAME [`Fanout`] the gateway publishes to and spawns a per-topic apply
/// loop that folds every fanned-out update — including those published by other
/// processes — into the local document. Without this, a process's document only
/// ever saw its own clients' edits and replicas behind a Redis fan-out diverged.
///
/// With a [`CollabStore`] ([`Self::with_store`]) the loop also loads the durable
/// snapshot on start and checkpoints the folded document back, so state survives
/// process restart and fan-out retention eviction.
pub struct CollabSync {
    fanout: Arc<dyn Fanout>,
    store: Option<Arc<dyn CollabStore>>,
    checkpoint_every: u64,
    topics: Mutex<HashMap<Topic, Arc<TopicState>>>,
}

/// Per-topic state: the document and the apply loop keeping it converged.
struct TopicState {
    doc: Arc<Mutex<CollabDocument>>,
    /// The convergence apply loop. Aborted when the topic goes idle (its last
    /// local subscriber leaves); the document then reloads from the store on the
    /// next subscriber.
    apply_loop: JoinHandle<()>,
}

impl Drop for TopicState {
    fn drop(&mut self) {
        // A dropped `JoinHandle` only DETACHES the task, leaving the loop parked
        // on the fan-out forever (it holds its own `Fanout`/`Doc` clones). Abort
        // on every drop path — eviction, `CollabSync` drop, reconfiguration — so
        // eviction is not the only way the loop stops. `abort` is idempotent.
        self.apply_loop.abort();
    }
}

impl CollabSync {
    /// Build a handler over the fan-out the gateway also publishes to. They MUST
    /// be the same [`Fanout`] instance, or peer updates will not converge.
    pub fn new(fanout: Arc<dyn Fanout>) -> Self {
        Self {
            fanout,
            store: None,
            checkpoint_every: CHECKPOINT_EVERY,
            topics: Mutex::new(HashMap::new()),
        }
    }

    /// Attach a durable [`CollabStore`]: each topic's apply loop then loads the
    /// snapshot on start and checkpoints the folded document back periodically.
    pub fn with_store(mut self, store: Arc<dyn CollabStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Checkpoint to the [`CollabStore`] every `n` folded updates (default
    /// [`CHECKPOINT_EVERY`]). Lower trades more write load for a fresher durable
    /// base; keep it well under the fan-out retention window.
    pub fn with_checkpoint_every(mut self, n: u64) -> Self {
        self.checkpoint_every = n.max(1);
        self
    }

    /// The per-topic state, created — and its apply loop spawned — on first
    /// access.
    fn topic_state(&self, topic: &Topic) -> Result<Arc<TopicState>, WsError> {
        let mut topics = self
            .topics
            .lock()
            .map_err(|_| WsError::backend("collab topic map poisoned"))?;
        if let Some(state) = topics.get(topic) {
            return Ok(state.clone());
        }
        let doc = Arc::new(Mutex::new(CollabDocument::new()));
        let apply_loop = tokio::spawn(run_apply_loop(
            self.fanout.clone(),
            self.store.clone(),
            self.checkpoint_every,
            topic.clone(),
            doc.clone(),
        ));
        let state = Arc::new(TopicState { doc, apply_loop });
        topics.insert(topic.clone(), state.clone());
        Ok(state)
    }
}

#[async_trait]
impl SubprotocolHandler for CollabSync {
    async fn on_data(&self, inbound: InboundData<'_>) -> Result<Reaction, WsError> {
        let message = CollabMessage::decode(inbound.payload)
            .map_err(|err| WsError::protocol(err.to_string()))?;

        // Ephemeral awareness (presence/cursors) never touches the document: relay
        // it to peers verbatim without taking the doc lock. It is high-frequency (a
        // cursor move per keystroke), so keeping it off the lock avoids contending
        // the convergence apply loop; and it is allowed from read-only peers — a
        // viewer's cursor is presence, not a mutation.
        if let CollabMessage::Awareness(_) = message {
            let mut reaction = Reaction::new();
            reaction.broadcast(inbound.payload.clone());
            return Ok(reaction);
        }

        let state = self.topic_state(inbound.topic)?;
        let doc = state
            .doc
            .lock()
            .map_err(|_| WsError::backend("collab document mutex poisoned"))?;

        let mut reaction = Reaction::new();
        match message {
            CollabMessage::SyncStep1(state_vector) => {
                // Reply with what this peer is missing (read access is enough).
                let diff = doc
                    .diff(&state_vector)
                    .map_err(|err| WsError::protocol(err.to_string()))?;
                reaction.reply(CollabMessage::SyncStep2(Bytes::from(diff)).encode());
                // Only writers are asked for what WE lack: prompting a read-only
                // peer with our state vector would invite a write it cannot make.
                if inbound.can_write {
                    reaction
                        .reply(CollabMessage::SyncStep1(Bytes::from(doc.state_vector())).encode());
                }
            }
            CollabMessage::SyncStep2(update) => {
                ensure_writable(&inbound)?;
                apply_update(&doc, &update)?;
                // Relabel a handshake SyncStep2 as a live Update for peers.
                reaction.broadcast(CollabMessage::Update(update).encode());
            }
            CollabMessage::Update(update) => {
                ensure_writable(&inbound)?;
                apply_update(&doc, &update)?;
                // Already a tagged Update on the wire — forward the original
                // payload verbatim (a cheap `Bytes` refcount bump, no re-encode).
                reaction.broadcast(inbound.payload.clone());
            }
            // Relayed before the doc lock above; never reaches here.
            CollabMessage::Awareness(_) => unreachable!("awareness is relayed pre-lock"),
        }
        Ok(reaction)
    }

    /// First local subscriber: start the topic's convergence apply loop so this
    /// process folds in peer edits even before any local client writes.
    fn on_topic_active(&self, topic: &Topic) {
        // Creating the state spawns the apply loop. Errors only on a poisoned
        // lock, where the next `on_data` will surface it.
        let _ = self.topic_state(topic);
    }

    /// Last local subscriber left: free the topic's document and stop its apply
    /// loop. State is durable (checkpointed to the store) and reloads on the
    /// next subscriber, so this is pure resource reclamation.
    fn on_topic_idle(&self, topic: &Topic) {
        let evicted = self
            .topics
            .lock()
            .ok()
            .and_then(|mut topics| topics.remove(topic));
        if let Some(state) = evicted {
            state.apply_loop.abort();
        }
    }
}

/// Apply an inbound `update` to the local document.
///
/// We deliberately do NOT suppress "no-op-looking" updates by comparing state
/// vectors: a yrs delete-only update does not advance the state vector (deletes
/// live in the delete-set), and an out-of-order update that references
/// not-yet-integrated state is buffered as *pending* without advancing it
/// either. Both are real edits that MUST still be fanned out, so the caller
/// broadcasts unconditionally on success. (Suppressing a genuinely-empty
/// update would need to inspect the update's own content, not the doc's SV.)
fn apply_update(doc: &CollabDocument, update: &[u8]) -> Result<(), WsError> {
    doc.apply_update(update)
        .map_err(|err| WsError::protocol(err.to_string()))
}

/// Refuse a document-mutating message (Update / SyncStep2) from a connection the
/// gateway's write policy marked read-only.
fn ensure_writable(inbound: &InboundData<'_>) -> Result<(), WsError> {
    if inbound.can_write {
        Ok(())
    } else {
        Err(WsError::forbidden("read-only collab connection"))
    }
}

/// Subscribe to `topic`'s fan-out and fold every update into `doc` forever, so
/// edits made through *other* processes converge into this process's document
/// (the local `on_data` path only ever sees this process's own clients). yrs
/// makes re-applying our own published updates a no-op, so this is safe to run
/// alongside the optimistic apply in `on_data`.
///
/// With a [`CollabStore`] the loop also seeds the document from the durable
/// snapshot on start (resuming the fan-out at the snapshot's cursor) and
/// checkpoints the folded document back every [`CHECKPOINT_EVERY`] updates.
async fn run_apply_loop(
    fanout: Arc<dyn Fanout>,
    store: Option<Arc<dyn CollabStore>>,
    checkpoint_every: u64,
    topic: Topic,
    doc: Arc<Mutex<CollabDocument>>,
) {
    let doc_key = topic.as_str();

    // Seed from the durable snapshot, then resume the fan-out at its cursor.
    let mut after = None;
    if let Some(store) = &store {
        match store.load_snapshot(doc_key).await {
            Ok(Some(snapshot)) => {
                if let Ok(doc) = doc.lock() {
                    let _ = doc.apply_update(&snapshot.blob);
                }
                after = Some(snapshot.last_seq);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(target: LOG_TARGET, error = %err, topic = doc_key, "collab apply loop: load_snapshot failed");
            }
        }
    }

    let mut stream = match subscribe_recovering(&fanout, &topic, after).await {
        Some(stream) => stream,
        None => return,
    };

    // The durable cursor must only move FORWARD. `highest_seq` tracks the
    // furthest seq folded; a recovery replay (which re-reads the retained tail
    // from a lower seq) must never checkpoint a cursor below what we already
    // persisted, or a restart could resume into an evicted gap.
    let mut highest_seq = after.unwrap_or(0);
    let mut last_checkpointed = after.unwrap_or(0);
    let mut folded = 0u64;
    loop {
        match stream.next().await {
            Ok(Some((seq, payload))) => {
                // Broadcasts are tagged `Update` messages; a malformed or
                // non-Update frame must never kill the convergence loop.
                if let Ok(CollabMessage::Update(update)) = CollabMessage::decode(&payload)
                    && let Ok(doc) = doc.lock()
                {
                    let _ = doc.apply_update(&update);
                }
                highest_seq = highest_seq.max(seq);
                folded += 1;
                if let Some(store) = &store
                    && folded >= checkpoint_every
                {
                    folded = 0;
                    if highest_seq > last_checkpointed {
                        checkpoint(store.as_ref(), doc_key, &doc, highest_seq).await;
                        last_checkpointed = highest_seq;
                    }
                }
            }
            // Fan-out closed (topic torn down / shutdown).
            Ok(None) => break,
            // Lagged or gapped: re-subscribe to replay the retained tail and
            // resume — never die silently on a recoverable hiccup.
            Err(_) => match subscribe_recovering(&fanout, &topic, None).await {
                Some(replacement) => stream = replacement,
                None => break,
            },
        }
    }
}

/// Subscribe to `topic` at `after`, transparently recovering an unreplayable
/// cursor by replaying the whole retained tail. Returns `None` if the fan-out
/// itself is unreachable.
async fn subscribe_recovering(
    fanout: &Arc<dyn Fanout>,
    topic: &Topic,
    after: Option<u64>,
) -> Option<TopicStream> {
    match fanout.subscribe(topic, after).await {
        Ok(stream) if !stream.gap() => Some(stream),
        Ok(_gapped) => {
            // The snapshot cursor aged past retention; replay what is retained.
            // The evicted middle is unrecoverable (the retention bound).
            tracing::warn!(target: LOG_TARGET, topic = topic.as_str(), "collab apply loop: resume gap, replaying retained tail");
            match fanout.subscribe(topic, None).await {
                Ok(stream) => Some(stream),
                Err(err) => {
                    tracing::warn!(target: LOG_TARGET, error = %err, "collab apply loop: subscribe failed");
                    None
                }
            }
        }
        Err(err) => {
            tracing::warn!(target: LOG_TARGET, error = %err, "collab apply loop: subscribe failed");
            None
        }
    }
}

/// Persist the folded document as the new durable base, current to `last_seq`.
/// The document lock is never held across the `.await`.
async fn checkpoint(
    store: &dyn CollabStore,
    doc_key: &str,
    doc: &Arc<Mutex<CollabDocument>>,
    last_seq: u64,
) {
    let snapshot = {
        let Ok(doc) = doc.lock() else { return };
        CollabSnapshot {
            blob: Bytes::from(doc.full_update()),
            state_vector: Bytes::from(doc.state_vector()),
            last_seq,
        }
    };
    if let Err(err) = store.save_snapshot(doc_key, snapshot).await {
        tracing::warn!(target: LOG_TARGET, error = %err, topic = doc_key, "collab apply loop: save_snapshot failed");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pocopine_realtime::LocalFanout;

    use super::*;

    /// A handler over a fresh in-process fan-out (no peers).
    fn sync() -> CollabSync {
        CollabSync::new(Arc::new(LocalFanout::new()))
    }

    /// Decode one collab message from raw frame bytes.
    fn decode(bytes: &Bytes) -> CollabMessage {
        CollabMessage::decode(bytes).expect("decode collab message")
    }

    /// Feed one inbound collab message from a writer and return its reaction.
    async fn feed(server: &CollabSync, topic: &Topic, message: CollabMessage) -> Reaction {
        feed_as(server, topic, message, true)
            .await
            .expect("handler accepted the message")
    }

    /// Feed one message with an explicit write capability, returning the raw
    /// result so read-only rejections can be asserted.
    async fn feed_as(
        server: &CollabSync,
        topic: &Topic,
        message: CollabMessage,
        can_write: bool,
    ) -> Result<Reaction, WsError> {
        let payload = message.encode();
        server
            .on_data(InboundData {
                topic,
                payload: &payload,
                can_write,
            })
            .await
    }

    #[tokio::test]
    async fn an_update_is_applied_and_broadcast() {
        let server = sync();
        let topic = Topic::new("collab:doc").unwrap();

        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "hello");
        let reaction = feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(edit.full_update())),
        )
        .await;

        // An update fans out to everyone and replies to no one.
        assert!(reaction.replies().is_empty());
        assert_eq!(reaction.broadcasts().len(), 1);
        assert!(matches!(
            decode(&reaction.broadcasts()[0]),
            CollabMessage::Update(_)
        ));
    }

    #[tokio::test]
    async fn awareness_is_relayed_verbatim_and_never_applied() {
        let server = sync();
        let topic = Topic::new("collab:doc").unwrap();

        let presence = Bytes::from_static(b"cursor@42");
        let reaction = feed(&server, &topic, CollabMessage::Awareness(presence.clone())).await;

        // Relayed to peers (broadcast), no direct reply, body preserved verbatim.
        assert!(reaction.replies().is_empty());
        assert_eq!(reaction.broadcasts().len(), 1);
        assert_eq!(
            decode(&reaction.broadcasts()[0]),
            CollabMessage::Awareness(presence)
        );
    }

    #[tokio::test]
    async fn awareness_is_allowed_from_a_read_only_peer() {
        // A viewer's cursor is presence, not a mutation — read-only peers may
        // publish awareness even though their Update/SyncStep2 are refused.
        let server = sync();
        let topic = Topic::new("collab:doc").unwrap();

        let reaction = feed_as(
            &server,
            &topic,
            CollabMessage::Awareness(Bytes::from_static(b"viewing")),
            false,
        )
        .await
        .expect("read-only awareness is accepted");
        assert_eq!(reaction.broadcasts().len(), 1);
    }

    #[tokio::test]
    async fn a_delete_only_update_is_still_broadcast() {
        // Regression guard: a delete does NOT advance the yrs state vector
        // (deletes live in the delete-set), so the old state-vector "no-op
        // guard" silently dropped every deletion from the fan-out. It must be
        // fanned out like any other edit.
        let server = sync();
        let topic = Topic::new("collab:doc").unwrap();

        // Seed "hello" on the server.
        let base = CollabDocument::new();
        base.insert_text("body", 0, "hello");
        feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(base.full_update())),
        )
        .await;

        // Produce a delete-only delta: load the same state, delete, diff.
        let editor = CollabDocument::from_snapshot(&base.full_update()).unwrap();
        let before = editor.state_vector();
        editor.delete_text("body", 0, 2); // remove "he"
        let delete_delta = editor.diff(&before).unwrap();

        let reaction = feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(delete_delta)),
        )
        .await;
        assert_eq!(
            reaction.broadcasts().len(),
            1,
            "a delete-only update must still be fanned out"
        );
    }

    #[tokio::test]
    async fn an_out_of_order_update_is_still_broadcast() {
        // Regression guard: an update that references not-yet-integrated state
        // is buffered by yrs as pending and does NOT advance the state vector,
        // so the old guard dropped it — yet it is a real edit peers need.
        let server = sync();
        let topic = Topic::new("collab:doc").unwrap();

        // Build U2 ("def" at index 3) that causally depends on U1 ("abc"),
        // which the server is NOT given — U2 will buffer pending on apply.
        let source = CollabDocument::new();
        source.insert_text("body", 0, "abc");
        let before_u2 = source.state_vector();
        source.insert_text("body", 3, "def");
        let u2 = source.diff(&before_u2).unwrap();

        let reaction = feed(&server, &topic, CollabMessage::Update(Bytes::from(u2))).await;
        assert_eq!(
            reaction.broadcasts().len(),
            1,
            "an out-of-order (pending) update must still be fanned out"
        );
    }

    #[tokio::test]
    async fn handshake_converges_server_and_client_both_ways() {
        let server = sync();
        let topic = Topic::new("collab:doc").unwrap();

        // Seed the server with prior shared state (an earlier peer's edit).
        let seed = CollabDocument::new();
        seed.insert_text("body", 0, "shared ");
        feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(seed.full_update())),
        )
        .await;

        // A fresh client carrying its own concurrent edit starts the handshake.
        let client = CollabDocument::new();
        client.insert_text("body", 0, "local ");

        // Client → SyncStep1(client_sv); server replies SyncStep2 + SyncStep1.
        let reaction = feed(
            &server,
            &topic,
            CollabMessage::SyncStep1(Bytes::from(client.state_vector())),
        )
        .await;
        assert!(reaction.broadcasts().is_empty());
        assert_eq!(reaction.replies().len(), 2);

        // Apply the server's catch-up; the client now holds the shared state.
        let catch_up = match decode(&reaction.replies()[0]) {
            CollabMessage::SyncStep2(update) => update,
            other => panic!("expected SyncStep2, got {other:?}"),
        };
        let server_sv = match decode(&reaction.replies()[1]) {
            CollabMessage::SyncStep1(sv) => sv,
            other => panic!("expected SyncStep1, got {other:?}"),
        };
        client.apply_update(&catch_up).unwrap();
        assert!(client.text("body").contains("shared"));

        // Client → SyncStep2(what the server is missing).
        let client_diff = client.diff(&server_sv).unwrap();
        let reaction = feed(
            &server,
            &topic,
            CollabMessage::SyncStep2(Bytes::from(client_diff)),
        )
        .await;
        assert!(reaction.replies().is_empty());
        assert_eq!(reaction.broadcasts().len(), 1, "the client's edit fans out");

        // The authoritative server doc now holds BOTH edits — prove it by
        // syncing a brand-new peer from scratch.
        let fresh = CollabDocument::new();
        let reaction = feed(
            &server,
            &topic,
            CollabMessage::SyncStep1(Bytes::from(fresh.state_vector())),
        )
        .await;
        let full = match decode(&reaction.replies()[0]) {
            CollabMessage::SyncStep2(update) => update,
            other => panic!("expected SyncStep2, got {other:?}"),
        };
        fresh.apply_update(&full).unwrap();
        let text = fresh.text("body");
        assert!(
            text.contains("shared") && text.contains("local"),
            "got {text:?}"
        );
    }

    #[tokio::test]
    async fn topics_are_isolated() {
        let server = sync();
        let a = Topic::new("collab:a").unwrap();
        let b = Topic::new("collab:b").unwrap();

        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "in-a");
        feed(
            &server,
            &a,
            CollabMessage::Update(Bytes::from(edit.full_update())),
        )
        .await;

        // Topic b is untouched: a fresh peer there catches up to an empty doc.
        let fresh = CollabDocument::new();
        let reaction = feed(
            &server,
            &b,
            CollabMessage::SyncStep1(Bytes::from(fresh.state_vector())),
        )
        .await;
        let full = match decode(&reaction.replies()[0]) {
            CollabMessage::SyncStep2(update) => update,
            other => panic!("expected SyncStep2, got {other:?}"),
        };
        fresh.apply_update(&full).unwrap();
        assert_eq!(fresh.text("body"), "");
    }

    #[tokio::test]
    async fn rejects_a_malformed_payload() {
        let server = sync();
        let topic = Topic::new("collab:doc").unwrap();
        let empty = Bytes::new();
        let err = server
            .on_data(InboundData {
                topic: &topic,
                payload: &empty,
                can_write: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, WsError::Protocol(_)));
    }

    #[tokio::test]
    async fn read_only_connection_can_sync_down_but_not_write() {
        let server = sync();
        let topic = Topic::new("collab:doc").unwrap();

        // Seed the server with some state (via a writer).
        let seed = CollabDocument::new();
        seed.insert_text("body", 0, "shared");
        feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(seed.full_update())),
        )
        .await;

        // A read-only peer's SyncStep1 is honored (it may catch up) but it is
        // NOT prompted with the server's SyncStep1 (no write is invited).
        let viewer = CollabDocument::new();
        let reaction = feed_as(
            &server,
            &topic,
            CollabMessage::SyncStep1(Bytes::from(viewer.state_vector())),
            false,
        )
        .await
        .expect("read is allowed");
        assert_eq!(reaction.replies().len(), 1, "only the catch-up SyncStep2");
        assert!(matches!(
            decode(&reaction.replies()[0]),
            CollabMessage::SyncStep2(_)
        ));

        // A read-only peer attempting to write is refused.
        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "sneaky");
        let err = feed_as(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(edit.full_update())),
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WsError::Forbidden(_)));
    }

    /// Read a handler's current document text by running a fresh SyncStep1
    /// against it (the same way a brand-new client would catch up).
    async fn handler_text(server: &CollabSync, topic: &Topic, field: &str) -> String {
        let probe = CollabDocument::new();
        let reaction = feed(
            server,
            topic,
            CollabMessage::SyncStep1(Bytes::from(probe.state_vector())),
        )
        .await;
        if let CollabMessage::SyncStep2(update) = decode(&reaction.replies()[0]) {
            probe.apply_update(&update).unwrap();
        }
        probe.text(field)
    }

    #[tokio::test]
    async fn peer_updates_converge_through_a_shared_fanout() {
        // Two handlers sharing ONE fan-out simulate two web processes on one
        // Redis bus (the gateway publishes each Reaction.broadcast to it).
        let fanout: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
        let process_a = CollabSync::new(fanout.clone());
        let process_b = CollabSync::new(fanout.clone());
        let topic = Topic::new("collab:doc").unwrap();

        // A client edits on process A; the gateway publishes A's broadcast.
        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "from-A");
        let reaction = feed(
            &process_a,
            &topic,
            CollabMessage::Update(Bytes::from(edit.full_update())),
        )
        .await;
        for payload in reaction.broadcasts() {
            fanout.publish(&topic, payload.clone()).await.unwrap();
        }

        // Process B never saw a client for this topic, yet its apply loop folds
        // A's edit in. Poll until converged (bounded; in-process is near-instant).
        let mut converged = false;
        for _ in 0..200 {
            if handler_text(&process_b, &topic, "body")
                .await
                .contains("from-A")
            {
                converged = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            converged,
            "process B should converge to A's edit via the shared fan-out"
        );
    }

    #[tokio::test]
    async fn checkpoints_to_the_store_and_a_fresh_process_reloads_it() {
        use super::super::store::{CollabStore, MemoryCollabStore};

        let store: Arc<dyn CollabStore> = Arc::new(MemoryCollabStore::new());
        let topic = Topic::new("collab:doc").unwrap();

        // Process 1 checkpoints on every folded update.
        let fanout1: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
        let p1 = CollabSync::new(fanout1.clone())
            .with_store(store.clone())
            .with_checkpoint_every(1);

        // An edit on p1; publish the broadcast so p1's apply loop folds + saves.
        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "durable");
        let reaction = feed(
            &p1,
            &topic,
            CollabMessage::Update(Bytes::from(edit.full_update())),
        )
        .await;
        for payload in reaction.broadcasts() {
            fanout1.publish(&topic, payload.clone()).await.unwrap();
        }
        let mut saved = false;
        for _ in 0..200 {
            if store.load_snapshot(topic.as_str()).await.unwrap().is_some() {
                saved = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(saved, "the apply loop should checkpoint a snapshot");

        // Process 2 starts on a DIFFERENT (empty) fan-out — a restart — and must
        // recover the document from the durable store, not the stream.
        let fanout2: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
        let p2 = CollabSync::new(fanout2).with_store(store.clone());
        let mut reloaded = false;
        for _ in 0..200 {
            if handler_text(&p2, &topic, "body").await.contains("durable") {
                reloaded = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            reloaded,
            "a fresh process should reload the document from the store"
        );
    }

    #[tokio::test]
    async fn idle_eviction_frees_the_topic_then_reactivation_reloads_it() {
        use super::super::store::{CollabStore, MemoryCollabStore};

        let store: Arc<dyn CollabStore> = Arc::new(MemoryCollabStore::new());
        let fanout: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
        let server = CollabSync::new(fanout.clone())
            .with_store(store.clone())
            .with_checkpoint_every(1);
        let topic = Topic::new("collab:doc").unwrap();

        // First subscriber activates the topic; an edit is folded + checkpointed.
        server.on_topic_active(&topic);
        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "kept");
        let reaction = feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(edit.full_update())),
        )
        .await;
        for payload in reaction.broadcasts() {
            fanout.publish(&topic, payload.clone()).await.unwrap();
        }
        for _ in 0..200 {
            if store.load_snapshot(topic.as_str()).await.unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Last subscriber leaves: the topic's document + apply loop are released.
        server.on_topic_idle(&topic);
        assert!(
            server.topics.lock().unwrap().get(&topic).is_none(),
            "an idle topic must be freed"
        );

        // A new subscriber reactivates it; the document comes back from the store.
        server.on_topic_active(&topic);
        let mut recovered = false;
        for _ in 0..200 {
            if handler_text(&server, &topic, "body").await.contains("kept") {
                recovered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(recovered, "reactivating an evicted topic should recover it");
    }
}
