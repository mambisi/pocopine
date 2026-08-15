use serde::{Deserialize, Serialize};

use crate::SyncResult;

use super::{MutationId, RowKey, RowVersion, SyncCollectionName, SyncCursor, SyncStreamName};

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

/// Why the server answered a CURSORED pull with a full snapshot
/// instead of an incremental batch. Stamped on `SyncPullResponse`
/// so the client knows the snapshot is a deliberate, authoritative
/// replacement — its local rows are KNOWN stale, and absences must
/// never reach absence-interpreting recovery policy (sync-query
/// classifies them `EvictionReason::StaleResync`, not `Unexplained`).
///
/// This is the loud half of the SPORC-style truncation watermark:
/// "too old to serve incrementally" is an explicit protocol outcome,
/// not a silent degradation into ambiguous absences.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncResyncReason {
    /// The client's cursor predates the change log's oldest retained
    /// position — the history between them has been GC'd, so the
    /// only correct answer is a full snapshot the client adopts
    /// wholesale (wipe-and-resync semantics).
    CursorTruncated,
}

/// Machine-readable class attached to a [`SyncRejectedMutation`]
/// when the rejection means more than "this one write failed".
/// `None` (the wire default) is an ordinary rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncRejectCode {
    /// The mutation's per-device counter regressed below a counter
    /// the server already accepted for the same device — the client
    /// identity is duplicated or restored from backup (two tabs
    /// sharing one device id, a restored IndexedDB). The client's
    /// durable identity can no longer be trusted; recovery is a
    /// fresh device identity + wipe-and-resync, not a retry.
    IdentityFault,
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

/// Positive deletion record for one row, carried on pull responses.
///
/// A tombstone is the authority saying "this key existed and was
/// deleted" — as opposed to a row merely being absent from a snapshot,
/// which is ambiguous (deleted? no longer matching the query's filter?
/// beyond the snapshot limit? lost at the backend?). Clients apply
/// tombstoned deletes with confidence; un-tombstoned absences stay a
/// policy decision (see `EvictionReason` in `pocopine-sync-query`).
///
/// Retention is the source's business: keep tombstones for a window
/// comfortably longer than the longest expected client offline period,
/// then GC. A client that was offline past the window sees the row as
/// an unexplained absence rather than a confirmed delete — degraded,
/// never wrong.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncTombstone {
    pub key: RowKey,
    /// Version the row had when it was deleted, when the source tracks
    /// one. Informational — equality on `key` is what drives the
    /// client-side delete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<RowVersion>,
}

impl SyncTombstone {
    pub fn new(key: impl Into<String>) -> SyncResult<Self> {
        Ok(Self {
            key: RowKey::new(key)?,
            version: None,
        })
    }

    /// Attach the deleted row's last version.
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
    /// The client mutation that produced this change, when the
    /// change log recorded one. This is the feed echo (SPORC §3.1
    /// step 6): a client that observes its OWN mutation id here
    /// knows the write committed — even if the push response that
    /// would have said so was lost — and retires the matching
    /// pending overlay through the type-erased path. `None` for
    /// changes with no recorded originator (server-side writes,
    /// imports, log impls that don't track it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MutationId>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn endpoint_paths_share_prefix() {
        assert!(SYNC_OPEN_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_OPEN_PATH.ends_with("/open"));
        assert!(SYNC_PULL_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_PULL_PATH.ends_with("/pull"));
        assert!(SYNC_PUSH_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_PUSH_PATH.ends_with("/push"));
    }
}
