use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{SyncError, SyncResult};

/// Current sync protocol identifier.
pub const SYNC_PROTOCOL_V1: &str = "pocopine.sync.v1";
/// Maximum length, in bytes, accepted for protocol token strings.
pub const MAX_SYNC_TOKEN_LEN: usize = 1024;

macro_rules! sync_path {
    ($suffix:literal) => {
        concat!("/__pocopine/sync/v1", $suffix)
    };
}

/// Default sync endpoint prefix mounted by the server plugin.
pub const SYNC_ENDPOINT_PREFIX: &str = sync_path!("");
/// Open endpoint path.
pub const SYNC_OPEN_PATH: &str = sync_path!("/open");
/// Pull endpoint path.
pub const SYNC_PULL_PATH: &str = sync_path!("/pull");
/// Push endpoint path.
pub const SYNC_PUSH_PATH: &str = sync_path!("/push");

fn validate_token(field: &'static str, value: String) -> SyncResult<String> {
    let trimmed = value.trim();
    if value.len() > MAX_SYNC_TOKEN_LEN
        || trimmed.is_empty()
        || trimmed != value
        || value.chars().any(char::is_control)
    {
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

/// Live query tag used to wake clients for a sync stream.
pub fn sync_stream_tag(stream: &str) -> String {
    format!("sync:stream:{stream}")
}

/// Operation attached to a sync change.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOp {
    Upsert,
    Delete,
    Reset,
}

/// Whether a pull response is a full replacement or incremental batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPullMode {
    Snapshot,
    Incremental,
}

/// Row payload plus sync metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncRow<T> {
    pub key: RowKey,
    pub version: Option<RowVersion>,
    pub value: T,
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub conflict: bool,
}

impl<T> SyncRow<T> {
    pub fn new(key: impl Into<String>, value: T) -> SyncResult<Self> {
        Ok(Self {
            key: RowKey::new(key)?,
            version: None,
            value,
            pending: false,
            conflict: false,
        })
    }

    pub fn version(mut self, version: impl Into<String>) -> SyncResult<Self> {
        self.version = Some(RowVersion::new(version)?);
        Ok(self)
    }
}

/// One ordered change in a sync stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncChange<T> {
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub row: Option<SyncRow<T>>,
    pub cursor: SyncCursor,
}

/// Open one or more streams.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenRequest {
    pub protocol: String,
    #[serde(default)]
    pub client_id: Option<String>,
    pub streams: Vec<SyncStreamName>,
}

impl SyncOpenRequest {
    pub fn new(streams: impl IntoIterator<Item = SyncStreamName>) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            client_id: None,
            streams: streams.into_iter().collect(),
        }
    }
}

/// Stream accepted by an open response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenStream {
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub cursor: Option<SyncCursor>,
}

/// Open response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenResponse {
    pub protocol: String,
    pub streams: Vec<SyncOpenStream>,
}

impl SyncOpenResponse {
    pub fn new(streams: Vec<SyncOpenStream>) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            streams,
        }
    }
}

/// Pull request for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncPullRequest {
    pub protocol: String,
    pub stream: SyncStreamName,
    pub cursor: Option<SyncCursor>,
    pub limit: u32,
}

impl SyncPullRequest {
    pub fn new(stream: SyncStreamName) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            cursor: None,
            limit: 500,
        }
    }

    pub fn cursor(mut self, cursor: Option<SyncCursor>) -> Self {
        self.cursor = cursor;
        self
    }
}

/// Pull response for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncPullResponse<T> {
    pub protocol: String,
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub mode: SyncPullMode,
    #[serde(default)]
    pub rows: Vec<SyncRow<T>>,
    #[serde(default)]
    pub changes: Vec<SyncChange<T>>,
    pub cursor: Option<SyncCursor>,
    pub has_more: bool,
}

impl<T> SyncPullResponse<T> {
    pub fn snapshot(
        stream: SyncStreamName,
        collection: SyncCollectionName,
        rows: Vec<SyncRow<T>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            collection,
            mode: SyncPullMode::Snapshot,
            rows,
            changes: Vec::new(),
            cursor,
            has_more: false,
        }
    }

    pub fn incremental(
        stream: SyncStreamName,
        collection: SyncCollectionName,
        changes: Vec<SyncChange<T>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            collection,
            mode: SyncPullMode::Incremental,
            rows: Vec::new(),
            changes,
            cursor,
            has_more: false,
        }
    }
}

/// Client mutation envelope. The first slice defines the wire format;
/// concrete mutation application belongs to stream sources.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct ClientMutation<M> {
    pub id: MutationId,
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub base_version: Option<RowVersion>,
    pub payload: M,
}

/// Push request for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct SyncPushRequest<M> {
    pub protocol: String,
    pub stream: SyncStreamName,
    #[serde(default)]
    pub mutations: Vec<ClientMutation<M>>,
}

impl<M> SyncPushRequest<M> {
    pub fn new(
        stream: SyncStreamName,
        mutations: impl IntoIterator<Item = ClientMutation<M>>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            mutations: mutations.into_iter().collect(),
        }
    }
}

/// Conflict returned by a push.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncConflict<T> {
    pub mutation_id: MutationId,
    pub key: Option<RowKey>,
    pub server_row: Option<SyncRow<T>>,
    pub reason: String,
}

/// Rejected mutation returned by a push.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncRejectedMutation {
    pub mutation_id: MutationId,
    pub key: Option<RowKey>,
    pub reason: String,
}

/// Push response for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncPushResponse<T> {
    pub protocol: String,
    pub stream: SyncStreamName,
    #[serde(default)]
    pub accepted: Vec<MutationId>,
    #[serde(default)]
    pub rejected: Vec<SyncRejectedMutation>,
    #[serde(default)]
    pub rows: Vec<SyncRow<T>>,
    #[serde(default)]
    pub conflicts: Vec<SyncConflict<T>>,
    pub cursor: Option<SyncCursor>,
}

impl<T> SyncPushResponse<T> {
    pub fn new(stream: SyncStreamName) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            accepted: Vec::new(),
            rejected: Vec::new(),
            rows: Vec::new(),
            conflicts: Vec::new(),
            cursor: None,
        }
    }
}

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

    #[test]
    fn deserializes_tokens_through_validation() {
        let stream: SyncStreamName = serde_json::from_str("\"posts_for_tenant\"").unwrap();
        assert_eq!(stream.as_str(), "posts_for_tenant");
        assert!(serde_json::from_str::<SyncStreamName>("\" posts\"").is_err());
        assert!(serde_json::from_str::<SyncStreamName>("\"posts\\nbad\"").is_err());
    }

    #[test]
    fn sync_stream_tag_is_stable() {
        assert_eq!(
            sync_stream_tag("posts_for_tenant"),
            "sync:stream:posts_for_tenant"
        );
    }

    #[test]
    fn endpoint_paths_share_prefix() {
        assert!(SYNC_OPEN_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_OPEN_PATH.ends_with("/open"));
        assert!(SYNC_PULL_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_PULL_PATH.ends_with("/pull"));
        assert!(SYNC_PUSH_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_PUSH_PATH.ends_with("/push"));
    }
}
