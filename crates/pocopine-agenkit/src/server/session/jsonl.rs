//! A durable [`SessionStore`] over a directory of JSONL logs.
//!
//! Each thread is two files in the store dir: `<id>.jsonl` — the append-only
//! record log, one JSON [`Record`](super::Record) per line — and `<id>.meta.json`
//! — the thread metadata (parent link + creation time + attributes; rewritten
//! only by `set_attributes`). `last_seq` is derived from the log's line count,
//! so appends never touch the meta file.
//!
//! This is the **dev/reference** durable store: `children` and `last_seq` scan
//! files (O(n)). A production store would keep a KV/SQL index (the roadmap's
//! `sled`/SQLite). Single-writer per thread is assumed (the design invariant),
//! so there is no file locking.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use super::{
    ParentLink, Record, RecordKind, SessionError, SessionFuture, SessionStore, ThreadId,
    ThreadMeta, backend, now_ms,
};

/// The on-disk meta sidecar (`last_seq` is derived from the log; the file is
/// rewritten only when `set_attributes` replaces the attributes).
#[derive(Serialize, Deserialize)]
struct StoredMeta {
    id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<ParentLink>,
    created_ms: u64,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    attributes: serde_json::Value,
}

/// A directory-backed durable session store. One JSONL log + one meta sidecar
/// per thread.
pub struct JsonlSessionStore {
    dir: PathBuf,
}

impl JsonlSessionStore {
    /// A store rooted at `dir` (created on first write if absent).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn log_path(&self, id: &ThreadId) -> SessionResult<PathBuf> {
        Ok(self.dir.join(format!("{}.jsonl", safe_stem(id)?)))
    }

    fn meta_path(&self, id: &ThreadId) -> SessionResult<PathBuf> {
        Ok(self.dir.join(format!("{}.meta.json", safe_stem(id)?)))
    }
}

type SessionResult<T> = super::SessionResult<T>;

/// Reject a thread id that isn't a safe filename stem (defends against path
/// traversal from a caller-supplied id; minted ids are always safe).
fn safe_stem(id: &ThreadId) -> SessionResult<&str> {
    let s = id.as_str();
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(s)
    } else {
        Err(SessionError::Backend(format!("unsafe thread id: {s:?}")))
    }
}

/// Count the records in a log by counting non-empty lines, WITHOUT parsing them.
/// So the next `seq` is derived even if an earlier line is corrupt (a partial
/// write on a crash never blocks future appends), and `append`/`meta` pay no
/// per-line JSON parse. Missing file → 0.
async fn count_records(path: &Path) -> SessionResult<u64> {
    let body = match tokio::fs::read_to_string(path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(backend(e)),
    };
    Ok(body.lines().filter(|l| !l.trim().is_empty()).count() as u64)
}

/// Scan the store dir for meta sidecars (missing dir → empty). Shared by
/// `children` (which filters by parent) and `threads` (which returns them all).
async fn scan_metas(dir: &Path) -> SessionResult<Vec<StoredMeta>> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(backend(e)),
    };
    while let Some(entry) = entries.next_entry().await.map_err(backend)? {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".meta.json"))
        {
            continue;
        }
        let body = tokio::fs::read_to_string(&path).await.map_err(backend)?;
        let stored: StoredMeta =
            serde_json::from_str(&body).map_err(|e| SessionError::Corrupt(e.to_string()))?;
        out.push(stored);
    }
    Ok(out)
}

/// Parse a JSONL log file into records (missing file → empty).
async fn read_records(path: &Path) -> SessionResult<Vec<Record>> {
    let body = match tokio::fs::read_to_string(path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(backend(e)),
    };
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<Record>(l).map_err(|e| SessionError::Corrupt(e.to_string()))
        })
        .collect()
}

impl SessionStore for JsonlSessionStore {
    fn create_thread<'a>(
        &'a self,
        parent: Option<ParentLink>,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ThreadMeta> {
        Box::pin(async move {
            let id = ThreadId::mint()?;
            let created_ms = now_ms();
            tokio::fs::create_dir_all(&self.dir)
                .await
                .map_err(backend)?;
            let stored = StoredMeta {
                id: id.clone(),
                parent: parent.clone(),
                created_ms,
                attributes: attributes.clone(),
            };
            let json = serde_json::to_string(&stored).map_err(backend)?;
            tokio::fs::write(self.meta_path(&id)?, json)
                .await
                .map_err(backend)?;
            // Touch an empty log so the thread exists even before any append.
            tokio::fs::write(self.log_path(&id)?, b"")
                .await
                .map_err(backend)?;
            Ok(ThreadMeta {
                id,
                parent,
                created_ms,
                last_seq: 0,
                attributes,
            })
        })
    }

    fn meta<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Option<ThreadMeta>> {
        Box::pin(async move {
            let body = match tokio::fs::read_to_string(self.meta_path(id)?).await {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(backend(e)),
            };
            let stored: StoredMeta =
                serde_json::from_str(&body).map_err(|e| SessionError::Corrupt(e.to_string()))?;
            let last_seq = count_records(&self.log_path(id)?).await?;
            Ok(Some(ThreadMeta {
                id: stored.id,
                parent: stored.parent,
                created_ms: stored.created_ms,
                last_seq,
                attributes: stored.attributes,
            }))
        })
    }

    fn append<'a>(
        &'a self,
        id: &'a ThreadId,
        kind: RecordKind,
        data: serde_json::Value,
    ) -> SessionFuture<'a, Record> {
        Box::pin(async move {
            let log = self.log_path(id)?;
            // The thread must exist (meta present); seq = current record count,
            // counted by line (no parse) so a corrupt earlier line can't block it.
            if !tokio::fs::try_exists(self.meta_path(id)?)
                .await
                .map_err(backend)?
            {
                return Err(SessionError::NotFound);
            }
            let seq = count_records(&log).await?;
            let record = Record {
                seq,
                timestamp_ms: now_ms(),
                kind,
                data,
            };
            let mut line = serde_json::to_string(&record).map_err(backend)?;
            line.push('\n');
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .await
                .map_err(backend)?;
            f.write_all(line.as_bytes()).await.map_err(backend)?;
            f.flush().await.map_err(backend)?;
            Ok(record)
        })
    }

    fn records<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<Record>> {
        Box::pin(async move {
            // A missing thread (no meta) is NotFound; a thread with an empty log
            // is fine.
            if !tokio::fs::try_exists(self.meta_path(id)?)
                .await
                .map_err(backend)?
            {
                return Err(SessionError::NotFound);
            }
            read_records(&self.log_path(id)?).await
        })
    }

    fn threads<'a>(&'a self) -> SessionFuture<'a, Vec<ThreadMeta>> {
        Box::pin(async move {
            let mut out = Vec::new();
            for stored in scan_metas(&self.dir).await? {
                let last_seq = count_records(&self.log_path(&stored.id)?).await?;
                out.push(ThreadMeta {
                    id: stored.id,
                    parent: stored.parent,
                    created_ms: stored.created_ms,
                    last_seq,
                    attributes: stored.attributes,
                });
            }
            Ok(out)
        })
    }

    fn set_attributes<'a>(
        &'a self,
        id: &'a ThreadId,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ()> {
        Box::pin(async move {
            let path = self.meta_path(id)?;
            let body = match tokio::fs::read_to_string(&path).await {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SessionError::NotFound);
                }
                Err(e) => return Err(backend(e)),
            };
            let mut stored: StoredMeta =
                serde_json::from_str(&body).map_err(|e| SessionError::Corrupt(e.to_string()))?;
            stored.attributes = attributes;
            let json = serde_json::to_string(&stored).map_err(backend)?;
            tokio::fs::write(&path, json).await.map_err(backend)?;
            Ok(())
        })
    }

    fn children<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<ThreadId>> {
        Box::pin(async move {
            Ok(scan_metas(&self.dir)
                .await?
                .into_iter()
                .filter(|s| s.parent.as_ref().is_some_and(|p| &p.parent == id))
                .map(|s| s.id)
                .collect())
        })
    }

    fn delete_thread<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, ()> {
        Box::pin(async move {
            // Refuse if a fork still points here — deleting would orphan it.
            if !self.children(id).await?.is_empty() {
                return Err(SessionError::HasChildren);
            }
            for path in [self.log_path(id)?, self.meta_path(id)?] {
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(backend(e)),
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_survives_a_corrupt_earlier_log_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(dir.path());
        let id = store
            .create_thread(None, serde_json::Value::Null)
            .await
            .unwrap()
            .id;
        store
            .append(&id, RecordKind::Message, serde_json::json!({ "n": 0 }))
            .await
            .unwrap();

        // Simulate a partial/corrupt write (a crash mid-append): a non-JSON line.
        let log = store.log_path(&id).unwrap();
        let mut body = tokio::fs::read_to_string(&log).await.unwrap();
        body.push_str("{ this line is truncated json\n");
        tokio::fs::write(&log, body).await.unwrap();

        // The next append still works — seq counts the corrupt line (so it is 2),
        // never re-parsing it. `meta.last_seq` is likewise line-counted.
        let rec = store
            .append(&id, RecordKind::Message, serde_json::json!({ "n": 2 }))
            .await
            .unwrap();
        assert_eq!(rec.seq, 2);
        assert_eq!(store.meta(&id).await.unwrap().unwrap().last_seq, 3);
    }
}
