use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    MutationId, RowKey, SYNC_PROTOCOL_V1, StreamParams, SyncCollectionName, SyncCursor, SyncRow,
    SyncStreamName, deserialize_params_null_as_default,
};

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
    /// response just ignores it (serde drops unknown fields). Explicit
    /// JSON `null` is also coerced to the default for clients (TS,
    /// Python) that emit `null` for unset fields.
    #[serde(
        default = "default_schema_version_one",
        deserialize_with = "deserialize_schema_version_default_one"
    )]
    pub schema_version: u32,
    /// Params the server accepted for this subscription, echoed back
    /// so the client can confirm what the server is actually serving.
    /// Empty when the subscription is not parameterized; the field is
    /// `#[serde(default)]` for backwards-compat with pre-RFC-085 servers.
    /// Explicit JSON `null` is also coerced to empty for clients (TS,
    /// Python) that emit `null` for unset fields.
    #[serde(
        default,
        deserialize_with = "deserialize_params_null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub params: StreamParams,
}

/// Default schema version when the wire field is missing or the source
/// doesn't override the trait default. `1` means "first version of the
/// schema" — apps that have never bumped never observe this field.
pub fn default_schema_version_one() -> u32 {
    1
}

/// Custom deserializer for the wire `schema_version` field. Accepts:
///
/// * the field being absent (handled by `#[serde(default = ...)]` —
///   this deserializer isn't called),
/// * the field being explicit `null` (TypeScript / Python clients
///   that emit `null` for unset fields), AND
/// * a normal `u32` value.
///
/// Both `missing` and `null` collapse to
/// [`default_schema_version_one`]. Without this, an explicit
/// `"schema_version": null` would fail deserialization with `invalid
/// type: null, expected u32`, which a non-Rust client cannot
/// distinguish from a network error.
pub(crate) fn deserialize_schema_version_default_one<'de, D>(
    deserializer: D,
) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<u32>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_else(default_schema_version_one))
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

/// Pull response for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncPullResponse<T> {
    pub protocol: String,
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub mode: super::SyncPullMode,
    #[serde(default)]
    pub rows: Vec<SyncRow<T>>,
    #[serde(default)]
    pub changes: Vec<super::SyncChange<T>>,
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
            mode: super::SyncPullMode::Snapshot,
            rows,
            changes: Vec::new(),
            cursor,
            has_more: false,
        }
    }

    pub fn incremental(
        stream: SyncStreamName,
        collection: SyncCollectionName,
        changes: Vec<super::SyncChange<T>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            collection,
            mode: super::SyncPullMode::Incremental,
            rows: Vec::new(),
            changes,
            cursor,
            has_more: false,
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
///
/// **Ordering:** `SyncPushResponse::rejected` is NOT guaranteed to
/// match the request's mutation order. The server may surface
/// schema-migration rejections after source-side rejections (or
/// vice versa). Consumers should correlate by `mutation_id`, not by
/// index.
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
    fn open_stream_schema_version_accepts_missing_and_explicit_null() {
        // Missing field — the existing `#[serde(default)]` already
        // handled this; lock the behaviour against future changes.
        let json = serde_json::json!({
            "stream": "posts",
            "collection": "posts",
            "cursor": null,
        });
        let stream: SyncOpenStream = serde_json::from_value(json).unwrap();
        assert_eq!(stream.schema_version, 1);

        // Explicit JSON null — the new `deserialize_with` coerces it
        // to the default. Without this, non-Rust clients emitting
        // `{"schema_version": null}` would fail with `invalid type:
        // null, expected u32`.
        let json = serde_json::json!({
            "stream": "posts",
            "collection": "posts",
            "cursor": null,
            "schema_version": null,
        });
        let stream: SyncOpenStream = serde_json::from_value(json).unwrap();
        assert_eq!(stream.schema_version, 1);

        // A concrete value still round-trips.
        let json = serde_json::json!({
            "stream": "posts",
            "collection": "posts",
            "cursor": null,
            "schema_version": 7,
        });
        let stream: SyncOpenStream = serde_json::from_value(json).unwrap();
        assert_eq!(stream.schema_version, 7);
    }
}
