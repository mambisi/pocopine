//! The client-side session protocol — a target-agnostic state machine driven by
//! the wasm WebSocket shell, so the protocol logic is unit-tested on the host.
//!
//! It owns no I/O: feed it inbound [`Frame`]s with [`ClientSession::on_frame`]
//! and it returns a [`SessionEvent`]; ask it for the [`Frame`]s to send with
//! [`ClientSession::subscribe`] / [`data`](ClientSession::data) /
//! [`heartbeat`](ClientSession::heartbeat). The shell does the actual send/recv.

use std::collections::HashMap;

use crate::protocol::{Control, Frame, FrameKind, ProtocolError};
use bytes::Bytes;

/// The transport-level liveness of the connection, surfaced to consumers via
/// `RealtimeClient::on_status` so a UI can show "reconnecting…". Distinct from
/// [`SessionEvent`], which reports *protocol* events on a live socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// The socket is OPEN; the session is live (or has just been re-established).
    Open,
    /// The socket dropped; a reconnect is scheduled or in flight. The transport
    /// re-subscribes every topic automatically once it reconnects.
    Reconnecting,
    /// The client was dropped / closed deliberately; no further reconnects.
    Closed,
}

/// Capped exponential backoff (milliseconds) before the `attempt`-th reconnect
/// (0-based: the first retry after a drop is `attempt == 0`).
///
/// Pure and target-agnostic so the schedule is unit-tested on the host; the
/// wasm transport drives it from a `setTimeout`.
pub fn reconnect_delay_ms(attempt: u32) -> u32 {
    /// Delay before the first retry.
    const BASE_MS: u32 = 500;
    /// Ceiling so a long outage settles to a steady retry cadence.
    const CAP_MS: u32 = 10_000;
    // `min(5)` caps the shift well below `u32` overflow; `BASE_MS << 5` = 16_000,
    // already past the cap, so every later attempt clamps to `CAP_MS`.
    BASE_MS.saturating_mul(1u32 << attempt.min(5)).min(CAP_MS)
}

/// Something the consumer should react to, produced from an inbound frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// The gateway accepted the connection (`Hello`).
    Connected {
        session_id: String,
        heartbeat_ms: u32,
    },
    /// A subscribe was acknowledged; `topic_ref` is now bound.
    Subscribed { topic: String, topic_ref: u64 },
    /// A topic was left.
    Unsubscribed { topic: String },
    /// An application Data payload arrived for `topic`.
    Data { topic: String, payload: Bytes },
    /// A subscription must be re-established fresh (unreplayable cursor / lag).
    Gap { topic: String, reason: String },
    /// A subscribe was rejected by the gateway's topic authorizer.
    SubscribeDenied { topic: String, reason: String },
    /// The server reported a typed error.
    Error { code: String, message: String },
    /// A heartbeat was acknowledged.
    HeartbeatAck,
}

/// The client end of the `pocopine.ws.v1` session.
#[derive(Default)]
pub struct ClientSession {
    session_id: Option<String>,
    heartbeat_ms: u32,
    /// topic name → assigned `topic_ref` (from the server's `SubscribeAck`).
    topic_ref: HashMap<String, u64>,
    /// `topic_ref` → topic name (to route inbound Data frames).
    topic_name: HashMap<u64, String>,
}

impl ClientSession {
    /// A fresh, not-yet-connected session.
    pub fn new() -> Self {
        Self::default()
    }

    /// The server-assigned session id, once `Hello` has arrived.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The heartbeat interval the server asked for (ms); 0 until `Hello`.
    pub fn heartbeat_interval_ms(&self) -> u32 {
        self.heartbeat_ms
    }

    /// The Subscribe frame to send to join `topic` under `subprotocol_id`.
    pub fn subscribe(&self, topic: &str, subprotocol_id: u64) -> Frame {
        Frame::subscribe(subprotocol_id, topic)
    }

    /// The Data frame to publish `payload` to `topic`, or `None` if `topic` has
    /// not been acknowledged yet (no `topic_ref` to address it by).
    pub fn data(&self, topic: &str, subprotocol_id: u64, payload: Bytes) -> Option<Frame> {
        let topic_ref = *self.topic_ref.get(topic)?;
        Some(Frame::data(subprotocol_id, topic_ref, 0, payload))
    }

    /// A liveness heartbeat frame.
    pub fn heartbeat(&self) -> Result<Frame, ProtocolError> {
        Control::Heartbeat { acks: Vec::new() }.into_frame()
    }

    /// Process one inbound frame, updating state and returning an event for the
    /// consumer (`None` for frames the client ignores).
    pub fn on_frame(&mut self, frame: &Frame) -> Result<Option<SessionEvent>, ProtocolError> {
        match frame.kind {
            FrameKind::Data => {
                Ok(self
                    .topic_name
                    .get(&frame.topic_ref)
                    .map(|topic| SessionEvent::Data {
                        topic: topic.clone(),
                        payload: frame.payload.clone(),
                    }))
            }
            FrameKind::Control => self.on_control(Control::decode(&frame.payload)?),
            // Subscribe / Unsubscribe are client→server only.
            FrameKind::Subscribe | FrameKind::Unsubscribe => Ok(None),
        }
    }

    fn on_control(&mut self, control: Control) -> Result<Option<SessionEvent>, ProtocolError> {
        let event = match control {
            Control::Hello {
                session_id,
                heartbeat_interval_ms,
                ..
            } => {
                self.session_id = Some(session_id.clone());
                self.heartbeat_ms = heartbeat_interval_ms;
                SessionEvent::Connected {
                    session_id,
                    heartbeat_ms: heartbeat_interval_ms,
                }
            }
            Control::SubscribeAck { topic, topic_ref } => {
                self.topic_ref.insert(topic.clone(), topic_ref);
                self.topic_name.insert(topic_ref, topic.clone());
                SessionEvent::Subscribed { topic, topic_ref }
            }
            // NOTE: Resume/Resumed is currently DORMANT for the wasm client. The
            // `RealtimeClient` recovers a dropped socket by re-subscribing fresh
            // (a new session + a fresh SubscribeAck per topic), never by sending
            // `Control::Resume`, so this arm isn't exercised in practice. It is
            // kept correct for a future backlog-replay relay topic — at which point
            // the server's `Resumed` must echo the new `topic_ref` (today it does
            // not; it reassigns a ref per connection), or this `keeps its prior
            // ref` assumption breaks for multi-topic resumes. Until then the Gap
            // fallback below keeps an unbound resume from mis-routing.
            Control::Resumed { topic } => match self.topic_ref.get(&topic).copied() {
                // A resumed topic keeps its prior ref; surface it as subscribed.
                Some(topic_ref) => SessionEvent::Subscribed { topic, topic_ref },
                // A resume for a topic this session never bound (lost ref state /
                // desync): force a fresh subscribe via Gap rather than fabricate
                // ref 0, which would mis-route every later frame on the topic.
                None => SessionEvent::Gap {
                    topic,
                    reason: "resume_without_binding".into(),
                },
            },
            Control::Unsubscribed { topic } => {
                if let Some(topic_ref) = self.topic_ref.remove(&topic) {
                    self.topic_name.remove(&topic_ref);
                }
                SessionEvent::Unsubscribed { topic }
            }
            Control::Gap { topic, reason } => {
                // The ref is stale; drop it so a re-subscribe rebinds.
                if let Some(topic_ref) = self.topic_ref.remove(&topic) {
                    self.topic_name.remove(&topic_ref);
                }
                SessionEvent::Gap { topic, reason }
            }
            Control::SubscribeDenied { topic, reason } => {
                if let Some(topic_ref) = self.topic_ref.remove(&topic) {
                    self.topic_name.remove(&topic_ref);
                }
                SessionEvent::SubscribeDenied { topic, reason }
            }
            Control::Error { code, message } => SessionEvent::Error { code, message },
            Control::HeartbeatAck {} => SessionEvent::HeartbeatAck,
            // Heartbeat / Resume are client→server; the client never receives them.
            Control::Heartbeat { .. } | Control::Resume { .. } => return Ok(None),
        };
        Ok(Some(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Frame {
        Control::Hello {
            session_id: "ws-7".into(),
            heartbeat_interval_ms: 15_000,
            protocol: "pocopine.ws.v1".into(),
        }
        .into_frame()
        .unwrap()
    }

    #[test]
    fn handshake_then_subscribe_then_data() {
        let mut s = ClientSession::new();

        // Hello -> Connected.
        assert_eq!(
            s.on_frame(&hello()).unwrap(),
            Some(SessionEvent::Connected {
                session_id: "ws-7".into(),
                heartbeat_ms: 15_000,
            })
        );
        assert_eq!(s.session_id(), Some("ws-7"));

        // Subscribe is a client→server frame; before the ack, no topic_ref.
        let sub = s.subscribe("collab:doc", 1);
        assert_eq!(sub.kind, FrameKind::Subscribe);
        assert!(s.data("collab:doc", 1, Bytes::from_static(b"x")).is_none());

        // SubscribeAck binds the ref.
        let ack = Control::SubscribeAck {
            topic: "collab:doc".into(),
            topic_ref: 42,
        }
        .into_frame()
        .unwrap();
        assert_eq!(
            s.on_frame(&ack).unwrap(),
            Some(SessionEvent::Subscribed {
                topic: "collab:doc".into(),
                topic_ref: 42,
            })
        );

        // Now a Data frame can be addressed, and inbound Data routes to the topic.
        let out = s.data("collab:doc", 1, Bytes::from_static(b"hi")).unwrap();
        assert_eq!(out.kind, FrameKind::Data);
        assert_eq!(out.topic_ref, 42);

        let inbound = Frame::data(1, 42, 5, Bytes::from_static(b"peer"));
        assert_eq!(
            s.on_frame(&inbound).unwrap(),
            Some(SessionEvent::Data {
                topic: "collab:doc".into(),
                payload: Bytes::from_static(b"peer"),
            })
        );
    }

    #[test]
    fn data_for_unknown_ref_is_ignored() {
        let mut s = ClientSession::new();
        let inbound = Frame::data(1, 99, 0, Bytes::from_static(b"stray"));
        assert_eq!(s.on_frame(&inbound).unwrap(), None);
    }

    #[test]
    fn gap_drops_the_ref_so_resubscribe_rebinds() {
        let mut s = ClientSession::new();
        let ack = Control::SubscribeAck {
            topic: "t".into(),
            topic_ref: 3,
        }
        .into_frame()
        .unwrap();
        s.on_frame(&ack).unwrap();

        let gap = Control::gap("t", "cursor_not_replayable")
            .into_frame()
            .unwrap();
        assert!(matches!(
            s.on_frame(&gap).unwrap(),
            Some(SessionEvent::Gap { .. })
        ));
        // ref is gone; inbound data on the old ref no longer routes.
        let inbound = Frame::data(1, 3, 0, Bytes::from_static(b"x"));
        assert_eq!(s.on_frame(&inbound).unwrap(), None);
    }

    #[test]
    fn resume_without_a_bound_ref_surfaces_a_gap() {
        let mut s = ClientSession::new();
        // Resumed for a topic this session never acked: must NOT fabricate ref 0.
        let resumed = Control::Resumed { topic: "t".into() }.into_frame().unwrap();
        assert_eq!(
            s.on_frame(&resumed).unwrap(),
            Some(SessionEvent::Gap {
                topic: "t".into(),
                reason: "resume_without_binding".into(),
            })
        );
        // No bogus ref leaked into the maps.
        assert!(s.data("t", 1, Bytes::from_static(b"x")).is_none());
    }

    #[test]
    fn subscribe_denied_is_visible_and_drops_any_stale_ref() {
        let mut s = ClientSession::new();
        let ack = Control::SubscribeAck {
            topic: "private".into(),
            topic_ref: 8,
        }
        .into_frame()
        .unwrap();
        s.on_frame(&ack).unwrap();
        assert!(s.data("private", 1, Bytes::from_static(b"x")).is_some());

        let denied = Control::subscribe_denied("private", "forbidden")
            .into_frame()
            .unwrap();
        assert_eq!(
            s.on_frame(&denied).unwrap(),
            Some(SessionEvent::SubscribeDenied {
                topic: "private".into(),
                reason: "forbidden".into(),
            })
        );
        assert!(s.data("private", 1, Bytes::from_static(b"x")).is_none());
    }

    #[test]
    fn heartbeat_builds_a_control_frame() {
        let s = ClientSession::new();
        assert_eq!(s.heartbeat().unwrap().kind, FrameKind::Control);
    }

    #[test]
    fn reconnect_backoff_grows_then_caps() {
        // Doubles from the 500ms base, then clamps at the 10s ceiling and stays.
        assert_eq!(reconnect_delay_ms(0), 500);
        assert_eq!(reconnect_delay_ms(1), 1_000);
        assert_eq!(reconnect_delay_ms(2), 2_000);
        assert_eq!(reconnect_delay_ms(3), 4_000);
        assert_eq!(reconnect_delay_ms(4), 8_000);
        assert_eq!(reconnect_delay_ms(5), 10_000);
        assert_eq!(reconnect_delay_ms(6), 10_000);
        // Never panics on a large attempt count (no shift overflow).
        assert_eq!(reconnect_delay_ms(u32::MAX), 10_000);
    }
}
