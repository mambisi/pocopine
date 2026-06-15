//! Durable persistence: the `CollabStore` trait and an in-memory reference
//! implementation.
//!
//! The durable database is the user's preference, so the contract is
//! deliberately DB-agnostic — a pure versioned blob-by-document store that
//! assumes nothing SQL-specific. It stores only the compacted snapshot, its
//! state vector, and the fan-out cursor; the document registry, ownership, and
//! permissions belong to the app's own database. See
//! `docs/internal/collab-persistence.md`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;

use super::doc::CollabDoc;
use super::error::{CollabError, CollabResult};

/// The persisted state of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollabSnapshot {
    /// The compacted document as a yrs full-state update
    /// (`CollabDocument::full_update`).
    pub blob: Bytes,
    /// The yrs state vector of `blob` (small; lets the server reason about the
    /// snapshot without decoding the whole blob).
    pub state_vector: Bytes,
    /// The `pocopine-realtime` topic cursor this snapshot is current to (where
    /// the compaction worker resumes folding the live update log).
    pub last_seq: u64,
}

/// Durable, DB-agnostic blob-by-document persistence.
///
/// Implement this for your database; the framework ships [`MemoryCollabStore`]
/// plus reference adapters, and never forces a specific engine. The store is
/// keyed by the document — it does not expose registry/list queries (those
/// belong to the app's own schema). The compaction worker is the only writer;
/// the `web` process is a reader (loading a snapshot to serve a join).
#[async_trait]
pub trait CollabStore: Send + Sync + 'static {
    /// Load a document's latest snapshot, or `None` if it has never been saved.
    async fn load_snapshot(&self, doc: &CollabDoc) -> CollabResult<Option<CollabSnapshot>>;

    /// Persist a document's compacted snapshot (overwriting any prior one).
    async fn save_snapshot(&self, doc: &CollabDoc, snapshot: CollabSnapshot) -> CollabResult<()>;
}

/// In-process [`CollabStore`] for tests and single-node dev (no database).
#[derive(Default)]
pub struct MemoryCollabStore {
    snapshots: Mutex<HashMap<String, CollabSnapshot>>,
}

impl MemoryCollabStore {
    /// An empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CollabStore for MemoryCollabStore {
    async fn load_snapshot(&self, doc: &CollabDoc) -> CollabResult<Option<CollabSnapshot>> {
        // std::Mutex held without crossing an .await — safe in async.
        let map = self
            .snapshots
            .lock()
            .map_err(|_| CollabError::store("memory store mutex poisoned"))?;
        Ok(map.get(&doc.doc_hash()).cloned())
    }

    async fn save_snapshot(&self, doc: &CollabDoc, snapshot: CollabSnapshot) -> CollabResult<()> {
        let mut map = self
            .snapshots
            .lock()
            .map_err(|_| CollabError::store("memory store mutex poisoned"))?;
        map.insert(doc.doc_hash(), snapshot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(blob: &[u8], seq: u64) -> CollabSnapshot {
        CollabSnapshot {
            blob: Bytes::copy_from_slice(blob),
            state_vector: Bytes::from_static(b"sv"),
            last_seq: seq,
        }
    }

    #[tokio::test]
    async fn round_trips_and_overwrites() {
        let store = MemoryCollabStore::new();
        let doc = CollabDoc::new("app", "docs", "1", "main");

        assert_eq!(store.load_snapshot(&doc).await.unwrap(), None);

        store.save_snapshot(&doc, snapshot(b"v1", 3)).await.unwrap();
        assert_eq!(
            store.load_snapshot(&doc).await.unwrap(),
            Some(snapshot(b"v1", 3))
        );

        store.save_snapshot(&doc, snapshot(b"v2", 7)).await.unwrap();
        assert_eq!(
            store.load_snapshot(&doc).await.unwrap(),
            Some(snapshot(b"v2", 7))
        );
    }

    #[tokio::test]
    async fn documents_are_isolated() {
        let store = MemoryCollabStore::new();
        let a = CollabDoc::new("app", "docs", "a", "main");
        let b = CollabDoc::new("app", "docs", "b", "main");
        store.save_snapshot(&a, snapshot(b"a", 1)).await.unwrap();
        assert_eq!(store.load_snapshot(&b).await.unwrap(), None);
        assert_eq!(store.load_snapshot(&a).await.unwrap().unwrap().last_seq, 1);
    }
}
