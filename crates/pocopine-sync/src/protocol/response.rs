use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    MutationId, RowKey, SYNC_PROTOCOL_V1, StreamParams, SyncCollectionName, SyncCursor, SyncRow,
    SyncScope, SyncStreamName, SyncTombstone, deserialize_params_null_as_default,
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
    /// Opaque principal scope the server derived from this request's
    /// authenticated context (see [`SyncStreamSource::scope`]). The
    /// client compares it against the scope persisted with the stream's
    /// durable compartment: a mismatch means a *different* principal is
    /// now answering for the same `(stream, params)` subscription, and
    /// the client redirects to a scope-qualified compartment instead of
    /// letting the new principal's truth overwrite the old one's cache.
    /// `None` (the wire default — old servers, unscoped sources) leaves
    /// the guard inert.
    ///
    /// [`SyncStreamSource::scope`]: crate::SyncStreamSource::scope
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<SyncScope>,
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
    /// Positive deletions the source still retains (see
    /// [`SyncTombstone`]). In `Snapshot` mode these disambiguate a
    /// row's absence — tombstoned keys were deleted at the authority;
    /// other absent keys are unexplained. In `Incremental` mode they
    /// carry deletions the same way a `SyncChange` with
    /// `SyncOp::Delete` would. Old servers never set the field and
    /// old clients ignore it — `#[serde(default)]` keeps both
    /// directions compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstones: Vec<SyncTombstone>,
    pub cursor: Option<SyncCursor>,
    pub has_more: bool,
    /// Principal scope of the authenticated context that produced this
    /// response. Same contract as [`SyncOpenStream::scope`]; stamped by
    /// the pull handler on every response so a session that changes
    /// principals mid-subscription (expiry to guest, tenant switch) is
    /// caught at settle time, not just at `/open`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<SyncScope>,
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
            tombstones: Vec::new(),
            cursor,
            has_more: false,
            scope: None,
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
            tombstones: Vec::new(),
            cursor,
            has_more: false,
            scope: None,
        }
    }

    /// Attach positive deletion records to this response.
    pub fn with_tombstones(mut self, tombstones: Vec<SyncTombstone>) -> Self {
        self.tombstones = tombstones;
        self
    }

    /// Attach the responding principal's scope to this response.
    pub fn with_scope(mut self, scope: Option<SyncScope>) -> Self {
        self.scope = scope;
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

    #[test]
    fn open_stream_scope_is_backwards_compatible() {
        // A pre-scope server response deserializes with scope = None
        // (the guard stays inert against old servers).
        let json = serde_json::json!({
            "stream": "posts",
            "collection": "posts",
            "cursor": null,
        });
        let stream: SyncOpenStream = serde_json::from_value(json).unwrap();
        assert!(stream.scope.is_none());

        // A scoped response round-trips, and serialization skips the
        // field when None so old clients never see an unknown key
        // for the common unscoped case.
        let json = serde_json::json!({
            "stream": "posts",
            "collection": "posts",
            "cursor": null,
            "scope": "user:alice",
        });
        let stream: SyncOpenStream = serde_json::from_value(json).unwrap();
        assert_eq!(
            stream.scope,
            Some(super::super::SyncScope::new("user:alice").unwrap())
        );
        let unscoped = SyncOpenStream {
            scope: None,
            ..stream
        };
        let value = serde_json::to_value(&unscoped).unwrap();
        assert!(value.get("scope").is_none());
    }

    #[test]
    fn pull_response_tombstones_and_scope_are_backwards_compatible() {
        use super::super::{RowVersion, SyncScope, SyncTombstone};

        // A pre-tombstone server response (no `tombstones`, no
        // `scope`) deserializes to the empty defaults.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "collection": "posts",
            "mode": "snapshot",
            "rows": [],
            "cursor": null,
            "has_more": false,
        });
        let response: SyncPullResponse<String> = serde_json::from_value(json).unwrap();
        assert!(response.tombstones.is_empty());
        assert!(response.scope.is_none());

        // Tombstones + scope round-trip through the wire shape.
        let response = SyncPullResponse::<String>::snapshot(
            SyncStreamName::new("posts").unwrap(),
            SyncCollectionName::new("posts").unwrap(),
            Vec::new(),
            None,
        )
        .with_tombstones(vec![
            SyncTombstone::new("post_1").unwrap(),
            SyncTombstone::new("post_2").unwrap().version("v9").unwrap(),
        ])
        .with_scope(Some(SyncScope::new("user:alice").unwrap()));
        let value = serde_json::to_value(&response).unwrap();
        let decoded: SyncPullResponse<String> = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.tombstones.len(), 2);
        assert_eq!(decoded.tombstones[0].key.as_str(), "post_1");
        assert!(decoded.tombstones[0].version.is_none());
        assert_eq!(
            decoded.tombstones[1].version,
            Some(RowVersion::new("v9").unwrap())
        );
        assert_eq!(decoded.scope, Some(SyncScope::new("user:alice").unwrap()));

        // Empty tombstones + no scope serialize to a wire body with
        // NEITHER key present — an old client deserializing the
        // common case never sees the new fields at all.
        let bare = SyncPullResponse::<String>::snapshot(
            SyncStreamName::new("posts").unwrap(),
            SyncCollectionName::new("posts").unwrap(),
            Vec::new(),
            None,
        );
        let value = serde_json::to_value(&bare).unwrap();
        assert!(value.get("tombstones").is_none());
        assert!(value.get("scope").is_none());
    }
}
