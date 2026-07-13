//! The collab sub-protocol carried inside `pocopine-realtime` Data frames.
//!
//! Every subscription starts with a compatibility-bearing [`CollabHello`]. A
//! peer must validate that hello before it exchanges or applies any yrs update.
//! This matters because an opaque yrs update does not describe the application
//! schema or step encoding it expects.
//!
//! ```text
//! message := u8(tag) body
//!   tag 0  Hello       body = u16(protocol, BE)
//!                              u8(flags)
//!                              [u8; 64](lowercase fingerprint hex)
//!                              state vector
//!   tag 1  SyncStep2   body = update diff
//!   tag 2  Update      body = live update
//!   tag 3  Awareness   body = ephemeral presence/cursors
//! ```
//!
//! A hello's `REQUEST_SYNC_STEP2` flag asks the receiver to return the diff the
//! sender is missing. The server clears that flag in its hello to a read-only
//! peer: the peer can validate the server before applying its catch-up, without
//! being invited to upload a diff it is not authorized to write.

use bytes::Bytes;

use crate::compatibility::{CompatibilityIdentity, FINGERPRINT_HEX_LEN};
use crate::error::{CollabError, CollabResult};

/// The conventional realtime subprotocol id for collaboration.
pub const COLLAB_SUBPROTOCOL: u64 = 1;

pub(crate) const TAG_HELLO: u8 = 0;
const TAG_SYNC_STEP2: u8 = 1;
const TAG_UPDATE: u8 = 2;
const TAG_AWARENESS: u8 = 3;

const FLAG_REQUEST_SYNC_STEP2: u8 = 1 << 0;
const KNOWN_HELLO_FLAGS: u8 = FLAG_REQUEST_SYNC_STEP2;
const HELLO_HEADER_LEN: usize = 2 + 1 + FINGERPRINT_HEX_LEN;

/// Opening compatibility negotiation for one document subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollabHello {
    compatibility: CompatibilityIdentity,
    state_vector: Bytes,
    request_sync_step2: bool,
}

impl CollabHello {
    /// Build a hello carrying this peer's state vector.
    pub fn new(
        compatibility: CompatibilityIdentity,
        state_vector: impl Into<Bytes>,
        request_sync_step2: bool,
    ) -> Self {
        Self {
            compatibility,
            state_vector: state_vector.into(),
            request_sync_step2,
        }
    }

    /// The application protocol and schema identity this peer speaks.
    pub fn compatibility(&self) -> &CompatibilityIdentity {
        &self.compatibility
    }

    /// This peer's yrs state vector.
    pub fn state_vector(&self) -> &Bytes {
        &self.state_vector
    }

    /// Whether the receiver should answer with a [`CollabMessage::SyncStep2`].
    pub fn requests_sync_step2(&self) -> bool {
        self.request_sync_step2
    }
}

/// One collab sub-protocol message (the body of one realtime Data frame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollabMessage {
    /// Required opening protocol/schema identity plus this peer's state vector.
    Hello(CollabHello),
    /// The update a peer was missing, computed from its hello state vector.
    SyncStep2(Bytes),
    /// A live document update to merge and converge everywhere.
    Update(Bytes),
    /// Ephemeral awareness/presence, relayed but never persisted.
    Awareness(Bytes),
}

impl CollabMessage {
    /// Encode the typed message into its compact wire representation.
    pub fn encode(&self) -> Bytes {
        match self {
            Self::Hello(hello) => {
                let mut out = Vec::with_capacity(1 + HELLO_HEADER_LEN + hello.state_vector.len());
                out.push(TAG_HELLO);
                out.extend_from_slice(&hello.compatibility.protocol_version().to_be_bytes());
                out.push(if hello.request_sync_step2 {
                    FLAG_REQUEST_SYNC_STEP2
                } else {
                    0
                });
                out.extend_from_slice(hello.compatibility.fingerprint().as_bytes());
                out.extend_from_slice(&hello.state_vector);
                Bytes::from(out)
            }
            Self::SyncStep2(body) => encode_body(TAG_SYNC_STEP2, body),
            Self::Update(body) => encode_body(TAG_UPDATE, body),
            Self::Awareness(body) => encode_body(TAG_AWARENESS, body),
        }
    }

    /// Decode and validate one frame payload.
    pub fn decode(payload: &[u8]) -> CollabResult<Self> {
        let (&tag, body) = payload
            .split_first()
            .ok_or_else(|| CollabError::Decode("empty collab message".into()))?;
        match tag {
            TAG_HELLO => decode_hello(body).map(Self::Hello),
            TAG_SYNC_STEP2 => Ok(Self::SyncStep2(Bytes::copy_from_slice(body))),
            TAG_UPDATE => Ok(Self::Update(Bytes::copy_from_slice(body))),
            TAG_AWARENESS => Ok(Self::Awareness(Bytes::copy_from_slice(body))),
            other => Err(CollabError::Decode(format!(
                "unknown collab message tag {other}"
            ))),
        }
    }
}

fn encode_body(tag: u8, body: &Bytes) -> Bytes {
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(tag);
    out.extend_from_slice(body);
    Bytes::from(out)
}

fn decode_hello(body: &[u8]) -> CollabResult<CollabHello> {
    if body.len() < HELLO_HEADER_LEN {
        return Err(CollabError::Decode(format!(
            "collab hello is truncated: expected at least {HELLO_HEADER_LEN} body bytes, got {}",
            body.len()
        )));
    }
    let protocol_version = u16::from_be_bytes([body[0], body[1]]);
    let flags = body[2];
    if flags & !KNOWN_HELLO_FLAGS != 0 {
        return Err(CollabError::Decode(format!(
            "collab hello has unknown flags 0x{flags:02x}"
        )));
    }

    let fingerprint_bytes = &body[3..HELLO_HEADER_LEN];
    let fingerprint = std::str::from_utf8(fingerprint_bytes)
        .map_err(|_| CollabError::Decode("collab hello fingerprint is not UTF-8".into()))?;
    let compatibility = CompatibilityIdentity::new(protocol_version, fingerprint.to_owned())
        .map_err(|err| CollabError::Decode(err.to_string()))?;

    Ok(CollabHello::new(
        compatibility,
        Bytes::copy_from_slice(&body[HELLO_HEADER_LEN..]),
        flags & FLAG_REQUEST_SYNC_STEP2 != 0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn identity() -> CompatibilityIdentity {
        CompatibilityIdentity::new(9, FINGERPRINT).unwrap()
    }

    fn roundtrip(message: CollabMessage) {
        let encoded = message.encode();
        assert_eq!(CollabMessage::decode(&encoded).unwrap(), message);
    }

    #[test]
    fn roundtrips_every_variant() {
        roundtrip(CollabMessage::Hello(CollabHello::new(
            identity(),
            Bytes::from_static(b"state-vector"),
            true,
        )));
        roundtrip(CollabMessage::Hello(CollabHello::new(
            identity(),
            Bytes::new(),
            false,
        )));
        roundtrip(CollabMessage::SyncStep2(Bytes::from_static(b"diff")));
        roundtrip(CollabMessage::Update(Bytes::from_static(b"update")));
        roundtrip(CollabMessage::Awareness(Bytes::from_static(b"presence")));
    }

    #[test]
    fn hello_layout_is_stable_and_versioned() {
        let encoded = CollabMessage::Hello(CollabHello::new(
            identity(),
            Bytes::from_static(b"sv"),
            true,
        ))
        .encode();
        assert_eq!(encoded[0], TAG_HELLO);
        assert_eq!(&encoded[1..3], &9_u16.to_be_bytes());
        assert_eq!(encoded[3], FLAG_REQUEST_SYNC_STEP2);
        assert_eq!(&encoded[4..68], FINGERPRINT.as_bytes());
        assert_eq!(&encoded[68..], b"sv");
    }

    #[test]
    fn malformed_hello_fails_closed() {
        assert!(matches!(
            CollabMessage::decode(&[TAG_HELLO]),
            Err(CollabError::Decode(_))
        ));

        let mut unknown_flags =
            CollabMessage::Hello(CollabHello::new(identity(), Bytes::new(), false))
                .encode()
                .to_vec();
        unknown_flags[3] = 0x80;
        assert!(matches!(
            CollabMessage::decode(&unknown_flags),
            Err(CollabError::Decode(_))
        ));

        let mut uppercase = CollabMessage::Hello(CollabHello::new(identity(), Bytes::new(), false))
            .encode()
            .to_vec();
        uppercase[4] = b'A';
        assert!(matches!(
            CollabMessage::decode(&uppercase),
            Err(CollabError::Decode(_))
        ));
    }

    #[test]
    fn empty_payload_and_unknown_tag_are_decode_errors() {
        assert!(matches!(
            CollabMessage::decode(&[]),
            Err(CollabError::Decode(_))
        ));
        assert!(matches!(
            CollabMessage::decode(&[9, 1, 2, 3]),
            Err(CollabError::Decode(_))
        ));
    }
}
