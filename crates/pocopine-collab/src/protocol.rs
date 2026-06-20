//! The collab sub-protocol carried inside `pocopine-realtime` Data frames
//! (RFC 073 Part II, the wire shape of the Yjs sync handshake).
//!
//! Each Data frame payload is exactly one [`CollabMessage`]: a single tag byte
//! followed by the raw body (a yrs state vector or update, both already
//! self-delimiting because they run to the end of the frame). This mirrors
//! y-protocols' `messageSync` sub-types, minus the redundant length prefix the
//! frame envelope already makes unnecessary.
//!
//! ```text
//! message := u8(tag) body
//!   tag 0  SyncStep1   body = state vector       ("what I have")
//!   tag 1  SyncStep2   body = update diff        ("what you were missing")
//!   tag 2  Update      body = update             (a live edit to converge)
//!   tag 3  Awareness   body = awareness update   (ephemeral presence/cursors)
//! ```
//!
//! `Awareness` is **ephemeral**: the server relays it to peers (broadcast) but
//! never applies it to the document and never persists it. It carries
//! presence/cursor state (see `pocopine_collab::awareness`), which is meaningless
//! once a peer disconnects, so there is nothing to converge or checkpoint.

use bytes::Bytes;

use crate::error::{CollabError, CollabResult};

/// The conventional `subprotocol_id` apps register the collab handler under and
/// tag collab Data frames with. Apps may choose another id; this is the default
/// both [`crate::CollabSync`] consumers and the client SDK assume.
pub const COLLAB_SUBPROTOCOL: u64 = 1;

const TAG_SYNC_STEP1: u8 = 0;
const TAG_SYNC_STEP2: u8 = 1;
const TAG_UPDATE: u8 = 2;
const TAG_AWARENESS: u8 = 3;

/// One collab sub-protocol message (the body of a single Data frame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollabMessage {
    /// A peer's state vector — "here is what I already have" (SyncStep1).
    SyncStep1(Bytes),
    /// The update a peer was missing, computed from its SyncStep1 (SyncStep2).
    SyncStep2(Bytes),
    /// A live document update to merge and converge everywhere (Update).
    Update(Bytes),
    /// An ephemeral awareness/presence update (cursors, selections, identity).
    /// Relayed to peers but never applied to the document or persisted.
    Awareness(Bytes),
}

impl CollabMessage {
    /// Encode as `tag || body`.
    pub fn encode(&self) -> Bytes {
        let (tag, body) = match self {
            Self::SyncStep1(body) => (TAG_SYNC_STEP1, body),
            Self::SyncStep2(body) => (TAG_SYNC_STEP2, body),
            Self::Update(body) => (TAG_UPDATE, body),
            Self::Awareness(body) => (TAG_AWARENESS, body),
        };
        let mut out = Vec::with_capacity(body.len() + 1);
        out.push(tag);
        out.extend_from_slice(body);
        Bytes::from(out)
    }

    /// Decode a frame payload into a message, or `CollabError::Decode` on an
    /// empty payload / unknown tag.
    pub fn decode(payload: &[u8]) -> CollabResult<Self> {
        let (&tag, body) = payload
            .split_first()
            .ok_or_else(|| CollabError::Decode("empty collab message".into()))?;
        let body = Bytes::copy_from_slice(body);
        match tag {
            TAG_SYNC_STEP1 => Ok(Self::SyncStep1(body)),
            TAG_SYNC_STEP2 => Ok(Self::SyncStep2(body)),
            TAG_UPDATE => Ok(Self::Update(body)),
            TAG_AWARENESS => Ok(Self::Awareness(body)),
            other => Err(CollabError::Decode(format!(
                "unknown collab message tag {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: CollabMessage) {
        let encoded = msg.encode();
        assert_eq!(CollabMessage::decode(&encoded).unwrap(), msg);
    }

    #[test]
    fn roundtrips_every_variant() {
        roundtrip(CollabMessage::SyncStep1(Bytes::from_static(b"\x00\x01sv")));
        roundtrip(CollabMessage::SyncStep2(Bytes::from_static(b"diff")));
        roundtrip(CollabMessage::Update(Bytes::from_static(b"update")));
        roundtrip(CollabMessage::Awareness(Bytes::from_static(b"presence")));
    }

    #[test]
    fn empty_body_roundtrips() {
        // An empty state vector (a brand-new peer) is a one-byte message.
        let encoded = CollabMessage::SyncStep1(Bytes::new()).encode();
        assert_eq!(encoded.len(), 1);
        assert_eq!(
            CollabMessage::decode(&encoded).unwrap(),
            CollabMessage::SyncStep1(Bytes::new())
        );
    }

    #[test]
    fn empty_payload_is_a_decode_error() {
        assert!(matches!(
            CollabMessage::decode(&[]),
            Err(CollabError::Decode(_))
        ));
    }

    #[test]
    fn unknown_tag_is_a_decode_error() {
        assert!(matches!(
            CollabMessage::decode(&[9, 1, 2, 3]),
            Err(CollabError::Decode(_))
        ));
    }
}
