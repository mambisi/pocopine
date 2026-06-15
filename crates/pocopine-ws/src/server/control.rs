//! Control-frame bodies (RFC 073 §8–§9).
//!
//! Control frames (`FrameKind::Control`) carry a JSON-tagged [`Control`]
//! value as their payload. JSON (rather than a bespoke binary encoding) keeps
//! the lifecycle protocol debuggable and forward-compatible; the surrounding
//! frame envelope is still the binary varUint header from
//! [`crate::server::frame`].
//! Data frames never use this module — their payloads are opaque bytes.

use serde::{Deserialize, Serialize};

use super::error::WsError;
use super::frame::Frame;

/// A `(topic, last_seq)` pair used by Heartbeat and Resume. On Resume the
/// `subprotocol_id` tells the gateway how to tag replayed/live Data frames for
/// the topic (Heartbeat acks leave it at its `0` default).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TopicSeq {
    pub topic: String,
    pub last_seq: u64,
    #[serde(default)]
    pub subprotocol_id: u64,
}

/// The gateway lifecycle control messages exchanged over `FrameKind::Control`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    /// Server → client, immediately after upgrade.
    Hello {
        session_id: String,
        heartbeat_interval_ms: u32,
        protocol: String,
    },
    /// Client → server liveness ping. Carries the client's highest processed
    /// seq per topic so the server can detect drift.
    Heartbeat {
        #[serde(default)]
        acks: Vec<TopicSeq>,
    },
    /// Server → client reply to a Heartbeat.
    HeartbeatAck {},
    /// Server → client: a Subscribe was accepted; `topic_ref` is the compact
    /// handle to use on subsequent Data/Unsubscribe frames for this topic.
    SubscribeAck { topic: String, topic_ref: u64 },
    /// Server → client: a topic was left.
    Unsubscribed { topic: String },
    /// Client → server: resume an existing session, replaying each topic from
    /// `last_seq`.
    Resume {
        session_id: String,
        #[serde(default)]
        topics: Vec<TopicSeq>,
    },
    /// Server → client: a topic's resume backlog has been flushed in order.
    Resumed { topic: String },
    /// Server → client: a resume cursor is unreplayable or the subscription
    /// lagged; the client must re-subscribe fresh.
    Gap { topic: String, reason: String },
    /// Server → client: a typed error (forbidden topic, oversized frame, ...).
    Error { code: String, message: String },
}

impl Control {
    /// JSON-encode the control body.
    pub fn encode(&self) -> Result<Vec<u8>, WsError> {
        serde_json::to_vec(self).map_err(|err| WsError::frame(format!("control encode: {err}")))
    }

    /// Decode a control body from a control frame payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, WsError> {
        serde_json::from_slice(bytes)
            .map_err(|err| WsError::frame(format!("control decode: {err}")))
    }

    /// Wrap this control body in a `FrameKind::Control` frame.
    pub fn into_frame(&self) -> Result<Frame, WsError> {
        Ok(Frame::control(self.encode()?))
    }

    /// Convenience constructor for an [`Control::Error`].
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Convenience constructor for an [`Control::Gap`].
    pub fn gap(topic: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Gap {
            topic: topic.into(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::frame::FrameKind;

    fn roundtrip(control: Control) {
        let bytes = control.encode().expect("encode");
        let decoded = Control::decode(&bytes).expect("decode");
        assert_eq!(decoded, control);
    }

    #[test]
    fn roundtrips_all_variants() {
        roundtrip(Control::Hello {
            session_id: "s1".into(),
            heartbeat_interval_ms: 15_000,
            protocol: "pocopine.ws.v1".into(),
        });
        roundtrip(Control::Heartbeat {
            acks: vec![TopicSeq {
                topic: "collab:abc".into(),
                last_seq: 12,
                subprotocol_id: 0,
            }],
        });
        roundtrip(Control::HeartbeatAck {});
        roundtrip(Control::SubscribeAck {
            topic: "collab:abc".into(),
            topic_ref: 1,
        });
        roundtrip(Control::Unsubscribed {
            topic: "collab:abc".into(),
        });
        roundtrip(Control::Resume {
            session_id: "s1".into(),
            topics: vec![TopicSeq {
                topic: "collab:abc".into(),
                last_seq: 9,
                subprotocol_id: 1,
            }],
        });
        roundtrip(Control::Resumed {
            topic: "collab:abc".into(),
        });
        roundtrip(Control::gap("collab:abc", "cursor_not_replayable"));
        roundtrip(Control::error("forbidden_topic", "denied"));
    }

    #[test]
    fn heartbeat_acks_default_to_empty() {
        let decoded = Control::decode(br#"{"type":"heartbeat"}"#).expect("decode");
        assert_eq!(decoded, Control::Heartbeat { acks: vec![] });
    }

    #[test]
    fn wraps_into_control_frame() {
        let frame = Control::HeartbeatAck {}.into_frame().expect("frame");
        assert_eq!(frame.kind, FrameKind::Control);
        let body = Control::decode(&frame.payload).expect("decode body");
        assert_eq!(body, Control::HeartbeatAck {});
    }

    #[test]
    fn rejects_unknown_type() {
        assert!(Control::decode(br#"{"type":"nope"}"#).is_err());
    }
}
