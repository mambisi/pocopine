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
//! This increment keeps documents in process memory; durable load/compaction
//! through [`CollabStore`](super::store::CollabStore) is the next step. Write
//! authorization is the gateway's per-topic policy; finer-grained
//! read-only-vs-read-write access ([`CollabAccess`](super::doc::CollabAccess))
//! is layered by a richer authorizer in a follow-up.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use pocopine_events::Topic;
use pocopine_realtime::{InboundData, Reaction, SubprotocolHandler, WsError};

use super::protocol::CollabMessage;
use super::sync::CollabDocument;

/// Server-side CRDT collaboration over the realtime gateway.
///
/// Shared across every connection and topic (held behind an `Arc` by the
/// gateway). Each topic's authoritative document lives behind its OWN `Mutex`,
/// so a slow operation on one document (e.g. encoding a large state vector)
/// never blocks edits to any other topic; the outer map lock is held only long
/// enough to look up the per-topic handle. The document operations are
/// synchronous and never cross an `.await`, so neither lock is held across a
/// suspension point.
#[derive(Default)]
pub struct CollabSync {
    docs: Mutex<HashMap<Topic, Arc<Mutex<CollabDocument>>>>,
}

impl CollabSync {
    /// A handler with no documents yet; each is created on its topic's first
    /// message.
    pub fn new() -> Self {
        Self::default()
    }

    /// The per-topic document handle, created on first access.
    fn document(&self, topic: &Topic) -> Result<Arc<Mutex<CollabDocument>>, WsError> {
        let mut docs = self
            .docs
            .lock()
            .map_err(|_| WsError::backend("collab document map poisoned"))?;
        Ok(Arc::clone(docs.entry(topic.clone()).or_insert_with(|| {
            Arc::new(Mutex::new(CollabDocument::new()))
        })))
    }
}

#[async_trait]
impl SubprotocolHandler for CollabSync {
    async fn on_data(&self, inbound: InboundData<'_>) -> Result<Reaction, WsError> {
        let message = CollabMessage::decode(inbound.payload)
            .map_err(|err| WsError::protocol(err.to_string()))?;

        let document = self.document(inbound.topic)?;
        let doc = document
            .lock()
            .map_err(|_| WsError::backend("collab document mutex poisoned"))?;

        let mut reaction = Reaction::new();
        match message {
            CollabMessage::SyncStep1(state_vector) => {
                // Reply with what this peer is missing...
                let diff = doc
                    .diff(&state_vector)
                    .map_err(|err| WsError::protocol(err.to_string()))?;
                reaction.reply(CollabMessage::SyncStep2(Bytes::from(diff)).encode());
                // ...and our own state vector, so it sends back what WE lack.
                reaction.reply(CollabMessage::SyncStep1(Bytes::from(doc.state_vector())).encode());
            }
            CollabMessage::SyncStep2(update) => {
                // Relabel a handshake SyncStep2 as a live Update for peers.
                if let Some(payload) = broadcast_if_advanced(&doc, &update, || {
                    CollabMessage::Update(update.clone()).encode()
                })? {
                    reaction.broadcast(payload);
                }
            }
            CollabMessage::Update(update) => {
                // Already a tagged Update on the wire — forward the original
                // payload verbatim (a cheap `Bytes` refcount bump, no re-encode).
                if let Some(payload) =
                    broadcast_if_advanced(&doc, &update, || inbound.payload.clone())?
                {
                    reaction.broadcast(payload);
                }
            }
        }
        Ok(reaction)
    }
}

/// Apply `update` to `doc`; return the to-broadcast payload (built lazily by
/// `payload`) only if the document actually advanced. A no-op update — a
/// duplicate, or a peer that had nothing new — is applied but NOT fanned out,
/// so it never wakes every subscriber or burns a slot of the bounded fan-out
/// replay window.
fn broadcast_if_advanced(
    doc: &CollabDocument,
    update: &[u8],
    payload: impl FnOnce() -> Bytes,
) -> Result<Option<Bytes>, WsError> {
    let before = doc.state_vector();
    doc.apply_update(update)
        .map_err(|err| WsError::protocol(err.to_string()))?;
    Ok((doc.state_vector() != before).then(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode one collab message from raw frame bytes.
    fn decode(bytes: &Bytes) -> CollabMessage {
        CollabMessage::decode(bytes).expect("decode collab message")
    }

    /// Feed one inbound collab message to the server and return its reaction.
    async fn feed(server: &CollabSync, topic: &Topic, message: CollabMessage) -> Reaction {
        let payload = message.encode();
        server
            .on_data(InboundData {
                topic,
                payload: &payload,
            })
            .await
            .expect("handler accepted the message")
    }

    #[tokio::test]
    async fn an_update_is_applied_and_broadcast() {
        let server = CollabSync::new();
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
    async fn a_noop_update_is_applied_but_not_rebroadcast() {
        let server = CollabSync::new();
        let topic = Topic::new("collab:doc").unwrap();
        let edit = CollabDocument::new();
        edit.insert_text("body", 0, "hello");
        let update = Bytes::from(edit.full_update());

        // First application advances the document and fans out.
        let first = feed(&server, &topic, CollabMessage::Update(update.clone())).await;
        assert_eq!(first.broadcasts().len(), 1);

        // Re-applying the identical update changes nothing: still applied, but
        // NOT fanned out again (no subscriber wake, no replay-window slot burnt).
        let second = feed(&server, &topic, CollabMessage::Update(update)).await;
        assert!(
            second.broadcasts().is_empty(),
            "a duplicate / no-op update must not be rebroadcast"
        );
    }

    #[tokio::test]
    async fn handshake_converges_server_and_client_both_ways() {
        let server = CollabSync::new();
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
        let server = CollabSync::new();
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
        let server = CollabSync::new();
        let topic = Topic::new("collab:doc").unwrap();
        let empty = Bytes::new();
        let err = server
            .on_data(InboundData {
                topic: &topic,
                payload: &empty,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, WsError::Protocol(_)));
    }
}
