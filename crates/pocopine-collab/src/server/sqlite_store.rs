//! A reference durable [`CollabStore`] backed by SQLite (the `sqlite` feature).
//!
//! The framework ships this so a single-DB deployment works out of the box; apps
//! with their own database implement [`CollabStore`] against it instead. One
//! `collab_snapshots` table keyed by the document topic, with the monotonic
//! conditional upsert the trait's contract requires — a lagging replica can never
//! roll the durable cursor backward.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use rusqlite::Connection;

use super::store::{CollabSnapshot, CollabStore};
use crate::error::{CollabError, CollabResult};

/// A SQLite-backed [`CollabStore`]. The connection is serialized behind a `Mutex`
/// (snapshots are small and writes are infrequent — a checkpoint every N folded
/// updates), so the brief synchronous query never crosses an `.await`.
pub struct SqliteCollabStore {
    conn: Mutex<Connection>,
}

impl SqliteCollabStore {
    /// Open (creating if absent) a store at `path`.
    pub fn open(path: impl AsRef<Path>) -> CollabResult<Self> {
        let conn = Connection::open(path).map_err(|err| CollabError::store(err.to_string()))?;
        // WAL gives atomic, crash-safe commits (a checkpoint either lands whole
        // or not at all) and lets a reader load while a writer checkpoints — the
        // standard durable config in this workspace. Irrelevant for `:memory:`,
        // so only file-backed connections enable it.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|err| CollabError::store(err.to_string()))?;
        // synchronous = FULL fsyncs every commit, NOT just at WAL-checkpoint
        // boundaries (NORMAL's default). This matters because the apply loop
        // trims the fan-out to a cursor as soon as `save_snapshot` returns Ok:
        // under NORMAL a power loss could revert that not-yet-fsynced snapshot
        // while the fan-out stayed trimmed past it, opening an evicted gap. FULL
        // makes the save durable BEFORE the trim. The cost — one fsync per
        // checkpoint — is negligible at the checkpoint cadence (every N updates).
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(|err| CollabError::store(err.to_string()))?;
        Self::from_connection(conn)
    }

    /// An ephemeral in-memory store (tests / single-process dev).
    pub fn in_memory() -> CollabResult<Self> {
        let conn =
            Connection::open_in_memory().map_err(|err| CollabError::store(err.to_string()))?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> CollabResult<Self> {
        // Retry briefly instead of failing immediately when another connection
        // holds the write lock (e.g. two processes sharing one file, or WAL
        // checkpointing) — `SQLITE_BUSY` should not surface as a lost checkpoint.
        conn.busy_timeout(Duration::from_millis(5_000))
            .map_err(|err| CollabError::store(err.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS collab_snapshots (
                 doc_key      TEXT PRIMARY KEY,
                 blob         BLOB NOT NULL,
                 state_vector BLOB NOT NULL,
                 last_seq     INTEGER NOT NULL
             );",
        )
        .map_err(|err| CollabError::store(err.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Lock the connection, recovering a poisoned mutex rather than wedging the
    /// store forever: each method runs one self-contained statement, so a panic
    /// elsewhere leaves no half-applied transaction to fear, and SQLite's own
    /// atomicity guards the write.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl CollabStore for SqliteCollabStore {
    async fn load_snapshot(&self, doc_key: &str) -> CollabResult<Option<CollabSnapshot>> {
        let conn = self.lock();
        let row = conn.query_row(
            "SELECT blob, state_vector, last_seq FROM collab_snapshots WHERE doc_key = ?1",
            [doc_key],
            |row| {
                let blob: Vec<u8> = row.get(0)?;
                let state_vector: Vec<u8> = row.get(1)?;
                let last_seq: i64 = row.get(2)?;
                Ok((blob, state_vector, last_seq))
            },
        );
        match row {
            Ok((blob, state_vector, last_seq)) => Ok(Some(CollabSnapshot {
                blob: Bytes::from(blob),
                state_vector: Bytes::from(state_vector),
                last_seq: last_seq as u64,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(CollabError::store(err.to_string())),
        }
    }

    async fn save_snapshot(&self, doc_key: &str, snapshot: CollabSnapshot) -> CollabResult<()> {
        let conn = self.lock();
        // SQLite stores INTEGER as i64; the fan-out cursor is u64. A real cursor
        // (a monotonic publish counter) never approaches i64::MAX. Refuse the
        // impossible overflow rather than clamp it: clamping would store a cursor
        // BELOW the one the handler trims the fan-out to (the handler trims at the
        // true u64 seq), so a restart would resume into an evicted gap. Returning
        // an error also skips the trim — `checkpoint_and_trim` only trims on a
        // successful save — keeping the store and the fan-out consistent.
        let last_seq = i64::try_from(snapshot.last_seq)
            .map_err(|_| CollabError::store("snapshot cursor exceeds i64::MAX"))?;
        // Monotonic upsert: only overwrite when the new cursor strictly advances,
        // so a lagging replica cannot roll `last_seq` backward into an evicted gap.
        conn.execute(
            "INSERT INTO collab_snapshots (doc_key, blob, state_vector, last_seq)
                 VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(doc_key) DO UPDATE SET
                 blob         = excluded.blob,
                 state_vector = excluded.state_vector,
                 last_seq     = excluded.last_seq
             WHERE excluded.last_seq > collab_snapshots.last_seq",
            rusqlite::params![
                doc_key,
                snapshot.blob.as_ref(),
                snapshot.state_vector.as_ref(),
                last_seq,
            ],
        )
        .map_err(|err| CollabError::store(err.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(blob: &'static [u8], last_seq: u64) -> CollabSnapshot {
        CollabSnapshot {
            blob: Bytes::from_static(blob),
            state_vector: Bytes::from_static(b"sv"),
            last_seq,
        }
    }

    #[tokio::test]
    async fn save_then_load_round_trips() {
        let store = SqliteCollabStore::in_memory().unwrap();
        assert!(store.load_snapshot("collab:doc").await.unwrap().is_none());

        let s = snap(b"document-blob", 7);
        store.save_snapshot("collab:doc", s.clone()).await.unwrap();
        assert_eq!(store.load_snapshot("collab:doc").await.unwrap(), Some(s));
    }

    #[tokio::test]
    async fn save_is_monotonic_in_last_seq() {
        let store = SqliteCollabStore::in_memory().unwrap();
        store.save_snapshot("d", snap(b"newer", 10)).await.unwrap();
        // A stale checkpoint (lower cursor) must NOT overwrite the fresher one.
        store.save_snapshot("d", snap(b"older", 3)).await.unwrap();

        let loaded = store.load_snapshot("d").await.unwrap().unwrap();
        assert_eq!(loaded.last_seq, 10);
        assert_eq!(loaded.blob, Bytes::from_static(b"newer"));

        // A strictly-newer checkpoint does overwrite.
        store.save_snapshot("d", snap(b"newest", 11)).await.unwrap();
        assert_eq!(
            store.load_snapshot("d").await.unwrap().unwrap().blob,
            Bytes::from_static(b"newest")
        );
    }

    #[tokio::test]
    async fn topics_are_isolated() {
        let store = SqliteCollabStore::in_memory().unwrap();
        store
            .save_snapshot("collab:a", snap(b"a", 1))
            .await
            .unwrap();
        store
            .save_snapshot("collab:b", snap(b"b", 1))
            .await
            .unwrap();
        assert_eq!(
            store.load_snapshot("collab:a").await.unwrap().unwrap().blob,
            Bytes::from_static(b"a")
        );
        assert_eq!(
            store.load_snapshot("collab:b").await.unwrap().unwrap().blob,
            Bytes::from_static(b"b")
        );
    }
}
