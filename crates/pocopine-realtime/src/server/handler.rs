//! Per-sub-protocol inbound handlers (RFC 073 §7, the stateful-server seam).
//!
//! By default a Data frame is a pure relay: the gateway re-checks write
//! authorization and publishes the payload to the topic's [`Fanout`], so every
//! subscriber (including other processes) receives it verbatim. That is all a
//! presence/broadcast sub-protocol needs.
//!
//! A CRDT sub-protocol needs more: the *server* must answer a peer's
//! `SyncStep1` with a computed diff (a reply to that one connection), while
//! document updates still broadcast to everyone. Registering a
//! [`SubprotocolHandler`] on the gateway for a `subprotocol_id` replaces the
//! relay for that sub-protocol with stateful server logic. The handler reacts
//! to each inbound Data frame with a [`Reaction`]: `reply` bytes go back to the
//! originating connection only, `broadcast` bytes go through the fan-out to
//! every subscriber. `pocopine-collab` is the canonical implementor.

use async_trait::async_trait;
use bytes::Bytes;
use pocopine_events::Topic;

use super::error::WsError;

/// One inbound Data frame, handed to a [`SubprotocolHandler`].
///
/// The connection has already passed the gateway's topic (join) authorization,
/// so the handler only sees frames on topics it may read. The payload is the
/// opaque sub-protocol body (the frame envelope has been stripped).
pub struct InboundData<'a> {
    /// The resolved topic the frame targets.
    pub topic: &'a Topic,
    /// The opaque sub-protocol payload (the Data frame body).
    pub payload: &'a Bytes,
    /// Whether this connection is authorized to *publish* on the topic
    /// (the gateway's write policy; defaults to allow). A handler that
    /// distinguishes read-only from read-write access (e.g. collab) gates
    /// its mutating messages on this; pure relays ignore it.
    pub can_write: bool,
}

/// What the gateway should emit after a [`SubprotocolHandler`] processes a
/// frame: direct replies to the originating connection and/or broadcasts to the
/// whole topic.
///
/// Replies and broadcasts are independent: a CRDT `SyncStep1` produces only
/// replies (the catch-up diff), a document update produces only a broadcast,
/// and the server's own `SyncStep1` rides along as a reply during the initial
/// handshake. Order between a reply and a concurrently-pumped broadcast is not
/// guaranteed — CRDT merges are commutative and idempotent, so it does not need
/// to be.
#[derive(Debug, Default)]
pub struct Reaction {
    pub(crate) replies: Vec<Bytes>,
    pub(crate) broadcasts: Vec<Bytes>,
}

impl Reaction {
    /// An empty reaction (no replies, no broadcasts).
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `payload` as a Data frame sent back to the originating connection
    /// only (never fanned out). Used for the sync handshake.
    pub fn reply(&mut self, payload: impl Into<Bytes>) -> &mut Self {
        self.replies.push(payload.into());
        self
    }

    /// Queue `payload` to publish to the topic's fan-out, reaching every
    /// subscriber (including peers on other processes). Used for document
    /// updates that must converge everywhere.
    pub fn broadcast(&mut self, payload: impl Into<Bytes>) -> &mut Self {
        self.broadcasts.push(payload.into());
        self
    }

    /// The queued direct replies, in order (lets a handler's author assert its
    /// output in tests).
    pub fn replies(&self) -> &[Bytes] {
        &self.replies
    }

    /// The queued broadcasts, in order.
    pub fn broadcasts(&self) -> &[Bytes] {
        &self.broadcasts
    }
}

/// Server-side handler for one sub-protocol's inbound Data frames.
///
/// Registered on the gateway by `subprotocol_id` via
/// [`WsGateway::with_handler`](super::gateway::WsGateway::with_handler). When a
/// Data frame for that sub-protocol arrives, the gateway calls [`Self::on_data`]
/// *instead of* the default publish-to-fan-out relay and then executes the
/// returned [`Reaction`].
///
/// Implementors are shared across every connection and topic (held behind an
/// `Arc`), so they must be `Send + Sync` and own any per-topic state behind
/// interior synchronization.
#[async_trait]
pub trait SubprotocolHandler: Send + Sync + 'static {
    /// React to one inbound Data frame. Returning `Err` rejects the frame (the
    /// gateway logs it and sends the peer a `bad_frame` control error) without
    /// tearing down the connection.
    async fn on_data(&self, inbound: InboundData<'_>) -> Result<Reaction, WsError>;
}
