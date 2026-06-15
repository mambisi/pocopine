//! Durable persistence: the `CollabStore` trait and an in-memory reference
//! implementation.
//!
//! The durable database is the user's preference, so the contract is
//! deliberately DB-agnostic — a pure versioned blob-by-document store that
//! assumes nothing SQL-specific. It stores only the compacted snapshot, its
//! state vector, and the fan-out cursor; the document registry, ownership, and
//! permissions belong to the app's own database. See
//! `docs/internal/collab-persistence.md`.
//!
//! The key is the document's **topic string** (`collab:{doc_hash}`, via
//! [`CollabDoc::topic`](super::doc::CollabDoc::topic)). The topic — not the full
//! [`CollabDoc`](super::doc::CollabDoc) — is what both the convergence apply
//! loop and the compaction path actually hold (the hash is one-way), so the
//! store keys by it directly.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;

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

/// Durable, DB-agnostic blob-by-document persistence, keyed by the document's
/// topic string (`collab:{doc_hash}`).
///
/// Implement this for your database; the framework ships [`MemoryCollabStore`]
/// plus reference adapters, and never forces a specific engine. The store does
/// not expose registry/list queries (those belong to the app's own schema). A
/// process loads a snapshot when it first opens a topic and checkpoints the
/// folded document back periodically, so the durable base survives restart and
/// fan-out retention eviction.
#[async_trait]
pub trait CollabStore: Send + Sync + 'static {
    /// Load a document's latest snapshot, or `None` if it has never been saved.
    async fn load_snapshot(&self, doc_key: &str) -> CollabResult<Option<CollabSnapshot>>;

    /// Persist a document's compacted snapshot — but ONLY if it is fresher than
    /// the stored one (`snapshot.last_seq` strictly greater). The cursor must be
    /// monotonic: replicas share one fan-out cursor and a lagging process can
    /// otherwise overwrite a newer checkpoint with an older blob, rolling
    /// `last_seq` backward so a restart resumes into an already-evicted gap. A
    /// SQL adapter expresses this as a conditional upsert
    /// (`... ON CONFLICT DO UPDATE WHERE excluded.last_seq > last_seq`).
    async fn save_snapshot(&self, doc_key: &str, snapshot: CollabSnapshot) -> CollabResult<()>;
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
    async fn load_snapshot(&self, doc_key: &str) -> CollabResult<Option<CollabSnapshot>> {
        // std::Mutex held without crossing an .await — safe in async.
        let map = self
            .snapshots
            .lock()
            .map_err(|_| CollabError::store("memory store mutex poisoned"))?;
        Ok(map.get(doc_key).cloned())
    }

    async fn save_snapshot(&self, doc_key: &str, snapshot: CollabSnapshot) -> CollabResult<()> {
        let mut map = self
            .snapshots
            .lock()
            .map_err(|_| CollabError::store("memory store mutex poisoned"))?;
        // Monotonic: ignore a checkpoint that does not advance the cursor.
        match map.get(doc_key) {
            Some(existing) if existing.last_seq >= snapshot.last_seq => {}
            _ => {
                map.insert(doc_key.to_string(), snapshot);
            }
        }
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
        let key = "collab:doc1";

        assert_eq!(store.load_snapshot(key).await.unwrap(), None);

        store.save_snapshot(key, snapshot(b"v1", 3)).await.unwrap();
        assert_eq!(
            store.load_snapshot(key).await.unwrap(),
            Some(snapshot(b"v1", 3))
        );

        store.save_snapshot(key, snapshot(b"v2", 7)).await.unwrap();
        assert_eq!(
            store.load_snapshot(key).await.unwrap(),
            Some(snapshot(b"v2", 7))
        );
    }

    #[tokio::test]
    async fn documents_are_isolated() {
        let store = MemoryCollabStore::new();
        store
            .save_snapshot("collab:a", snapshot(b"a", 1))
            .await
            .unwrap();
        assert_eq!(store.load_snapshot("collab:b").await.unwrap(), None);
        assert_eq!(
            store
                .load_snapshot("collab:a")
                .await
                .unwrap()
                .unwrap()
                .last_seq,
            1
        );
    }

    #[tokio::test]
    async fn ignores_a_stale_or_equal_checkpoint() {
        let store = MemoryCollabStore::new();
        let key = "collab:doc";
        store
            .save_snapshot(key, snapshot(b"fresh", 10))
            .await
            .unwrap();

        // A lagging replica writing an OLDER cursor must not roll it back.
        store
            .save_snapshot(key, snapshot(b"stale", 5))
            .await
            .unwrap();
        let got = store.load_snapshot(key).await.unwrap().unwrap();
        assert_eq!(got.last_seq, 10);
        assert_eq!(got.blob, Bytes::from_static(b"fresh"));

        // An equal cursor is also ignored (no churn).
        store
            .save_snapshot(key, snapshot(b"equal", 10))
            .await
            .unwrap();
        assert_eq!(
            store.load_snapshot(key).await.unwrap().unwrap().blob,
            Bytes::from_static(b"fresh")
        );

        // A newer cursor still advances.
        store
            .save_snapshot(key, snapshot(b"newer", 11))
            .await
            .unwrap();
        assert_eq!(
            store.load_snapshot(key).await.unwrap().unwrap().last_seq,
            11
        );
    }
}
