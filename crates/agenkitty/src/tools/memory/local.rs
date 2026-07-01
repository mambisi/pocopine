//! `LocalJsonlMemoryStore`: a durable, append-only backend.
//!
//! All entries live in one `memory.jsonl` log under a host-provided root (the
//! framework never picks a global user-memory path). Each operation persists its
//! record(s) before committing them to the in-process [`MemoryState`], so a
//! failed write never leaves a change that would vanish on reload. The log is
//! replayed once on `open`; this backend assumes a single in-process writer
//! (multi-process coordination is a deferred follow-up).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};

use super::common::{
    MemoryCompactionReport, MemoryCompactionRequest, MemoryEntry, MemoryFuture, MemoryPatch,
    MemorySearchFilter, MemorySearchHit, MemoryStore, MemoryStoreKind, MemoryTombstone, lock_err,
};
use super::store::{MemoryRecord, MemoryState};

const LOG_FILE_NAME: &str = "memory.jsonl";

#[derive(Debug)]
pub struct LocalJsonlMemoryStore {
    root: PathBuf,
    log_path: PathBuf,
    inner: Mutex<MemoryState>,
}

impl LocalJsonlMemoryStore {
    /// Open (creating if needed) a memory log under `root`. Replays the existing
    /// log into the in-process index.
    pub fn open(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        let root = root.as_ref();
        // Reject a symlinked root before following it: `create_dir_all` and
        // `canonicalize` both resolve the link, so a check after canonicalizing
        // would only ever see the (non-symlink) target and wave it through. A
        // checked-out `.agenkitty/.../memory` symlink could otherwise redirect
        // the log outside the intended root.
        if let Ok(metadata) = fs::symlink_metadata(root)
            && metadata.file_type().is_symlink()
        {
            return Err(AgenkitError::tool_policy(format!(
                "memory store root `{}` is a symlink",
                root.display()
            )));
        }
        fs::create_dir_all(root).map_err(io_err)?;
        let root = fs::canonicalize(root).map_err(io_err)?;
        let log_path = root.join(LOG_FILE_NAME);
        ensure_under_root(&root, &log_path)?;

        let mut state = MemoryState::default();
        replay_into(&log_path, &mut state)?;
        Ok(Self {
            root,
            log_path,
            inner: Mutex::new(state),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Append records to the log, refusing to follow a symlinked log file.
    fn persist(&self, records: &[MemoryRecord]) -> AgenkitResult<()> {
        match fs::symlink_metadata(&self.log_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AgenkitError::tool_policy(format!(
                    "memory log `{}` is a symlink",
                    self.log_path.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err(err)),
        }
        let mut buffer = String::new();
        for record in records {
            buffer.push_str(&serde_json::to_string(record).map_err(json_err)?);
            buffer.push('\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(io_err)?;
        file.write_all(buffer.as_bytes()).map_err(io_err)?;
        file.flush().map_err(io_err)?;
        Ok(())
    }
}

impl MemoryStore for LocalJsonlMemoryStore {
    fn append<'a>(&'a self, entry: MemoryEntry) -> MemoryFuture<'a, MemoryEntry> {
        Box::pin(async move {
            let mut state = self.inner.lock().map_err(lock_err)?;
            let (entry, record) = state.plan_append(entry)?;
            self.persist(std::slice::from_ref(&record))?;
            state.apply_record(record);
            Ok(entry)
        })
    }

    fn read<'a>(
        &'a self,
        id: &'a str,
        version: Option<u64>,
    ) -> MemoryFuture<'a, Option<MemoryEntry>> {
        Box::pin(async move {
            let state = self.inner.lock().map_err(lock_err)?;
            Ok(state.read_entry(id, version))
        })
    }

    fn search<'a>(&'a self, filter: MemorySearchFilter) -> MemoryFuture<'a, Vec<MemorySearchHit>> {
        Box::pin(async move {
            let state = self.inner.lock().map_err(lock_err)?;
            Ok(state.search_entries(&filter))
        })
    }

    fn update<'a>(
        &'a self,
        id: &'a str,
        expected_version: u64,
        patch: MemoryPatch,
    ) -> MemoryFuture<'a, MemoryEntry> {
        Box::pin(async move {
            let mut state = self.inner.lock().map_err(lock_err)?;
            let (entry, record) = state.plan_update(id, expected_version, patch)?;
            self.persist(std::slice::from_ref(&record))?;
            state.apply_record(record);
            Ok(entry)
        })
    }

    fn tombstone<'a>(
        &'a self,
        id: &'a str,
        expected_version: u64,
        reason: String,
    ) -> MemoryFuture<'a, MemoryTombstone> {
        Box::pin(async move {
            let mut state = self.inner.lock().map_err(lock_err)?;
            let (tombstone, record) = state.plan_tombstone(id, expected_version, reason)?;
            self.persist(std::slice::from_ref(&record))?;
            state.apply_record(record);
            Ok(tombstone)
        })
    }

    fn compact<'a>(
        &'a self,
        request: MemoryCompactionRequest,
    ) -> MemoryFuture<'a, MemoryCompactionReport> {
        Box::pin(async move {
            let mut state = self.inner.lock().map_err(lock_err)?;
            let (report, records) = state.plan_compact(request)?;
            self.persist(&records)?;
            for record in records {
                state.apply_record(record);
            }
            Ok(report)
        })
    }

    fn kind(&self) -> MemoryStoreKind {
        MemoryStoreKind::LocalJsonl
    }
}

fn replay_into(path: &Path, state: &mut MemoryState) -> AgenkitResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AgenkitError::tool_policy(format!(
                "memory log `{}` is a symlink",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io_err(err)),
    }

    let body = fs::read_to_string(path).map_err(io_err)?;
    let ended_with_newline = body.ends_with('\n');
    let lines = body.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: MemoryRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            // A torn final line (a crash mid-write, no trailing newline) is
            // tolerated; corruption anywhere else is a hard error.
            Err(_) if !ended_with_newline && index + 1 == lines.len() => break,
            Err(err) => {
                return Err(AgenkitError::Json {
                    message: format!(
                        "memory log `{}` line {} is corrupt: {err}",
                        path.display(),
                        index + 1
                    ),
                });
            }
        };
        state.apply_record(record);
    }
    Ok(())
}

fn ensure_under_root(root: &Path, path: &Path) -> AgenkitResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AgenkitError::validation("memory log path has no parent"))?;
    let parent = if parent.exists() {
        fs::canonicalize(parent).map_err(io_err)?
    } else {
        parent.to_path_buf()
    };
    if parent == root {
        Ok(())
    } else {
        Err(AgenkitError::tool_policy(format!(
            "memory log path `{}` escapes `{}`",
            path.display(),
            root.display()
        )))
    }
}

fn io_err(err: std::io::Error) -> AgenkitError {
    AgenkitError::internal(err.to_string())
}

fn json_err(err: serde_json::Error) -> AgenkitError {
    AgenkitError::Json {
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::common::{
        MemoryEntry, MemoryKind, MemoryRetention, MemoryScope, MemorySource,
    };

    fn draft(title: &str, body: &str) -> MemoryEntry {
        MemoryEntry::draft(
            MemoryScope::Project,
            "proj",
            MemoryKind::Fact,
            title,
            body,
            vec![],
            MemorySource::Agent,
            vec![],
            "reason",
            MemoryRetention::Session,
            None,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn entries_survive_reload() {
        let dir = tempfile::tempdir().unwrap();
        let id = {
            let store = LocalJsonlMemoryStore::open(dir.path()).unwrap();
            assert_eq!(store.kind(), MemoryStoreKind::LocalJsonl);
            store.append(draft("yrs", "we chose yrs")).await.unwrap().id
        };

        let reopened = LocalJsonlMemoryStore::open(dir.path()).unwrap();
        let entry = reopened.read(&id, None).await.unwrap().unwrap();
        assert_eq!(entry.id, "mem-1");
        assert_eq!(entry.body, "we chose yrs");

        // The id sequence resumes from the replayed max, not from zero.
        let next = reopened.append(draft("second", "b")).await.unwrap();
        assert_eq!(next.id, "mem-2");
    }

    #[tokio::test]
    async fn updates_replay_to_latest_version() {
        let dir = tempfile::tempdir().unwrap();
        let id = {
            let store = LocalJsonlMemoryStore::open(dir.path()).unwrap();
            let id = store.append(draft("t", "b")).await.unwrap().id;
            store
                .update(
                    &id,
                    1,
                    MemoryPatch {
                        body: Some("revised".to_string()),
                        reason: "clarify".to_string(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            id
        };

        let reopened = LocalJsonlMemoryStore::open(dir.path()).unwrap();
        let current = reopened.read(&id, None).await.unwrap().unwrap();
        assert_eq!(current.version, 2);
        assert_eq!(current.body, "revised");
        // The earlier revision still replays and is readable by version.
        assert_eq!(
            reopened.read(&id, Some(1)).await.unwrap().unwrap().body,
            "b"
        );
    }

    #[tokio::test]
    async fn tombstones_survive_reload() {
        let dir = tempfile::tempdir().unwrap();
        let id = {
            let store = LocalJsonlMemoryStore::open(dir.path()).unwrap();
            let id = store.append(draft("t", "b")).await.unwrap().id;
            store.tombstone(&id, 1, "stale".to_string()).await.unwrap();
            id
        };

        let reopened = LocalJsonlMemoryStore::open(dir.path()).unwrap();
        assert!(reopened.read(&id, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn torn_final_line_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let id = {
            let store = LocalJsonlMemoryStore::open(dir.path()).unwrap();
            store.append(draft("good", "body")).await.unwrap().id
        };
        // Simulate a crash mid-write: append a partial JSON line with no newline.
        let log = dir.path().join(LOG_FILE_NAME);
        let mut file = OpenOptions::new().append(true).open(&log).unwrap();
        file.write_all(b"{\"kind\":\"append\",\"entry\":{").unwrap();
        file.flush().unwrap();

        let reopened = LocalJsonlMemoryStore::open(dir.path()).unwrap();
        assert!(reopened.read(&id, None).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn mid_file_corruption_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join(LOG_FILE_NAME);
        fs::write(&log, "not json\n{\"kind\":\"append\"}\n").unwrap();
        let err = LocalJsonlMemoryStore::open(dir.path());
        assert!(err.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // The root itself is a symlink to a real dir; it must be refused before
        // create_dir_all/canonicalize follow it.
        assert!(LocalJsonlMemoryStore::open(&link).is_err());
    }

    #[tokio::test]
    async fn symlinked_log_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.jsonl");
        fs::write(&outside, "").unwrap();
        let log = dir.path().join(LOG_FILE_NAME);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &log).unwrap();
        #[cfg(unix)]
        {
            let err = LocalJsonlMemoryStore::open(dir.path());
            assert!(err.is_err());
        }
    }
}
