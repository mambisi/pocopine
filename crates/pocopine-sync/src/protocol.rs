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

    /// Attach an already validated row version.
    pub fn row_version(mut self, version: RowVersion) -> Self {
        self.version = Some(version);
        self
    }

    /// Mark whether this row is waiting for a local push outcome.
    pub fn pending(mut self, pending: bool) -> Self {
        self.pending = pending;
        self
    }

    /// Mark whether this row is showing a server conflict.
    pub fn conflict(mut self, conflict: bool) -> Self {
        self.conflict = conflict;
        self
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
    pub client_id: Option<SyncDeviceId>,
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

    pub fn client_id(mut self, client_id: SyncDeviceId) -> Self {
        self.client_id = Some(client_id);
        self
    }
}

/// Stream accepted by an open response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenStream {
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub cursor: Option<SyncCursor>,
    /// Application-level schema version the server is serving for this
    /// stream. Authors declare it via `#[resource(schema_version = N)]`
    /// (or directly on `SyncStreamSource::schema_version`); the client
    /// compares against its cached value to decide whether the local cache
    /// is still valid. The `#[serde(default)]` makes the wire field
    /// backwards-compatible: an old server response without the field
    /// deserializes as `1`, and an old client reading a field-bearing
    /// response just ignores it (serde drops unknown fields).
    #[serde(default = "default_schema_version_one")]
    pub schema_version: u32,
}

/// Default schema version when the wire field is missing or the source
/// doesn't override the trait default. `1` means "first version of the
/// schema" — apps that have never bumped never observe this field.
pub fn default_schema_version_one() -> u32 {
    1
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

impl<M> ClientMutation<M> {
    /// Build a mutation with a caller-supplied id.
    pub fn new(id: MutationId, op: SyncOp, payload: M) -> Self {
        Self {
            id,
            key: None,
            op,
            base_version: None,
            payload,
        }
    }

    /// Build an upsert mutation with a caller-supplied id.
    pub fn upsert(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Upsert, payload)
    }

    /// Build a delete mutation with a caller-supplied id.
    pub fn delete(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Delete, payload)
    }

    /// Build a reset mutation with a caller-supplied id.
    pub fn reset(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Reset, payload)
    }

    /// Build a mutation scoped to a row.
    pub fn for_row(
        id: MutationId,
        op: SyncOp,
        key: impl Into<String>,
        payload: M,
    ) -> SyncResult<Self> {
        Self::new(id, op, payload).key(key)
    }

    /// Build an upsert mutation scoped to a row.
    pub fn upsert_row(id: MutationId, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::upsert(id, payload).key(key)
    }

    /// Build a delete mutation scoped to a row.
    pub fn delete_row(id: MutationId, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::delete(id, payload).key(key)
    }

    /// Attach a row key.
    pub fn key(mut self, key: impl Into<String>) -> SyncResult<Self> {
        self.key = Some(RowKey::new(key)?);
        Ok(self)
    }

    /// Attach an already validated row key.
    pub fn row_key(mut self, key: RowKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Attach a base row version for conflict detection.
    pub fn base_version(mut self, version: impl Into<String>) -> SyncResult<Self> {
        self.base_version = Some(RowVersion::new(version)?);
        Ok(self)
    }

    /// Attach an already validated base row version.
    pub fn row_version(mut self, version: RowVersion) -> Self {
        self.base_version = Some(version);
        self
    }

    /// Attach an optional already validated base row version.
    pub fn base_row_version(mut self, version: Option<RowVersion>) -> Self {
        self.base_version = version;
        self
    }
}

/// Client mutation before the local store reserves a durable mutation id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct ClientMutationDraft<M> {
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub base_version: Option<RowVersion>,
    pub payload: M,
}

impl<M> ClientMutationDraft<M> {
    /// Build a draft mutation for the given operation.
    pub fn new(op: SyncOp, payload: M) -> Self {
        Self {
            key: None,
            op,
            base_version: None,
            payload,
        }
    }

    /// Build an upsert draft.
    pub fn upsert(payload: M) -> Self {
        Self::new(SyncOp::Upsert, payload)
    }

    /// Build a delete draft.
    pub fn delete(payload: M) -> Self {
        Self::new(SyncOp::Delete, payload)
    }

    /// Build a reset draft.
    pub fn reset(payload: M) -> Self {
        Self::new(SyncOp::Reset, payload)
    }

    /// Build a draft mutation scoped to a row.
    pub fn for_row(op: SyncOp, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::new(op, payload).key(key)
    }

    /// Build an upsert draft scoped to a row.
    pub fn upsert_row(key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::upsert(payload).key(key)
    }

    /// Build a delete draft scoped to a row.
    pub fn delete_row(key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::delete(payload).key(key)
    }

    /// Attach a row key.
    pub fn key(mut self, key: impl Into<String>) -> SyncResult<Self> {
        self.key = Some(RowKey::new(key)?);
        Ok(self)
    }

    /// Attach an already validated row key.
    pub fn row_key(mut self, key: RowKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Attach a base row version for conflict detection.
    pub fn base_version(mut self, version: impl Into<String>) -> SyncResult<Self> {
        self.base_version = Some(RowVersion::new(version)?);
        Ok(self)
    }

    /// Attach an already validated base row version.
    pub fn row_version(mut self, version: RowVersion) -> Self {
        self.base_version = Some(version);
        self
    }

    /// Attach an optional already validated base row version.
    pub fn base_row_version(mut self, version: Option<RowVersion>) -> Self {
        self.base_version = version;
        self
    }

    /// Convert this draft into a wire mutation after an id is reserved.
    pub fn with_id(self, id: MutationId) -> ClientMutation<M> {
        ClientMutation {
            id,
            key: self.key,
            op: self.op,
            base_version: self.base_version,
            payload: self.payload,
        }
    }
}

/// Push request for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct SyncPushRequest<M> {
    pub protocol: String,
    pub stream: SyncStreamName,
    #[serde(default)]
    pub mutations: Vec<ClientMutation<M>>,
    /// Application-level schema version the CLIENT encoded these
    /// mutations against. The server compares against
    /// `SyncStreamSource::schema_version()`; on mismatch the source's
    /// `migrate_payload` is invoked per mutation. A source that hasn't
    /// registered a migrator rejects each mutation with
    /// `SyncError::SchemaMigration`. Defaults to `1` so an old client
    /// that doesn't send the field is treated as v1, matching Batch 1's
    /// default for `SyncStreamSource::schema_version`.
    #[serde(default = "default_schema_version_one")]
    pub schema_version: u32,
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
            schema_version: default_schema_version_one(),
        }
    }

    /// Set the application-level schema version the mutations are
    /// encoded under. Generated client helpers fill this from the
    /// resource's compile-time `SCHEMA_VERSION` constant.
    pub fn with_schema_version(mut self, schema_version: u32) -> Self {
        self.schema_version = schema_version;
        self
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<SyncCollectionName>,
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
            collection: None,
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
    fn open_request_serializes_typed_client_id() {
        let request = SyncOpenRequest::new([SyncStreamName::new("posts").unwrap()])
            .client_id(SyncDeviceId::new("device_abc").unwrap());

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["client_id"], "device_abc");
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
    fn sync_row_helpers_attach_version_and_flags() {
        let version = RowVersion::new("row_1").unwrap();
        let row = SyncRow::new("post_1", "hello".to_string())
            .unwrap()
            .row_version(version.clone())
            .pending(true)
            .conflict(true);

        assert_eq!(row.version, Some(version));
        assert!(row.pending);
        assert!(row.conflict);
    }

    #[test]
    fn client_mutation_helpers_build_row_scoped_mutations() {
        let id = MutationId::new("device_abc:1").unwrap();
        let version = RowVersion::new("row_1").unwrap();

        let mutation = ClientMutation::upsert_row(id.clone(), "post_1", "payload")
            .unwrap()
            .base_row_version(Some(version.clone()));

        assert_eq!(mutation.id, id);
        assert_eq!(mutation.key.unwrap().as_str(), "post_1");
        assert_eq!(mutation.op, SyncOp::Upsert);
        assert_eq!(mutation.base_version, Some(version));
        assert_eq!(mutation.payload, "payload");
    }

    #[test]
    fn client_mutation_draft_helpers_build_row_scoped_mutations() {
        let id = MutationId::new("device_abc:1").unwrap();
        let version = RowVersion::new("row_1").unwrap();

        let mutation = ClientMutationDraft::delete_row("post_1", ())
            .unwrap()
            .base_row_version(Some(version.clone()))
            .with_id(id.clone());

        assert_eq!(mutation.id, id);
        assert_eq!(mutation.key.unwrap().as_str(), "post_1");
        assert_eq!(mutation.op, SyncOp::Delete);
        assert_eq!(mutation.base_version, Some(version));
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
