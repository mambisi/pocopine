use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{SyncError, SyncResult};

/// Current sync protocol identifier.
pub const SYNC_PROTOCOL_V1: &str = "pocopine.sync.v1";

/// Default sync endpoint prefix mounted by the server plugin.
pub const SYNC_ENDPOINT_PREFIX: &str = "/__pocopine/sync/v1";
/// Open endpoint path.
pub const SYNC_OPEN_PATH: &str = "/__pocopine/sync/v1/open";
/// Pull endpoint path.
pub const SYNC_PULL_PATH: &str = "/__pocopine/sync/v1/pull";
/// Push endpoint path.
pub const SYNC_PUSH_PATH: &str = "/__pocopine/sync/v1/push";

fn validate_token(field: &'static str, value: String) -> SyncResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed != value || value.chars().any(char::is_control) {
        return Err(SyncError::invalid_value(field, value));
    }
    Ok(value)
}

macro_rules! opaque_string_type {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
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

opaque_string_type!(SyncShapeName, "shape", "Server-registered sync shape name.");
opaque_string_type!(
    SyncCollectionName,
    "collection",
    "Public collection name exposed to the client."
);
opaque_string_type!(SyncCursor, "cursor", "Opaque server-issued sync cursor.");
opaque_string_type!(
    RowKey,
    "row key",
    "Public row identity inside one sync shape."
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

/// Live query tag used to wake clients for a sync shape.
pub fn sync_shape_tag(shape: &str) -> String {
    format!("sync:shape:{shape}")
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
    pub shape: SyncShapeName,
    pub collection: SyncCollectionName,
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub row: Option<SyncRow<T>>,
    pub cursor: SyncCursor,
}

/// Open one or more shapes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenRequest {
    pub protocol: String,
    #[serde(default)]
    pub client_id: Option<String>,
    pub shapes: Vec<SyncShapeName>,
}

impl SyncOpenRequest {
    pub fn new(shapes: impl IntoIterator<Item = SyncShapeName>) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            client_id: None,
            shapes: shapes.into_iter().collect(),
        }
    }
}

/// Shape accepted by an open response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenShape {
    pub shape: SyncShapeName,
    pub collection: SyncCollectionName,
    pub cursor: Option<SyncCursor>,
}

/// Open response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenResponse {
    pub protocol: String,
    pub shapes: Vec<SyncOpenShape>,
}

impl SyncOpenResponse {
    pub fn new(shapes: Vec<SyncOpenShape>) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            shapes,
        }
    }
}

/// Pull request for one shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncPullRequest {
    pub protocol: String,
    pub shape: SyncShapeName,
    pub cursor: Option<SyncCursor>,
    pub limit: u32,
}

impl SyncPullRequest {
    pub fn new(shape: SyncShapeName) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            shape,
            cursor: None,
            limit: 500,
        }
    }

    pub fn cursor(mut self, cursor: Option<SyncCursor>) -> Self {
        self.cursor = cursor;
        self
    }
}

/// Pull response for one shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncPullResponse<T> {
    pub protocol: String,
    pub shape: SyncShapeName,
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
        shape: SyncShapeName,
        collection: SyncCollectionName,
        rows: Vec<SyncRow<T>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            shape,
            collection,
            mode: SyncPullMode::Snapshot,
            rows,
            changes: Vec::new(),
            cursor,
            has_more: false,
        }
    }

    pub fn incremental(
        shape: SyncShapeName,
        collection: SyncCollectionName,
        changes: Vec<SyncChange<T>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            shape,
            collection,
            mode: SyncPullMode::Incremental,
            rows: Vec::new(),
            changes,
            cursor,
            has_more: false,
        }
    }
}

/// Client mutation envelope. The first slice defines the wire shape;
/// concrete mutation application belongs to shape sources.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct ClientMutation<M> {
    pub id: MutationId,
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub base_version: Option<RowVersion>,
    pub payload: M,
}

/// Push request for one shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct SyncPushRequest<M> {
    pub protocol: String,
    pub shape: SyncShapeName,
    #[serde(default)]
    pub mutations: Vec<ClientMutation<M>>,
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

/// Push response for one shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncPushResponse<T> {
    pub protocol: String,
    pub shape: SyncShapeName,
    #[serde(default)]
    pub accepted: Vec<MutationId>,
    #[serde(default)]
    pub rows: Vec<SyncRow<T>>,
    #[serde(default)]
    pub conflicts: Vec<SyncConflict<T>>,
    pub cursor: Option<SyncCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_shape_names() {
        assert!(SyncShapeName::new("posts_for_tenant").is_ok());
        assert!(SyncShapeName::new("").is_err());
        assert!(SyncShapeName::new(" posts").is_err());
        assert!(SyncShapeName::new("posts\nbad").is_err());
    }

    #[test]
    fn sync_shape_tag_is_stable() {
        assert_eq!(
            sync_shape_tag("posts_for_tenant"),
            "sync:shape:posts_for_tenant"
        );
    }
}
