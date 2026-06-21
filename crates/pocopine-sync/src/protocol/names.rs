use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{SyncError, SyncResult};

use super::MAX_SYNC_TOKEN_LEN;

fn validate_token(field: &'static str, value: String) -> SyncResult<String> {
    let trimmed = value.trim();
    if value.len() > MAX_SYNC_TOKEN_LEN
        || trimmed.is_empty()
        || trimmed != value
        || value.chars().any(char::is_control)
    {
        return Err(SyncError::invalid_value(field, value));
    }
    // Stream-name-only: reject `:` because the live-wakeup wire
    // protocol uses it as a structural delimiter (see
    // [`sync_stream_tag`] / [`sync_stream_params_tag`] / RFC 088
    // §C). A stream named `"issues:abcd1234abcd1234"` would
    // produce the bare topic `query:sync:stream:issues:abcd…`
    // which is indistinguishable from a per-params topic for the
    // public `issues` prefix — a public-prefix allowlist would
    // then authorize access to the private sibling stream. We
    // forbid the collision at the source of truth (the token
    // validator) rather than chase it downstream.
    //
    // Field-specific via the `field` discriminator — RowKey,
    // MutationId, etc. legitimately use `:` in composite keys.
    // Only the stream token namespace is protocol-reserved.
    if field == "stream" && value.contains(':') {
        return Err(SyncError::invalid_value(field, value));
    }
    Ok(value)
}

macro_rules! opaque_string_type {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(String);

        impl $name {
            /// Build a validated value.
            pub fn new(value: impl Into<String>) -> SyncResult<Self> {
                validate_token($field, value.into()).map(Self)
            }

            /// Borrow the string value.
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl std::str::FromStr for $name {
            type Err = SyncError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = SyncError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = SyncError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

opaque_string_type!(
    SyncStreamName,
    "stream",
    "Server-registered sync stream name."
);
opaque_string_type!(
    SyncCollectionName,
    "collection",
    "Public collection name exposed to the client."
);
opaque_string_type!(SyncCursor, "cursor", "Opaque server-issued sync cursor.");
opaque_string_type!(
    RowKey,
    "row key",
    "Public row identity inside one sync stream."
);
opaque_string_type!(
    RowVersion,
    "row version",
    "Opaque server-issued row version."
);
opaque_string_type!(
    MutationId,
    "mutation id",
    "Client-generated mutation idempotency key."
);

impl MutationId {
    /// Generate a UUIDv7-backed mutation id. The default constructor
    /// used by `TypedMutation::push(&qc)` and any other framework
    /// auto-id path.
    ///
    /// UUIDv7 is time-ordered (the leading 48 bits are an ms-resolution
    /// Unix timestamp), so mutation logs and offline replay queues keep
    /// their insertion order without an extra sort key. The string form
    /// (`xxxxxxxx-xxxx-7xxx-yxxx-xxxxxxxxxxxx`) is always 36 bytes —
    /// well under `MutationId`'s opaque-string length cap — so the
    /// `unwrap` below is infallible.
    pub fn uuid() -> Self {
        Self::new(uuid::Uuid::now_v7().to_string()).expect("UUIDv7 is always a valid MutationId")
    }
}

opaque_string_type!(
    SyncDeviceId,
    "device id",
    "Stable client device identity persisted by a local sync store."
);
opaque_string_type!(
    SyncSessionId,
    "session id",
    "Ephemeral sync session identity for one running client instance."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_stream_names() {
        assert!(SyncStreamName::new("posts_for_tenant").is_ok());
        assert!(SyncStreamName::new("").is_err());
        assert!(SyncStreamName::new(" posts").is_err());
        assert!(SyncStreamName::new("posts\nbad").is_err());
        assert!(SyncStreamName::new("x".repeat(MAX_SYNC_TOKEN_LEN + 1)).is_err());
    }

    // RFC 088 §C: stream names must not contain `:` — the live-
    // wakeup wire protocol uses it as a structural delimiter, and
    // a sibling stream `issues:abcd1234abcd1234` would otherwise
    // collide with `issues`'s per-params topic format, breaking the
    // `LiveHub::allow_topic_prefixes` security boundary.
    #[test]
    fn rejects_colon_in_stream_name() {
        // 16-hex-suffix sibling: indistinguishable from per-params
        // format `query:sync:stream:issues:<hash>`.
        assert!(SyncStreamName::new("issues:abcd1234abcd1234").is_err());
        // Other colon-containing names also rejected.
        assert!(SyncStreamName::new("a:b").is_err());
        // Other token types may still use `:` (RowKey composite keys,
        // etc.). Only `stream` is protocol-reserved.
        assert!(RowKey::new("tenant:row_42").is_ok());
        assert!(MutationId::new("test:1").is_ok());
    }

    #[test]
    fn token_types_parse_and_borrow_as_str() {
        let stream: SyncStreamName = "posts_for_tenant".parse().unwrap();

        assert_eq!(stream.as_ref(), "posts_for_tenant");
        assert!("bad\nstream".parse::<SyncStreamName>().is_err());
    }

    #[test]
    fn validates_device_and_session_ids() {
        assert!(SyncDeviceId::new("device_abc").is_ok());
        assert!(SyncSessionId::new("session_abc").is_ok());
        assert!(SyncDeviceId::new("").is_err());
        assert!(SyncSessionId::new("session bad\n").is_err());
    }

    #[test]
    fn deserializes_tokens_through_validation() {
        let stream: SyncStreamName = serde_json::from_str("\"posts_for_tenant\"").unwrap();
        assert_eq!(stream.as_str(), "posts_for_tenant");
        assert!(serde_json::from_str::<SyncStreamName>("\" posts\"").is_err());
        assert!(serde_json::from_str::<SyncStreamName>("\"posts\\nbad\"").is_err());
    }
}
