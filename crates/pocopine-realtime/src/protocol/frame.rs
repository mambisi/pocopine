//! Binary frame envelope (RFC 073 §7).
//!
//! ```text
//! frame := varUint(frame_kind)
//!          varUint(subprotocol_id)
//!          varUint(topic_ref)        # 0 for control frames not bound to a topic
//!          varUint(seq)             # per-subscription seq on delivered Data; 0 otherwise
//!          payload_bytes            # opaque to the gateway
//! ```
//!
//! The gateway parses only this header; the trailing `payload_bytes` are
//! opaque to it. Control frames (kind 0) carry a JSON [`crate::Control`]
//! body by convention; Data frames (kind 3) carry an opaque application
//! payload the gateway never decodes. `seq` carries the monotonic
//! per-subscription sequence number on server→client Data frames (RFC 073
//! §9); it is 0 on control frames and on client→server frames.

use bytes::Bytes;

use super::error::ProtocolError;
use super::varint::{read_var_uint, write_var_uint};

/// Top-level frame discriminant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameKind {
    /// Gateway lifecycle (Hello, Heartbeat, Resume, Gap, ...). Payload is a
    /// JSON [`crate::Control`] body.
    Control,
    /// Request to join a topic. Payload is the UTF-8 topic string.
    Subscribe,
    /// Leave a topic identified by `topic_ref`. Payload is empty.
    Unsubscribe,
    /// An application (sub-protocol) message bound to `topic_ref`.
    Data,
}

impl FrameKind {
    fn to_u64(self) -> u64 {
        match self {
            Self::Control => 0,
            Self::Subscribe => 1,
            Self::Unsubscribe => 2,
            Self::Data => 3,
        }
    }

    fn from_u64(value: u64) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(Self::Control),
            1 => Ok(Self::Subscribe),
            2 => Ok(Self::Unsubscribe),
            3 => Ok(Self::Data),
            other => Err(ProtocolError::frame(format!("unknown frame_kind {other}"))),
        }
    }
}

/// A decoded frame envelope plus its opaque payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: FrameKind,
    pub subprotocol_id: u64,
    pub topic_ref: u64,
    pub seq: u64,
    pub payload: Bytes,
}

impl Frame {
    /// Construct an arbitrary frame.
    pub fn new(
        kind: FrameKind,
        subprotocol_id: u64,
        topic_ref: u64,
        seq: u64,
        payload: impl Into<Bytes>,
    ) -> Self {
        Self {
            kind,
            subprotocol_id,
            topic_ref,
            seq,
            payload: payload.into(),
        }
    }

    /// A control frame (kind 0, sub-protocol 0, topic_ref 0, seq 0).
    pub fn control(payload: impl Into<Bytes>) -> Self {
        Self::new(FrameKind::Control, 0, 0, 0, payload)
    }

    /// A Subscribe frame naming `topic` for `subprotocol_id`.
    pub fn subscribe(subprotocol_id: u64, topic: &str) -> Self {
        Self::new(
            FrameKind::Subscribe,
            subprotocol_id,
            0,
            0,
            Bytes::copy_from_slice(topic.as_bytes()),
        )
    }

    /// An Unsubscribe frame for `topic_ref`.
    pub fn unsubscribe(topic_ref: u64) -> Self {
        Self::new(FrameKind::Unsubscribe, 0, topic_ref, 0, Bytes::new())
    }

    /// A server→client Data frame for `topic_ref` carrying `seq` and an opaque
    /// application payload.
    pub fn data(subprotocol_id: u64, topic_ref: u64, seq: u64, payload: impl Into<Bytes>) -> Self {
        Self::new(FrameKind::Data, subprotocol_id, topic_ref, seq, payload)
    }

    /// Serialize the envelope header followed by the payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.payload.len() + 5);
        write_var_uint(&mut buf, self.kind.to_u64());
        write_var_uint(&mut buf, self.subprotocol_id);
        write_var_uint(&mut buf, self.topic_ref);
        write_var_uint(&mut buf, self.seq);
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Decode a frame from a complete WebSocket binary message.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let mut pos = 0usize;
        let kind = FrameKind::from_u64(read_var_uint(bytes, &mut pos)?)?;
        let subprotocol_id = read_var_uint(bytes, &mut pos)?;
        let topic_ref = read_var_uint(bytes, &mut pos)?;
        let seq = read_var_uint(bytes, &mut pos)?;
        let payload = Bytes::copy_from_slice(&bytes[pos..]);
        Ok(Self {
            kind,
            subprotocol_id,
            topic_ref,
            seq,
            payload,
        })
    }

    /// Interpret the payload as a UTF-8 string (used for Subscribe frames).
    pub fn payload_str(&self) -> Result<&str, ProtocolError> {
        std::str::from_utf8(&self.payload)
            .map_err(|_| ProtocolError::frame("frame payload is not valid UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_data_frame() {
        let frame = Frame::data(1, 42, 7, Bytes::from_static(b"\x00\x01\x02opaque"));
        let encoded = frame.encode();
        let decoded = Frame::decode(&encoded).expect("decode");
        assert_eq!(decoded, frame);
        assert_eq!(decoded.kind, FrameKind::Data);
        assert_eq!(decoded.subprotocol_id, 1);
        assert_eq!(decoded.topic_ref, 42);
        assert_eq!(decoded.seq, 7);
    }

    #[test]
    fn roundtrips_control_frame() {
        let frame = Frame::control(Bytes::from_static(b"{\"type\":\"heartbeat_ack\"}"));
        let decoded = Frame::decode(&frame.encode()).expect("decode");
        assert_eq!(decoded.kind, FrameKind::Control);
        assert_eq!(decoded.subprotocol_id, 0);
        assert_eq!(decoded.topic_ref, 0);
        assert_eq!(decoded.seq, 0);
        assert_eq!(decoded.payload, frame.payload);
    }

    #[test]
    fn subscribe_frame_carries_topic_name() {
        let frame = Frame::subscribe(1, "collab:abc");
        let decoded = Frame::decode(&frame.encode()).expect("decode");
        assert_eq!(decoded.kind, FrameKind::Subscribe);
        assert_eq!(decoded.subprotocol_id, 1);
        assert_eq!(decoded.payload_str().unwrap(), "collab:abc");
    }

    #[test]
    fn empty_payload_roundtrips() {
        let frame = Frame::unsubscribe(7);
        let decoded = Frame::decode(&frame.encode()).expect("decode");
        assert_eq!(decoded, frame);
        assert!(decoded.payload.is_empty());
        assert_eq!(decoded.topic_ref, 7);
    }

    #[test]
    fn large_values_roundtrip() {
        let frame = Frame::data(9, u32::MAX as u64 + 5, u64::MAX, Bytes::from_static(b"x"));
        let decoded = Frame::decode(&frame.encode()).expect("decode");
        assert_eq!(decoded.topic_ref, u32::MAX as u64 + 5);
        assert_eq!(decoded.seq, u64::MAX);
    }

    #[test]
    fn unknown_kind_errors() {
        // frame_kind = 9 (varuint single byte), then three zero varuints.
        let bytes = [9u8, 0, 0, 0];
        assert!(Frame::decode(&bytes).is_err());
    }

    #[test]
    fn truncated_header_errors() {
        // Only frame_kind present, missing the rest of the header.
        let bytes = [3u8];
        assert!(Frame::decode(&bytes).is_err());
    }
}
