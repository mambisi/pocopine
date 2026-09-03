//! The server-side collab handler: a [`pocopine_realtime::SubprotocolHandler`]
//! that runs the Yjs sync handshake over the gateway (RFC 073 Part II).
//!
//! [`CollabSync`] holds one authoritative [`CollabDocument`] per topic and
//! reacts to each inbound collab message:
//!
//! - **Hello** (protocol/schema identity + state vector) → validate before any
//!   yrs exchange, then reply with the server hello followed by the diff the
//!   peer is missing (SyncStep2). A writer answers the server hello with what
//!   the server lacks.
//! - **SyncStep2 / Update** (state the server was missing / a live edit) → apply
//!   it to the authoritative document and broadcast it as an Update so every
//!   other subscriber — including peers on other processes — converges.
//!
//! Because CRDT merges are commutative and idempotent, the unordered
//! reply-vs-broadcast delivery the gateway provides is safe, and a client
//! applying its own echoed Update is a no-op.
//!
//! Write access is enforced via the gateway's write policy
//! ([`InboundData::can_write`]): a read-only connection may negotiate and sync
//! down but its Update / SyncStep2 messages are refused, and the server hello
//! does not request an upload. This is the realtime-layer counterpart
//! of [`CollabAccess`](crate::doc::CollabAccess).
//!
//! Every process **self-subscribes** to each topic's fan-out and folds peer
//! updates into its local document, so replicas behind a multi-process fan-out
//! (Redis) converge — not just the clients of one process. Durable load and
//! checkpointing run through [`CollabStore`](super::store::CollabStore).
//!
//! ## Durability window (at-least-once *through the client*)
//!
//! [`on_data`](CollabSync::on_data) applies an inbound edit to the in-memory
//! document and RETURNS a broadcast; the gateway then publishes that broadcast
//! to the fan-out (and so, durably, toward the store). If the process crashes in
//! that window — after the optimistic apply, before the gateway publishes — the
//! edit reached neither the fan-out nor the store. It is not lost outright: the
//! originating client re-runs the sync handshake on reconnect (its Hello
//! re-uploads exactly what the server lacks), so the edit returns as long as
//! that client reconnects. The server is thus at-least-once *through the client*,
//! not server-durable the instant it acks; the gap is only real if the client
//! also disappears in that window. Closing it fully would have the handler
//! publish to the fan-out itself before acking (a realtime↔collab contract
//! change deferred as its own piece).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::Instrument as _;

use async_trait::async_trait;
use bytes::Bytes;
use pocopine_events::Topic;
use pocopine_observe::LOG_TARGET;
use pocopine_realtime::{Fanout, InboundData, Reaction, SubprotocolHandler, TopicStream, WsError};
use tokio::task::JoinHandle;

use super::store::{CollabSnapshot, CollabStore};
use crate::compatibility::CompatibilityIdentity;
use crate::protocol::{CollabHello, CollabMessage, TAG_HELLO};
use crate::sync::CollabDocument;

/// Save a checkpoint snapshot once this many fan-out updates have been folded
/// in. Far below the fan-out's retention window, so the snapshot cursor always
/// stays replayable and the durable base never lags into an eviction gap.
const CHECKPOINT_EVERY: u64 = 64;

/// Bound on a single inbound document update / catch-up (RFC 073 §12: "Update
/// size caps are mandatory"). The realtime gateway also caps the whole frame,
/// but the collab handler enforces its own ceiling so the guarantee does not
/// depend on transport configuration. Generous enough for a large paste; small
/// enough that a hostile peer cannot force an unbounded `yrs` allocation.
const MAX_UPDATE_BYTES: usize = 8 * 1024 * 1024;

/// Cap on how long a single checkpoint may block the convergence apply loop. A
/// slow or stalled store must not wedge folding indefinitely: on timeout the
/// checkpoint is abandoned (the next batch retries; the store's monotonic guard
/// makes a later, fresher checkpoint correct regardless).
const CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(5);

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
    compatibility: CompatibilityIdentity,
    store: Option<Arc<dyn CollabStore>>,
    checkpoint_every: u64,
    max_update_bytes: usize,
    topics: Mutex<HashMap<Topic, Arc<TopicState>>>,
    verified_subscriptions: Mutex<HashSet<SubscriptionKey>>,
}

/// One socket's compatibility negotiation for one subscribed document.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SubscriptionKey {
    session_id: String,
    topic: Topic,
}

impl SubscriptionKey {
    fn new(session_id: &str, topic: &Topic) -> Self {
        Self {
            session_id: session_id.to_owned(),
            topic: topic.clone(),
        }
    }
}

/// Per-topic state: the document and the apply loop keeping it converged.
struct TopicState {
    doc: Arc<Mutex<CollabDocument>>,
    /// The furthest fan-out cursor the apply loop has folded into `doc`,
    /// published by the loop. Read by [`CollabSync::on_topic_idle`] to flush a
    /// final checkpoint at the right cursor before the loop is torn down, so an
    /// idle eviction never strands updates that were folded since the last
    /// periodic checkpoint.
    last_folded: Arc<AtomicU64>,
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
    pub fn new(fanout: Arc<dyn Fanout>, compatibility: CompatibilityIdentity) -> Self {
        Self {
            fanout,
            compatibility,
            store: None,
            checkpoint_every: CHECKPOINT_EVERY,
            max_update_bytes: MAX_UPDATE_BYTES,
            topics: Mutex::new(HashMap::new()),
            verified_subscriptions: Mutex::new(HashSet::new()),
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

    /// Cap a single inbound document update / catch-up at `n` bytes (default
    /// [`MAX_UPDATE_BYTES`]). A larger frame is refused before it reaches `yrs`.
    pub fn with_max_update_bytes(mut self, n: usize) -> Self {
        self.max_update_bytes = n.max(1);
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
        let last_folded = Arc::new(AtomicU64::new(0));
        let apply_loop = tokio::spawn(run_apply_loop(
            self.fanout.clone(),
            self.store.clone(),
            self.checkpoint_every,
            topic.clone(),
            doc.clone(),
            last_folded.clone(),
        ));
        let state = Arc::new(TopicState {
            doc,
            last_folded,
            apply_loop,
        });
        topics.insert(topic.clone(), state.clone());
        Ok(state)
    }

    fn ensure_compatible_topic(&self, topic: &Topic) -> Result<(), WsError> {
        if self.compatibility.accepts_topic(topic.as_str()) {
            Ok(())
        } else {
            Err(WsError::protocol(format!(
                "collab topic `{topic}` is outside server compatibility namespace v{}:{}",
                self.compatibility.protocol_version(),
                self.compatibility.fingerprint()
            )))
        }
    }

    fn revoke_subscription(&self, session_id: &str, topic: &Topic) -> Result<(), WsError> {
        self.verified_subscriptions
            .lock()
            .map_err(|_| WsError::backend("collab compatibility map poisoned"))?
            .remove(&SubscriptionKey::new(session_id, topic));
        Ok(())
    }

    fn verify_subscription(&self, session_id: &str, topic: &Topic) -> Result<(), WsError> {
        self.verified_subscriptions
            .lock()
            .map_err(|_| WsError::backend("collab compatibility map poisoned"))?
            .insert(SubscriptionKey::new(session_id, topic));
        Ok(())
    }

    fn ensure_verified(&self, session_id: &str, topic: &Topic) -> Result<(), WsError> {
        let verified = self
            .verified_subscriptions
            .lock()
            .map_err(|_| WsError::backend("collab compatibility map poisoned"))?
            .contains(&SubscriptionKey::new(session_id, topic));
        if verified {
            Ok(())
        } else {
            Err(WsError::protocol(
                "collab message received before a compatible hello",
            ))
        }
    }
}

#[async_trait]
impl SubprotocolHandler for CollabSync {
    fn outbound_starts_paused(&self) -> bool {
        true
    }

    async fn on_data(&self, inbound: InboundData<'_>) -> Result<Reaction, WsError> {
        self.ensure_compatible_topic(inbound.topic)?;

        let message = match CollabMessage::decode(inbound.payload) {
            Ok(message) => message,
            Err(err) => {
                // A malformed repeated hello must not leave an earlier verified
                // state live. Fresh malformed hellos are already absent.
                if inbound.payload.first().copied() == Some(TAG_HELLO) {
                    inbound.outbound_gate.close();
                    self.revoke_subscription(inbound.session_id, inbound.topic)?;
                }
                return Err(WsError::protocol(err.to_string()));
            }
        };

        if let CollabMessage::Hello(hello) = message {
            // A repeated hello starts a fresh negotiation. Revoke first so any
            // mismatch or malformed yrs state vector fails closed.
            inbound.outbound_gate.close();
            self.revoke_subscription(inbound.session_id, inbound.topic)?;
            if hello.compatibility() != &self.compatibility {
                return Err(WsError::protocol(format!(
                    "collab compatibility mismatch: peer v{}:{}, server v{}:{}",
                    hello.compatibility().protocol_version(),
                    hello.compatibility().fingerprint(),
                    self.compatibility.protocol_version(),
                    self.compatibility.fingerprint()
                )));
            }

            let state = self.topic_state(inbound.topic)?;
            let doc = state
                .doc
                .lock()
                .map_err(|_| WsError::backend("collab document mutex poisoned"))?;

            // Decoding the state vector is part of accepting the hello even when
            // the peer did not request a catch-up. No verified state is recorded
            // until every compatibility/yrs check above has succeeded.
            let diff = doc
                .diff(hello.state_vector())
                .map_err(|err| WsError::protocol(err.to_string()))?;
            let server_hello = CollabHello::new(
                self.compatibility.clone(),
                Bytes::from(doc.state_vector()),
                inbound.can_write,
            );

            let mut reaction = Reaction::new();
            // The peer sees and validates this identity before the following
            // SyncStep2 can be delivered/applied.
            reaction.reply(CollabMessage::Hello(server_hello).encode());
            if hello.requests_sync_step2() {
                reaction.reply(CollabMessage::SyncStep2(Bytes::from(diff)).encode());
            }
            reaction.open_outbound();
            self.verify_subscription(inbound.session_id, inbound.topic)?;
            return Ok(reaction);
        }

        // Every other message is invalid until this exact session+topic has
        // completed a compatible hello. This includes awareness: otherwise a
        // pre-hello frame would still cross schema/protocol room boundaries.
        self.ensure_verified(inbound.session_id, inbound.topic)?;

        // Awareness never touches the document and remains available to a
        // verified read-only peer.
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
            CollabMessage::SyncStep2(update) => {
                ensure_writable(&inbound)?;
                ensure_within_cap(&update, self.max_update_bytes)?;
                apply_update(&doc, &update)?;
                reaction.broadcast(CollabMessage::Update(update).encode());
            }
            CollabMessage::Update(update) => {
                ensure_writable(&inbound)?;
                ensure_within_cap(&update, self.max_update_bytes)?;
                apply_update(&doc, &update)?;
                reaction.broadcast(inbound.payload.clone());
            }
            CollabMessage::Hello(_) => unreachable!("hello returned before update matching"),
            CollabMessage::Awareness(_) => unreachable!("awareness returned before doc lock"),
        }
        Ok(reaction)
    }

    /// First local subscriber: start the topic's convergence apply loop so this
    /// process folds in peer edits even before any local client writes.
    fn on_topic_active(&self, topic: &Topic) {
        // Creating the state spawns the apply loop. Errors only on a poisoned
        // lock, where the next `on_data` will surface it.
        if self.compatibility.accepts_topic(topic.as_str()) {
            let _ = self.topic_state(topic);
        }
    }

    /// Last local subscriber left: free the topic's document and stop its apply
    /// loop. State is durable (checkpointed to the store) and reloads on the
    /// next subscriber, so this is resource reclamation — but it must not strand
    /// updates folded since the last periodic checkpoint, so we flush a final
    /// checkpoint first.
    fn on_topic_idle(&self, topic: &Topic) {
        if let Ok(mut verified) = self.verified_subscriptions.lock() {
            verified.retain(|key| &key.topic != topic);
        }
        let evicted = self
            .topics
            .lock()
            .ok()
            .and_then(|mut topics| topics.remove(topic));
        let Some(state) = evicted else { return };

        // Flush a final checkpoint at the furthest folded cursor BEFORE aborting
        // the loop. The detached task holds its own `doc`/`store`/`fanout`
        // clones, so the document outlives the (about-to-be-aborted) loop until
        // the save completes — closing the window where an idle eviction between
        // periodic checkpoints would otherwise lose those updates if the fan-out
        // tail had aged out by the time the topic reactivated.
        if let Some(store) = self.store.clone() {
            let doc = state.doc.clone();
            let fanout = self.fanout.clone();
            let topic = topic.clone();
            let seq = state.last_folded.load(Ordering::Relaxed);
            tokio::spawn(checkpoint_task(store, fanout, topic, doc, seq));
        }
        state.apply_loop.abort();
    }

    fn on_subscription_closed(&self, session_id: &str, topic: &Topic) {
        let _ = self.revoke_subscription(session_id, topic);
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

/// Refuse an inbound update whose body exceeds `cap` bytes before it is decoded
/// or applied (RFC 073 §12: update size caps are mandatory). Enforced here, not
/// only at the transport frame boundary, so the bound holds regardless of how
/// the gateway is configured.
fn ensure_within_cap(update: &[u8], cap: usize) -> Result<(), WsError> {
    if update.len() <= cap {
        Ok(())
    } else {
        Err(WsError::protocol(format!(
            "collab update too large: {} bytes exceeds cap {cap}",
            update.len()
        )))
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
/// checkpoints the folded document back every [`CHECKPOINT_EVERY`] updates,
/// trimming the now-durable fan-out prefix after each successful save.
///
/// Recovery from a lag/gap is two-tier: first resume at the loop's OWN folded
/// cursor (cheap — no store round-trip — and correct whenever the fan-out still
/// retains everything past it); only when THAT gaps (a peer trimmed the durable
/// prefix past us) reload the durable snapshot and resume at its cursor, which
/// is gap-free by the trim invariant ([`checkpoint_and_trim`] only ever trims
/// `<= durable`). Reloading on that gap is what stops a replica that fell behind
/// from skipping an unfolded-but-trimmed range and silently diverging.
async fn run_apply_loop(
    fanout: Arc<dyn Fanout>,
    store: Option<Arc<dyn CollabStore>>,
    checkpoint_every: u64,
    topic: Topic,
    doc: Arc<Mutex<CollabDocument>>,
    last_folded: Arc<AtomicU64>,
) {
    let doc_key = topic.as_str();

    // Seed from the durable snapshot, then resume the fan-out at its cursor.
    let mut after = seed_from_snapshot(store.as_deref(), &doc, doc_key).await;
    let mut stream = match subscribe_recovering(&fanout, &topic, after).await {
        Some(stream) => stream,
        None => return,
    };

    // `highest_seq` tracks the furthest seq folded; published to `last_folded` so
    // an idle eviction can flush a final checkpoint at the right cursor. The
    // durable cursor only ever moves forward.
    let mut highest_seq = after.unwrap_or(0);
    last_folded.store(highest_seq, Ordering::Relaxed);
    // The cursor a checkpoint was last INITIATED for. Checkpoints are detached
    // (below) so a slow store never blocks folding; at most one runs at a time.
    let mut last_checkpointed = after.unwrap_or(0);
    let mut checkpoint: Option<JoinHandle<()>> = None;
    let mut folded = 0u64;
    loop {
        match stream.next().await {
            Ok(Some((seq, payload))) => {
                // RFC-123 Phase 4: the apply loop belongs to no request, so each
                // fold is its own short root span.
                let apply_span = tracing::info_span!(
                    target: pocopine_observe::TRACE_TARGET,
                    parent: None,
                    pocopine_observe::spans::COLLAB_APPLY,
                    pocopine.collab.topic = topic.as_str(),
                    pocopine.collab.seq = seq,
                );
                // Broadcasts are tagged `Update` messages; a malformed or
                // non-Update frame must never kill the convergence loop.
                apply_span.in_scope(|| {
                    if let Ok(CollabMessage::Update(update)) = CollabMessage::decode(&payload)
                        && let Ok(doc) = doc.lock()
                    {
                        let _ = doc.apply_update(&update);
                    }
                });
                highest_seq = highest_seq.max(seq);
                last_folded.store(highest_seq, Ordering::Relaxed);
                folded += 1;
                // Spawn a checkpoint without awaiting it, so the loop keeps
                // folding while the store writes. Only one at a time (skip if the
                // previous is still running) and only when there is new state to
                // save; the store's monotonic guard + CRDT idempotency make an
                // out-of-order or redundant detached save harmless. The save is
                // still bounded by CHECKPOINT_TIMEOUT inside `checkpoint_and_trim`,
                // so a wedged store frees the slot rather than blocking forever.
                if let Some(store) = &store
                    && folded >= checkpoint_every
                {
                    folded = 0;
                    let idle = checkpoint.as_ref().is_none_or(|h| h.is_finished());
                    if idle && highest_seq > last_checkpointed {
                        last_checkpointed = highest_seq;
                        let (store, fanout, topic, doc, seq) = (
                            store.clone(),
                            fanout.clone(),
                            topic.clone(),
                            doc.clone(),
                            highest_seq,
                        );
                        checkpoint = Some(tokio::spawn(checkpoint_task(
                            store, fanout, topic, doc, seq,
                        )));
                    }
                }
            }
            // Fan-out closed (topic torn down / shutdown).
            Ok(None) => return,
            // Lagged or gapped — recover (two-tier, see the fn doc).
            Err(_) => match fanout.subscribe(&topic, Some(highest_seq)).await {
                // Our own progress is still retained: resume with no reload.
                Ok(resumed) if !resumed.gap() => stream = resumed,
                // Trimmed past us (or unreachable): reload the snapshot and
                // resume at its cursor. The snapshot covers `<= after`, so
                // re-folding from there cannot lose the trimmed range.
                _ => {
                    after = seed_from_snapshot(store.as_deref(), &doc, doc_key).await;
                    highest_seq = highest_seq.max(after.unwrap_or(0));
                    last_checkpointed = last_checkpointed.max(after.unwrap_or(0));
                    last_folded.store(highest_seq, Ordering::Relaxed);
                    match subscribe_recovering(&fanout, &topic, after).await {
                        Some(resumed) => stream = resumed,
                        None => return,
                    }
                }
            },
        }
    }
}

/// Seed `doc` from the durable snapshot (if a store is configured), returning
/// the fan-out cursor to resume at. Idempotent: applying the snapshot to a
/// document already ahead of it is a CRDT no-op, so it is safe to call again on
/// recovery.
async fn seed_from_snapshot(
    store: Option<&dyn CollabStore>,
    doc: &Arc<Mutex<CollabDocument>>,
    doc_key: &str,
) -> Option<u64> {
    let store = store?;
    match store.load_snapshot(doc_key).await {
        Ok(Some(snapshot)) => {
            if let Ok(doc) = doc.lock() {
                let _ = doc.apply_update(&snapshot.blob);
            }
            Some(snapshot.last_seq)
        }
        Ok(None) => None,
        Err(err) => {
            tracing::warn!(target: LOG_TARGET, error = %err, topic = doc_key, "collab apply loop: load_snapshot failed");
            None
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

/// Persist the folded document as the new durable base current to `last_seq`,
/// then — only on a confirmed save — release the now-durable fan-out prefix.
/// Returns whether the save succeeded, so the caller advances its checkpoint
/// cursor (and trims) only when the durable base actually covers `last_seq`.
///
/// The document lock is never held across an `.await`. The save is bounded by
/// [`CHECKPOINT_TIMEOUT`]: a slow store degrades to "checkpoint skipped, retry
/// next batch" instead of stalling convergence, and the store's monotonic guard
/// makes a later fresher checkpoint correct regardless.
/// A detached checkpoint under its own root span (RFC-123 Phase 4): it
/// belongs to no request, and the span closes as OK or ERROR from the
/// store's answer.
async fn checkpoint_task(
    store: Arc<dyn CollabStore>,
    fanout: Arc<dyn Fanout>,
    topic: Topic,
    doc: Arc<Mutex<CollabDocument>>,
    seq: u64,
) {
    let span = tracing::info_span!(
        target: pocopine_observe::TRACE_TARGET,
        parent: None,
        pocopine_observe::spans::COLLAB_CHECKPOINT,
        pocopine.collab.topic = topic.as_str(),
        pocopine.collab.seq = seq,
        otel.status_code = tracing::field::Empty,
    );
    let saved = checkpoint_and_trim(store.as_ref(), &fanout, &topic, &doc, seq)
        .instrument(span.clone())
        .await;
    span.record(
        pocopine_observe::fields::OTEL_STATUS_CODE,
        if saved { "OK" } else { "ERROR" },
    );
}

async fn checkpoint_and_trim(
    store: &dyn CollabStore,
    fanout: &Arc<dyn Fanout>,
    topic: &Topic,
    doc: &Arc<Mutex<CollabDocument>>,
    last_seq: u64,
) -> bool {
    let doc_key = topic.as_str();
    let snapshot = {
        let Ok(doc) = doc.lock() else { return false };
        CollabSnapshot {
            blob: Bytes::from(doc.full_update()),
            state_vector: Bytes::from(doc.state_vector()),
            last_seq,
        }
    };
    let saved = match tokio::time::timeout(
        CHECKPOINT_TIMEOUT,
        store.save_snapshot(doc_key, snapshot),
    )
    .await
    {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            tracing::warn!(target: LOG_TARGET, error = %err, topic = doc_key, "collab apply loop: save_snapshot failed");
            false
        }
        Err(_) => {
            tracing::warn!(target: LOG_TARGET, topic = doc_key, "collab apply loop: save_snapshot timed out");
            false
        }
    };

    // After a successful save_snapshot, everything `<= last_seq` is durable —
    // whether the store wrote our snapshot or skipped it for a fresher one (its
    // cursor is then `>= last_seq`). Either way the fan-out no longer needs to
    // retain `<= last_seq` for crash recovery, so release it (a no-op on a
    // non-durable in-process fan-out).
    if saved && let Err(err) = fanout.trim_after(topic, last_seq).await {
        tracing::warn!(target: LOG_TARGET, error = %err, topic = doc_key, "collab apply loop: trim_after failed");
    }
    saved
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pocopine_realtime::LocalFanout;

    use super::*;

    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn compatibility() -> CompatibilityIdentity {
        CompatibilityIdentity::new(1, FINGERPRINT).unwrap()
    }

    fn topic(document_key: &str) -> Topic {
        Topic::new(compatibility().namespace_topic(document_key)).unwrap()
    }

    /// A handler over a fresh in-process fan-out (no peers).
    fn sync() -> CollabSync {
        CollabSync::new(Arc::new(LocalFanout::new()), compatibility())
    }

    /// Decode one collab message from raw frame bytes.
    fn decode(bytes: &Bytes) -> CollabMessage {
        CollabMessage::decode(bytes).expect("decode collab message")
    }

    fn hello(doc: &CollabDocument, request_sync_step2: bool) -> CollabMessage {
        CollabMessage::Hello(CollabHello::new(
            compatibility(),
            doc.state_vector(),
            request_sync_step2,
        ))
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
        if !matches!(&message, CollabMessage::Hello(_)) {
            let opener = CollabDocument::new();
            raw_feed_as(
                server,
                topic,
                CollabMessage::Hello(CollabHello::new(
                    compatibility(),
                    opener.state_vector(),
                    true,
                )),
                can_write,
                "test-session",
            )
            .await?;
        }
        raw_feed_as(server, topic, message, can_write, "test-session").await
    }

    async fn raw_feed_as(
        server: &CollabSync,
        topic: &Topic,
        message: CollabMessage,
        can_write: bool,
        session_id: &str,
    ) -> Result<Reaction, WsError> {
        let payload = message.encode();
        let principal = pocopine_realtime::Principal::anonymous();
        let outbound_gate = pocopine_realtime::OutboundGate::new(false);
        server
            .on_data(InboundData {
                session_id,
                outbound_gate: &outbound_gate,
                topic,
                payload: &payload,
                can_write,
                principal: &principal,
            })
            .await
    }

    #[tokio::test]
    async fn an_update_is_applied_and_broadcast() {
        let server = sync();
        let topic = topic("doc");

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
        let topic = topic("doc");

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
        let topic = topic("doc");

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
        let topic = topic("doc");

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
        let topic = topic("doc");

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
        let topic = topic("doc");

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

        // Client hello; server answers with its hello first, then SyncStep2.
        let reaction = feed(&server, &topic, hello(&client, true)).await;
        assert!(reaction.broadcasts().is_empty());
        assert_eq!(reaction.replies().len(), 2);

        // Apply the server's catch-up; the client now holds the shared state.
        let catch_up = match decode(&reaction.replies()[1]) {
            CollabMessage::SyncStep2(update) => update,
            other => panic!("expected SyncStep2, got {other:?}"),
        };
        let server_sv = match decode(&reaction.replies()[0]) {
            CollabMessage::Hello(hello) => hello.state_vector().clone(),
            other => panic!("expected Hello, got {other:?}"),
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
        let reaction = feed(&server, &topic, hello(&fresh, true)).await;
        let full = match decode(&reaction.replies()[1]) {
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
        let a = topic("a");
        let b = topic("b");

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
        let reaction = feed(&server, &b, hello(&fresh, true)).await;
        let full = match decode(&reaction.replies()[1]) {
            CollabMessage::SyncStep2(update) => update,
            other => panic!("expected SyncStep2, got {other:?}"),
        };
        fresh.apply_update(&full).unwrap();
        assert_eq!(fresh.text("body"), "");
    }

    #[tokio::test]
    async fn rejects_a_malformed_payload() {
        let server = sync();
        let topic = topic("doc");
        let empty = Bytes::new();
        let principal = pocopine_realtime::Principal::anonymous();
        let outbound_gate = pocopine_realtime::OutboundGate::new(false);
        let err = server
            .on_data(InboundData {
                session_id: "malformed-session",
                outbound_gate: &outbound_gate,
                topic: &topic,
                payload: &empty,
                can_write: true,
                principal: &principal,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, WsError::Protocol(_)));
    }

    #[tokio::test]
    async fn mismatched_and_pre_hello_peers_exchange_no_yrs_updates() {
        let server = sync();
        let topic = topic("doc");

        let peer = CollabDocument::new();
        peer.insert_text("body", 0, "must-not-apply");
        let update = Bytes::from(peer.full_update());

        // Neither live updates nor handshake diffs are accepted before hello.
        for message in [
            CollabMessage::Update(update.clone()),
            CollabMessage::SyncStep2(update.clone()),
        ] {
            let err = raw_feed_as(&server, &topic, message, true, "peer-a")
                .await
                .unwrap_err();
            assert!(matches!(err, WsError::Protocol(_)));
        }

        let mismatch = CompatibilityIdentity::new(
            compatibility().protocol_version(),
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .unwrap();
        let err = raw_feed_as(
            &server,
            &topic,
            CollabMessage::Hello(CollabHello::new(mismatch, peer.state_vector(), true)),
            true,
            "peer-a",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WsError::Protocol(_)));

        // The rejected hello did not authorize a later valid yrs update.
        let err = raw_feed_as(
            &server,
            &topic,
            CollabMessage::Update(update.clone()),
            true,
            "peer-a",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WsError::Protocol(_)));

        // A compatible probe sees an empty authoritative document: none of the
        // valid yrs bytes above crossed the compatibility boundary.
        let probe = CollabDocument::new();
        let reaction = raw_feed_as(&server, &topic, hello(&probe, true), true, "probe")
            .await
            .unwrap();
        assert_eq!(reaction.replies().len(), 2);
        let CollabMessage::SyncStep2(catch_up) = decode(&reaction.replies()[1]) else {
            panic!("compatible hello must receive catch-up")
        };
        probe.apply_update(&catch_up).unwrap();
        assert_eq!(probe.text("body"), "");
    }

    #[tokio::test]
    async fn hello_is_scoped_to_the_exact_session_and_subscription() {
        let server = sync();
        let topic = topic("doc");
        let opener = CollabDocument::new();
        raw_feed_as(&server, &topic, hello(&opener, true), true, "peer-a")
            .await
            .unwrap();

        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "other socket");
        let err = raw_feed_as(
            &server,
            &topic,
            CollabMessage::Update(edit.full_update().into()),
            true,
            "peer-b",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WsError::Protocol(_)));

        server.on_subscription_closed("peer-a", &topic);
        let err = raw_feed_as(
            &server,
            &topic,
            CollabMessage::Update(edit.full_update().into()),
            true,
            "peer-a",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WsError::Protocol(_)));
    }

    #[tokio::test]
    async fn read_only_connection_can_sync_down_but_not_write() {
        let server = sync();
        let topic = topic("doc");

        // Seed the server with some state (via a writer).
        let seed = CollabDocument::new();
        seed.insert_text("body", 0, "shared");
        feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(seed.full_update())),
        )
        .await;

        // A read-only peer catches up, but the server hello does not invite a
        // SyncStep2 upload.
        let viewer = CollabDocument::new();
        let reaction = feed_as(&server, &topic, hello(&viewer, true), false)
            .await
            .expect("read is allowed");
        assert_eq!(reaction.replies().len(), 2, "hello then catch-up SyncStep2");
        assert!(matches!(
            decode(&reaction.replies()[1]),
            CollabMessage::SyncStep2(_)
        ));
        let CollabMessage::Hello(server_hello) = decode(&reaction.replies()[0]) else {
            panic!("server must identify itself before catch-up")
        };
        assert!(!server_hello.requests_sync_step2());

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

    /// Read a handler's current document text by running a fresh hello
    /// against it (the same way a brand-new client would catch up).
    async fn handler_text(server: &CollabSync, topic: &Topic, field: &str) -> String {
        let probe = CollabDocument::new();
        let reaction = feed(server, topic, hello(&probe, true)).await;
        if let CollabMessage::SyncStep2(update) = decode(&reaction.replies()[1]) {
            probe.apply_update(&update).unwrap();
        }
        probe.text(field)
    }

    #[tokio::test]
    async fn peer_updates_converge_through_a_shared_fanout() {
        // Two handlers sharing ONE fan-out simulate two web processes on one
        // Redis bus (the gateway publishes each Reaction.broadcast to it).
        let fanout: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
        let process_a = CollabSync::new(fanout.clone(), compatibility());
        let process_b = CollabSync::new(fanout.clone(), compatibility());
        let topic = topic("doc");

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
        let topic = topic("doc");

        // Process 1 checkpoints on every folded update.
        let fanout1: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
        let p1 = CollabSync::new(fanout1.clone(), compatibility())
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
        let p2 = CollabSync::new(fanout2, compatibility()).with_store(store.clone());
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
        let server = CollabSync::new(fanout.clone(), compatibility())
            .with_store(store.clone())
            .with_checkpoint_every(1);
        let topic = topic("doc");

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

    #[tokio::test]
    async fn an_oversized_update_is_refused_before_apply() {
        // RFC 073 §12: update size caps are mandatory, enforced in the handler
        // (not only at the transport frame boundary) and BEFORE decode/apply.
        let server = CollabSync::new(Arc::new(LocalFanout::new()), compatibility())
            .with_max_update_bytes(16);
        let topic = topic("doc");

        let oversized = CollabMessage::Update(Bytes::from(vec![0u8; 64]));
        let err = feed_as(&server, &topic, oversized, true).await.unwrap_err();
        assert!(
            matches!(err, WsError::Protocol(_)),
            "an update past the cap must be refused, got {err:?}"
        );

        // The same cap applies to a handshake SyncStep2.
        let oversized_step2 = CollabMessage::SyncStep2(Bytes::from(vec![0u8; 64]));
        let err = feed_as(&server, &topic, oversized_step2, true)
            .await
            .unwrap_err();
        assert!(matches!(err, WsError::Protocol(_)));
    }

    /// A [`Fanout`] that delegates to an inner [`LocalFanout`] but records every
    /// `trim_after` cursor, so a test can assert a checkpoint released the
    /// fan-out prefix.
    struct RecordingFanout {
        inner: LocalFanout,
        trims: Arc<Mutex<Vec<u64>>>,
    }

    #[async_trait]
    impl Fanout for RecordingFanout {
        async fn publish(&self, topic: &Topic, payload: Bytes) -> Result<u64, WsError> {
            self.inner.publish(topic, payload).await
        }
        async fn subscribe(
            &self,
            topic: &Topic,
            after: Option<u64>,
        ) -> Result<TopicStream, WsError> {
            self.inner.subscribe(topic, after).await
        }
        async fn trim_after(&self, topic: &Topic, durable_seq: u64) -> Result<(), WsError> {
            self.trims.lock().unwrap().push(durable_seq);
            self.inner.trim_after(topic, durable_seq).await
        }
    }

    #[tokio::test]
    async fn a_successful_checkpoint_trims_the_fanout() {
        use super::super::store::MemoryCollabStore;

        // C3: "trim ... only after durable save." A folded + checkpointed update
        // releases the now-durable fan-out prefix via `trim_after`.
        let trims = Arc::new(Mutex::new(Vec::new()));
        let fanout: Arc<dyn Fanout> = Arc::new(RecordingFanout {
            inner: LocalFanout::new(),
            trims: trims.clone(),
        });
        let store: Arc<dyn CollabStore> = Arc::new(MemoryCollabStore::new());
        let server = CollabSync::new(fanout.clone(), compatibility())
            .with_store(store)
            .with_checkpoint_every(1);
        let topic = topic("doc");
        server.on_topic_active(&topic);

        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "x");
        let reaction = feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(edit.full_update())),
        )
        .await;
        for payload in reaction.broadcasts() {
            fanout.publish(&topic, payload.clone()).await.unwrap();
        }

        let mut trimmed = false;
        for _ in 0..200 {
            if !trims.lock().unwrap().is_empty() {
                trimmed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(trimmed, "a successful checkpoint must trim the fan-out");
        assert!(
            trims.lock().unwrap().iter().copied().max().unwrap_or(0) >= 1,
            "trim cursor should be the folded seq"
        );
    }

    #[tokio::test]
    async fn idle_eviction_flushes_a_final_checkpoint() {
        use super::super::store::{CollabStore, MemoryCollabStore};

        // Cadence set so high the periodic checkpoint NEVER fires for one edit:
        // only the idle flush can persist it. Guards the abort-mid-checkpoint
        // data-loss path — an idle eviction must not strand folded updates.
        let store: Arc<dyn CollabStore> = Arc::new(MemoryCollabStore::new());
        let fanout: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
        let server = CollabSync::new(fanout.clone(), compatibility())
            .with_store(store.clone())
            .with_checkpoint_every(1_000_000);
        let topic = topic("doc");
        server.on_topic_active(&topic);

        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "only-in-memory");
        let reaction = feed(
            &server,
            &topic,
            CollabMessage::Update(Bytes::from(edit.full_update())),
        )
        .await;
        for payload in reaction.broadcasts() {
            fanout.publish(&topic, payload.clone()).await.unwrap();
        }

        // Nothing is persisted yet — the cadence is far out of reach.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            store.load_snapshot(topic.as_str()).await.unwrap().is_none(),
            "the periodic checkpoint must not fire at this cadence"
        );

        // Idle eviction flushes a final checkpoint of the current document.
        server.on_topic_idle(&topic);
        let mut saved = false;
        for _ in 0..200 {
            if store.load_snapshot(topic.as_str()).await.unwrap().is_some() {
                saved = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(saved, "idle eviction must flush a final checkpoint");

        // A fresh process reloads exactly that flushed state.
        let fanout2: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
        let p2 = CollabSync::new(fanout2, compatibility()).with_store(store.clone());
        let mut reloaded = false;
        for _ in 0..200 {
            if handler_text(&p2, &topic, "body")
                .await
                .contains("only-in-memory")
            {
                reloaded = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(reloaded, "the final-checkpoint state must reload");
    }
}
