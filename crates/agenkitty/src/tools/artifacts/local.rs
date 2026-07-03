//! `LocalArtifactStore`: the durable artifact backend.
//!
//! Metadata lives in one append-only `artifacts.jsonl` log under a
//! host-provided root; owned contents live out-of-band as content-addressed
//! blob files under `blobs/<sha256>` (identical contents share one blob).
//! Each operation persists its record before committing it to the in-process
//! index — a failed write never leaves a change that would vanish on reload.
//! The log is replayed once on `open`; this backend assumes a single
//! in-process writer (multi-process coordination is a deferred follow-up,
//! same as the memory store).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use serde::{Deserialize, Serialize};

use super::common::{
    ArtifactContentWindow, ArtifactDraft, ArtifactFuture, ArtifactMetadata, ArtifactScope,
    ArtifactStore, ArtifactStoreKind, MAX_CONTENT_BYTES, content_hash,
};
use super::store::{content_window, deleted_err, is_accessible, not_found, read_linked_file};

/// A blob file name is always a lowercase-hex SHA-256 — exactly 64 chars, no
/// path separators, no `.`. Anything else in a (disk-sourced, agent-writable)
/// log record is tampering: the join `blobs_dir/<hash>` would otherwise let a
/// crafted `../`-laden hash escape the store and delete/read arbitrary host
/// files. This is the sole gate every blob path passes through.
fn valid_blob_hash(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

const LOG_FILE_NAME: &str = "artifacts.jsonl";
const BLOBS_DIR_NAME: &str = "blobs";

/// One line of the metadata log.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum ArtifactRecord {
    Written { metadata: ArtifactMetadata },
    Deleted { id: String },
}

#[derive(Default)]
struct LocalState {
    seq: u64,
    records: Vec<ArtifactMetadata>,
}

pub struct LocalArtifactStore {
    root: PathBuf,
    log_path: PathBuf,
    blobs_dir: PathBuf,
    workspace_root: Option<PathBuf>,
    inner: Mutex<LocalState>,
}

impl LocalArtifactStore {
    /// Open (creating if needed) an artifact store under `root`. Replays the
    /// existing log into the in-process index.
    pub fn open(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        let root = root.as_ref();
        // Reject a symlinked root before following it (same rationale as the
        // memory store: a checked-out symlink could redirect the log/blobs
        // outside the intended root, and a post-canonicalize check would only
        // ever see the resolved target).
        if let Ok(metadata) = fs::symlink_metadata(root)
            && metadata.file_type().is_symlink()
        {
            return Err(AgenkitError::tool_policy(format!(
                "artifact store root `{}` is a symlink",
                root.display()
            )));
        }
        fs::create_dir_all(root).map_err(io_err)?;
        let root = fs::canonicalize(root).map_err(io_err)?;
        let log_path = root.join(LOG_FILE_NAME);
        let blobs_dir = root.join(BLOBS_DIR_NAME);
        // The blobs dir gets the same no-symlink guard as the root: a
        // pre-created `blobs` symlink in an untrusted checkout would redirect
        // every blob write outside the store.
        if let Ok(metadata) = fs::symlink_metadata(&blobs_dir)
            && metadata.file_type().is_symlink()
        {
            return Err(AgenkitError::tool_policy(format!(
                "artifact blobs dir `{}` is a symlink",
                blobs_dir.display()
            )));
        }
        fs::create_dir_all(&blobs_dir).map_err(io_err)?;

        let mut state = LocalState::default();
        // A log that doesn't end with a newline is a crash mid-append: a later
        // append would glue onto the final `}` (two objects on one line) and
        // hard-fail every reopen. Normalize NOW so the file ends on a record
        // boundary before the runner writes anything.
        match replay_into(&log_path, &mut state)? {
            LogRepair::None => {}
            // The final line was a *valid* record with no trailing newline —
            // keep it (already applied to `state`) and just terminate it.
            LogRepair::AppendNewline => {
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&log_path)
                    .map_err(io_err)?;
                file.write_all(b"\n").map_err(io_err)?;
                file.flush().map_err(io_err)?;
            }
            // The final line was torn garbage — drop it back to the last
            // complete record.
            LogRepair::Truncate(valid_len) => {
                let file = OpenOptions::new()
                    .write(true)
                    .open(&log_path)
                    .map_err(io_err)?;
                file.set_len(valid_len).map_err(io_err)?;
            }
        }
        Ok(Self {
            root,
            log_path,
            blobs_dir,
            workspace_root: None,
            inner: Mutex::new(state),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Set the workspace root linked artifacts resolve against.
    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// The on-disk path for a blob, validating the hash is a single 64-hex
    /// component first (defense-in-depth beside the replay-time check) so a
    /// tampered `sha256` can never escape `blobs_dir`.
    fn blob_path(&self, sha256: &str) -> AgenkitResult<PathBuf> {
        if !valid_blob_hash(sha256) {
            return Err(AgenkitError::tool_policy(
                "artifact blob hash is not a valid 64-hex digest".to_string(),
            ));
        }
        let path = self.blobs_dir.join(sha256);
        // The join must land directly inside blobs_dir (belt-and-suspenders:
        // valid_blob_hash already precludes separators).
        if path.parent() != Some(self.blobs_dir.as_path()) {
            return Err(AgenkitError::tool_policy(
                "artifact blob path escapes the store".to_string(),
            ));
        }
        Ok(path)
    }

    /// Persist a blob if absent. Never follows a planted symlink: the blob
    /// path is inspected with `symlink_metadata` (a symlink is refused, not
    /// dereferenced), and the temp file is created with `create_new` (O_EXCL,
    /// which fails on any pre-existing entry, symlink included) under an
    /// unguessable name before the atomic rename.
    ///
    /// Content-addressed dedupe only trusts an existing blob whose bytes
    /// actually hash to `sha256`; a substituted or corrupted file at that path
    /// (untrusted checkout, manual edit) is overwritten with the correct bytes
    /// rather than silently served for the new artifact.
    fn persist_blob(&self, sha256: &str, contents: &[u8]) -> AgenkitResult<()> {
        let blob = self.blob_path(sha256)?;
        match fs::symlink_metadata(&blob) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AgenkitError::tool_policy(format!(
                    "artifact blob `{sha256}` is a symlink"
                )));
            }
            Ok(metadata) if metadata.is_file() => {
                // Trust the existing blob only if it is within the cap AND its
                // content still hashes to its name; otherwise (corrupt,
                // oversized, or substituted) fall through and overwrite. The
                // capped read means a huge pre-planted file can't exhaust
                // memory during the dedupe check.
                if let Some(existing) = read_capped(&blob, MAX_CONTENT_BYTES)?
                    && content_hash(&existing) == sha256
                {
                    return Ok(());
                }
            }
            Ok(_) => {
                return Err(AgenkitError::internal(format!(
                    "artifact blob `{sha256}` is not a regular file"
                )));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err(err)),
        }
        let tmp = self
            .blobs_dir
            .join(format!("{sha256}.{:016x}.tmp", rand::random::<u64>()));
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(io_err)?;
            file.write_all(contents).map_err(io_err)?;
            file.flush().map_err(io_err)?;
        }
        // `rename` replaces any stale/corrupt regular file at `blob` atomically.
        if let Err(err) = fs::rename(&tmp, &blob) {
            let _ = fs::remove_file(&tmp);
            return Err(io_err(err));
        }
        Ok(())
    }

    /// Read a blob without following a planted symlink, and verify the bytes
    /// still hash to `sha256` — a blob modified/corrupted after the write must
    /// never be served under a citable id whose recorded hash no longer
    /// matches (content-addressed integrity, the read-side of `persist_blob`'s
    /// dedupe check).
    fn read_blob(&self, id: &str, sha256: &str) -> AgenkitResult<Vec<u8>> {
        let blob = self.blob_path(sha256)?;
        match fs::symlink_metadata(&blob) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AgenkitError::tool_policy(format!(
                    "artifact blob for `{id}` is a symlink"
                )));
            }
            Ok(_) => {}
            Err(err) => {
                return Err(AgenkitError::internal(format!(
                    "stat artifact blob `{id}`: {err}"
                )));
            }
        }
        // Capped read: a corrupt/oversized blob (bigger than any valid
        // artifact) can't exhaust memory before the hash check rejects it.
        let Some(bytes) = read_capped(&blob, MAX_CONTENT_BYTES)? else {
            return Err(AgenkitError::tool_policy(format!(
                "artifact `{id}` blob failed its integrity check (larger than the artifact cap)"
            )));
        };
        if content_hash(&bytes) != sha256 {
            return Err(AgenkitError::tool_policy(format!(
                "artifact `{id}` blob failed its integrity check (contents no longer match the \
                 recorded hash)"
            )));
        }
        Ok(bytes)
    }

    /// Append one record to the log, refusing to follow a symlinked log file.
    fn persist(&self, record: &ArtifactRecord) -> AgenkitResult<()> {
        match fs::symlink_metadata(&self.log_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AgenkitError::tool_policy(format!(
                    "artifact log `{}` is a symlink",
                    self.log_path.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err(err)),
        }
        let mut line = serde_json::to_string(record).map_err(json_err)?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(io_err)?;
        file.write_all(line.as_bytes()).map_err(io_err)?;
        file.flush().map_err(io_err)
    }
}

impl ArtifactStore for LocalArtifactStore {
    fn write<'a>(
        &'a self,
        draft: ArtifactDraft,
        contents: Vec<u8>,
    ) -> ArtifactFuture<'a, ArtifactMetadata> {
        Box::pin(async move {
            // A link derives size/hash from the live workspace file and never
            // owns a blob; an owned write stores what it was given.
            let contents = match &draft.link_path {
                Some(link_path) => read_linked_file(self.workspace_root.as_deref(), link_path)?,
                None => contents,
            };
            let sha256 = content_hash(&contents);
            // The whole mutation runs under one lock — id reservation, the
            // file writes, and the index commit — so concurrent writes get
            // distinct `art-N` ids and the durable log never diverges from the
            // in-process index. The I/O here is synchronous, so no await is
            // held across the guard. A failure before the final `seq += 1`
            // leaves the id unconsumed (nothing persisted under it).
            let mut inner = self.inner.lock().map_err(lock_err)?;
            let metadata = ArtifactMetadata {
                id: format!("art-{}", inner.seq + 1),
                name: draft.name,
                media_type: draft.media_type,
                size: contents.len() as u64,
                sha256: sha256.clone(),
                scope: draft.scope,
                namespace: draft.namespace,
                source_refs: draft.source_refs,
                link_path: draft.link_path,
                created_at_ms: draft.created_at_ms,
                deleted: false,
            };
            if metadata.link_path.is_none() {
                self.persist_blob(&sha256, &contents)?;
            }
            self.persist(&ArtifactRecord::Written {
                metadata: metadata.clone(),
            })?;
            inner.seq += 1;
            inner.records.push(metadata.clone());
            Ok(metadata)
        })
    }

    fn stat<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
    ) -> ArtifactFuture<'a, ArtifactMetadata> {
        Box::pin(async move {
            let inner = self.inner.lock().map_err(lock_err)?;
            find(&inner.records, id, accessible).cloned()
        })
    }

    fn read<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
        offset: u64,
        max_bytes: usize,
    ) -> ArtifactFuture<'a, (ArtifactMetadata, ArtifactContentWindow)> {
        Box::pin(async move {
            let metadata = {
                let inner = self.inner.lock().map_err(lock_err)?;
                find(&inner.records, id, accessible)?.clone()
            };
            if metadata.deleted {
                return Err(deleted_err(id));
            }
            let bytes = match &metadata.link_path {
                Some(link_path) => read_linked_file(self.workspace_root.as_deref(), link_path)?,
                None => self.read_blob(id, &metadata.sha256)?,
            };
            Ok((metadata, content_window(&bytes, offset, max_bytes)))
        })
    }

    fn list<'a>(
        &'a self,
        accessible: &'a [(ArtifactScope, String)],
        scope: Option<ArtifactScope>,
        limit: usize,
    ) -> ArtifactFuture<'a, Vec<ArtifactMetadata>> {
        Box::pin(async move {
            let inner = self.inner.lock().map_err(lock_err)?;
            let mut listed: Vec<ArtifactMetadata> = inner
                .records
                .iter()
                .filter(|metadata| !metadata.deleted)
                .filter(|metadata| is_accessible(metadata, accessible))
                .filter(|metadata| scope.is_none_or(|scope| metadata.scope == scope))
                .cloned()
                .collect();
            listed.reverse(); // newest first (append order)
            listed.truncate(limit);
            Ok(listed)
        })
    }

    fn delete<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
    ) -> ArtifactFuture<'a, ArtifactMetadata> {
        Box::pin(async move {
            // The whole mutation runs under one lock — tombstone persist, index
            // commit, and the last-reference check that gates blob removal —
            // so a concurrent write of the same content can't slip a new
            // reference in *after* the check but *before* the removal and lose
            // its blob. I/O here is synchronous; no await is held across the
            // guard.
            let mut inner = self.inner.lock().map_err(lock_err)?;
            let metadata = find(&inner.records, id, accessible)?.clone();
            if metadata.deleted {
                return Err(deleted_err(id));
            }
            self.persist(&ArtifactRecord::Deleted {
                id: metadata.id.clone(),
            })?;
            let record = inner
                .records
                .iter_mut()
                .find(|record| record.id == metadata.id)
                .ok_or_else(|| not_found(id))?;
            record.deleted = true;
            let deleted = record.clone();
            // Recomputed under the same lock, reflecting any write that
            // committed while we held it.
            let blob_still_referenced = inner.records.iter().any(|other| {
                other.id != metadata.id
                    && !other.deleted
                    && other.link_path.is_none()
                    && other.sha256 == metadata.sha256
            });
            if metadata.link_path.is_none() && !blob_still_referenced {
                // Only ever remove a validated in-store blob path — a tampered
                // hash (e.g. from a poisoned log) can never target a file
                // outside `blobs_dir`.
                if let Ok(blob) = self.blob_path(&metadata.sha256) {
                    let _ = fs::remove_file(blob);
                }
            }
            Ok(deleted)
        })
    }

    fn kind(&self) -> ArtifactStoreKind {
        ArtifactStoreKind::LocalFs
    }
}

fn find<'r>(
    records: &'r [ArtifactMetadata],
    id: &str,
    accessible: &[(ArtifactScope, String)],
) -> AgenkitResult<&'r ArtifactMetadata> {
    records
        .iter()
        .find(|metadata| metadata.id == id && is_accessible(metadata, accessible))
        .ok_or_else(|| not_found(id))
}

/// How `open` must normalize the log after replay so the next append lands on
/// a clean record boundary.
enum LogRepair {
    /// The log already ends with a newline (or is empty/absent). Nothing to do.
    None,
    /// The final line is a *valid* record with no trailing newline (a crash
    /// after the record bytes flushed but before its `\n`). Append the newline
    /// — the record is kept.
    AppendNewline,
    /// The final line is torn garbage. Truncate to this byte length (the end
    /// of the last complete record), dropping it.
    Truncate(u64),
}

/// Replay the log into `state`, returning how `open` must repair the file.
/// A non-newline-terminated final line always needs repair (append a newline
/// if it parsed, truncate if it didn't) so a later append cannot glue two
/// records onto one line. Corruption before the final line is a hard error.
fn replay_into(log_path: &Path, state: &mut LocalState) -> AgenkitResult<LogRepair> {
    // Same no-follow guard as `persist`: replay must not read through a
    // planted symlink either.
    match fs::symlink_metadata(log_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AgenkitError::tool_policy(format!(
                "artifact log `{}` is a symlink",
                log_path.display()
            )));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(LogRepair::None),
        Err(err) => return Err(io_err(err)),
    }
    let raw = fs::read_to_string(log_path).map_err(io_err)?;
    if raw.is_empty() {
        return Ok(LogRepair::None);
    }
    let ended_with_newline = raw.ends_with('\n');
    let lines = raw.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let is_final_line = index + 1 == lines.len();
        if line.trim().is_empty() {
            continue;
        }
        let record: ArtifactRecord = match serde_json::from_str(line) {
            // A valid record with no trailing newline (crash after the bytes
            // flushed, before the `\n`): keep it, but the file must be
            // terminated so a later append doesn't glue onto it.
            Ok(record) if is_final_line && !ended_with_newline => {
                apply_record(state, record, log_path, index)?;
                return Ok(LogRepair::AppendNewline);
            }
            Ok(record) => record,
            // A torn final line: drop back to the last complete record.
            Err(_) if is_final_line && !ended_with_newline => {
                let valid_len = raw.rfind('\n').map(|at| at as u64 + 1).unwrap_or(0);
                return Ok(LogRepair::Truncate(valid_len));
            }
            Err(err) => {
                return Err(AgenkitError::internal(format!(
                    "artifact log `{}` line {} is corrupt: {err}",
                    log_path.display(),
                    index + 1
                )));
            }
        };
        apply_record(state, record, log_path, index)?;
    }
    Ok(LogRepair::None)
}

fn apply_record(
    state: &mut LocalState,
    record: ArtifactRecord,
    log_path: &Path,
    index: usize,
) -> AgenkitResult<()> {
    match record {
        ArtifactRecord::Written { metadata } => {
            // A replayed hash is disk-sourced and agent-writable — validate it
            // as a 64-hex digest before it ever reaches a blob path. A tampered
            // record (e.g. a `../`-laden hash aiming a later delete/read at a
            // host file outside the store) fails the open closed.
            if !valid_blob_hash(&metadata.sha256) {
                return Err(AgenkitError::tool_policy(format!(
                    "artifact log `{}` record {} has an invalid blob hash",
                    log_path.display(),
                    index + 1
                )));
            }
            if let Some(seq) = metadata
                .id
                .strip_prefix("art-")
                .and_then(|n| n.parse::<u64>().ok())
            {
                state.seq = state.seq.max(seq);
            }
            state.records.push(metadata);
        }
        ArtifactRecord::Deleted { id } => {
            if let Some(record) = state.records.iter_mut().find(|record| record.id == id) {
                record.deleted = true;
            }
        }
    }
    Ok(())
}

/// Read at most `cap` bytes from a regular file. Returns `None` when the file
/// holds more than `cap` bytes (so a corrupt/oversized blob is rejected
/// without loading it into memory), `Some(bytes)` otherwise.
fn read_capped(path: &Path, cap: usize) -> AgenkitResult<Option<Vec<u8>>> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(io_err)?;
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_err)?;
    if bytes.len() > cap {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn io_err(err: std::io::Error) -> AgenkitError {
    AgenkitError::internal(format!("artifact store io: {err}"))
}

fn json_err(err: serde_json::Error) -> AgenkitError {
    AgenkitError::internal(format!("artifact store encode: {err}"))
}

fn lock_err<T>(_err: std::sync::PoisonError<T>) -> AgenkitError {
    AgenkitError::internal("artifact store lock poisoned")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::common::current_time_ms;
    use super::*;

    fn draft(name: &str, scope: ArtifactScope, namespace: &str) -> ArtifactDraft {
        ArtifactDraft {
            name: name.to_string(),
            media_type: "text/plain".to_string(),
            scope,
            namespace: namespace.to_string(),
            source_refs: Vec::new(),
            link_path: None,
            created_at_ms: current_time_ms(),
        }
    }

    fn session_ns() -> Vec<(ArtifactScope, String)> {
        vec![(ArtifactScope::Session, "thread-1".to_string())]
    }

    #[tokio::test]
    async fn artifacts_survive_reload_with_hash_and_media_type() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = LocalArtifactStore::open(dir.path()).unwrap();
            let meta = store
                .write(
                    draft("report.md", ArtifactScope::Session, "thread-1"),
                    b"# durable".to_vec(),
                )
                .await
                .unwrap();
            assert_eq!(meta.id, "art-1");
        }
        // Reopen: replayed metadata + blob contents are intact (the plan's
        // "listed and read after session reload" gate).
        let store = LocalArtifactStore::open(dir.path()).unwrap();
        let listed = store.list(&session_ns(), None, 10).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].media_type, "text/plain");
        assert_eq!(listed[0].sha256, content_hash(b"# durable"));

        let (_, window) = store.read("art-1", &session_ns(), 0, 64).await.unwrap();
        assert_eq!(window.content, "# durable");

        // New writes continue the id sequence after replay.
        let next = store
            .write(
                draft("second.md", ArtifactScope::Session, "thread-1"),
                b"two".to_vec(),
            )
            .await
            .unwrap();
        assert_eq!(next.id, "art-2");
    }

    #[tokio::test]
    async fn deletion_survives_reload_and_drops_unreferenced_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::open(dir.path()).unwrap();
        let first = store
            .write(
                draft("a.txt", ArtifactScope::Session, "thread-1"),
                b"shared".to_vec(),
            )
            .await
            .unwrap();
        // Same contents → same blob (content-addressed dedupe).
        store
            .write(
                draft("b.txt", ArtifactScope::Session, "thread-1"),
                b"shared".to_vec(),
            )
            .await
            .unwrap();
        let blob = dir.path().join("blobs").join(&first.sha256);
        assert!(blob.exists());

        // Deleting one keeps the blob (still referenced), deleting both drops it.
        store.delete("art-1", &session_ns()).await.unwrap();
        assert!(blob.exists());
        store.delete("art-2", &session_ns()).await.unwrap();
        assert!(!blob.exists());

        // The tombstones survive reload.
        drop(store);
        let store = LocalArtifactStore::open(dir.path()).unwrap();
        assert!(store.stat("art-1", &session_ns()).await.unwrap().deleted);
        assert!(
            store
                .list(&session_ns(), None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn symlinked_root_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        std::fs::create_dir_all(&target).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(LocalArtifactStore::open(&link).is_err());
    }

    #[tokio::test]
    async fn planted_blob_symlinks_are_never_followed() {
        // An untrusted checkout pre-creates blob paths as symlinks; neither
        // the write (dedupe check) nor the read may dereference them.
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "outside contents").unwrap();

        let store_root = dir.path().join("store");
        let store = LocalArtifactStore::open(&store_root).unwrap();
        let sha = content_hash(b"payload");
        std::os::unix::fs::symlink(&outside, store_root.join("blobs").join(&sha)).unwrap();

        // Write of matching content refuses the planted path (no dedupe
        // through a symlink, no overwrite of the target).
        let err = store
            .write(
                draft("a.txt", ArtifactScope::Session, "thread-1"),
                b"payload".to_vec(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "outside contents",
            "the symlink target must be untouched"
        );

        // A blob symlink planted under an already-recorded artifact refuses
        // reads instead of serving the target.
        let store_root_2 = dir.path().join("store2");
        let store2 = LocalArtifactStore::open(&store_root_2).unwrap();
        let meta = store2
            .write(
                draft("b.txt", ArtifactScope::Session, "thread-1"),
                b"real".to_vec(),
            )
            .await
            .unwrap();
        let blob = store_root_2.join("blobs").join(&meta.sha256);
        std::fs::remove_file(&blob).unwrap();
        std::os::unix::fs::symlink(&outside, &blob).unwrap();
        let err = store2
            .read("art-1", &session_ns(), 0, 64)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    #[tokio::test]
    async fn concurrent_writes_get_distinct_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalArtifactStore::open(dir.path()).unwrap());
        let mut handles = Vec::new();
        for i in 0..16u32 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .write(
                        draft(&format!("a{i}.txt"), ArtifactScope::Session, "thread-1"),
                        format!("content-{i}").into_bytes(),
                    )
                    .await
                    .unwrap()
                    .id
            }));
        }
        let mut ids = Vec::new();
        for handle in handles {
            ids.push(handle.await.unwrap());
        }
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 16, "every concurrent write got a unique id");

        // The durable index reopens cleanly with all 16 rows.
        drop(store);
        let reopened = LocalArtifactStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.list(&session_ns(), None, 100).await.unwrap().len(),
            16
        );
    }

    #[tokio::test]
    async fn concurrent_delete_and_shared_write_keep_the_blob() {
        // Two artifacts share one content-addressed blob. Racing a delete of
        // one against a fresh write of the same content must never orphan the
        // blob the surviving/new artifact needs.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalArtifactStore::open(dir.path()).unwrap());
        let first = store
            .write(
                draft("a.txt", ArtifactScope::Session, "thread-1"),
                b"shared payload".to_vec(),
            )
            .await
            .unwrap();
        let blob = dir.path().join("blobs").join(&first.sha256);
        assert!(blob.exists());

        let mut handles = Vec::new();
        for i in 0..8u32 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                if i % 2 == 0 {
                    let _ = store
                        .write(
                            draft(&format!("w{i}.txt"), ArtifactScope::Session, "thread-1"),
                            b"shared payload".to_vec(),
                        )
                        .await;
                } else {
                    let _ = store.delete(&format!("art-{}", i), &session_ns()).await;
                }
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        // Every live artifact that references the blob must still be readable —
        // the blob was never orphaned by a racing delete.
        let live = store.list(&session_ns(), None, 100).await.unwrap();
        for meta in live {
            let (_, window) = store
                .read(&meta.id, &session_ns(), 0, 64)
                .await
                .unwrap_or_else(|e| panic!("live artifact {} unreadable: {e}", meta.id));
            assert_eq!(window.content, "shared payload");
        }
    }

    #[tokio::test]
    async fn substituted_blob_content_is_repaired_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::open(dir.path()).unwrap();
        let meta = store
            .write(
                draft("a.txt", ArtifactScope::Session, "thread-1"),
                b"correct bytes".to_vec(),
            )
            .await
            .unwrap();
        // Tamper: overwrite the blob file with different bytes under the same
        // (now-wrong) hash-named path.
        let blob = dir.path().join("blobs").join(&meta.sha256);
        std::fs::write(&blob, b"TAMPERED").unwrap();

        // A second write of the same logical content must NOT trust the
        // tampered file — it restores the correct bytes.
        store
            .write(
                draft("b.txt", ArtifactScope::Session, "thread-1"),
                b"correct bytes".to_vec(),
            )
            .await
            .unwrap();
        let (_, window) = store.read("art-1", &session_ns(), 0, 64).await.unwrap();
        assert_eq!(window.content, "correct bytes");
    }

    #[tokio::test]
    async fn symlinked_blobs_dir_is_rejected_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let real_blobs = dir.path().join("elsewhere");
        std::fs::create_dir_all(&real_blobs).unwrap();
        let store_root = dir.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        std::os::unix::fs::symlink(&real_blobs, store_root.join("blobs")).unwrap();
        assert!(LocalArtifactStore::open(&store_root).is_err());
    }

    #[tokio::test]
    async fn torn_final_log_line_is_tolerated_but_mid_log_corruption_is_not() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = LocalArtifactStore::open(dir.path()).unwrap();
            store
                .write(
                    draft("a.txt", ArtifactScope::Session, "thread-1"),
                    b"one".to_vec(),
                )
                .await
                .unwrap();
        }
        // A crash mid-append leaves an unterminated partial record.
        let log = dir.path().join("artifacts.jsonl");
        let mut contents = std::fs::read_to_string(&log).unwrap();
        contents.push_str("{\"record\":\"written\",\"metadata\":{\"id\":\"art-2\"");
        std::fs::write(&log, &contents).unwrap();

        let store = LocalArtifactStore::open(dir.path()).unwrap();
        let listed = store.list(&session_ns(), None, 10).await.unwrap();
        assert_eq!(listed.len(), 1, "the intact record replays");
        // Open REPAIRED the log (truncated the partial record), so a new
        // append cannot glue onto the torn bytes and brick the next reopen.
        assert!(std::fs::read_to_string(&log).unwrap().ends_with('\n'));
        let next = store
            .write(
                draft("b.txt", ArtifactScope::Session, "thread-1"),
                b"two".to_vec(),
            )
            .await
            .unwrap();
        assert_eq!(next.id, "art-2");
        drop(store);
        let reopened = LocalArtifactStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.list(&session_ns(), None, 10).await.unwrap().len(),
            2,
            "both records survive the crash + repair + append cycle"
        );

        // Corruption in the MIDDLE of the log (newline-terminated garbage) is
        // a hard error, never silently skipped.
        let mut contents = std::fs::read_to_string(&log).unwrap();
        contents = format!("not-json\n{contents}");
        std::fs::write(&log, &contents).unwrap();
        assert!(LocalArtifactStore::open(dir.path()).is_err());
    }

    #[tokio::test]
    async fn a_valid_unterminated_final_record_is_kept_and_terminated() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = LocalArtifactStore::open(dir.path()).unwrap();
            store
                .write(
                    draft("a.txt", ArtifactScope::Session, "thread-1"),
                    b"one".to_vec(),
                )
                .await
                .unwrap();
        }
        // Crash after the record bytes flushed but before the trailing newline.
        let log = dir.path().join("artifacts.jsonl");
        let mut contents = std::fs::read_to_string(&log).unwrap();
        assert!(contents.ends_with('\n'));
        contents.pop(); // drop the newline — a valid record, unterminated
        std::fs::write(&log, &contents).unwrap();

        // Open keeps the record AND terminates the file, so a later append is
        // clean and every reopen succeeds.
        let store = LocalArtifactStore::open(dir.path()).unwrap();
        assert_eq!(store.list(&session_ns(), None, 10).await.unwrap().len(), 1);
        assert!(std::fs::read_to_string(&log).unwrap().ends_with('\n'));
        let next = store
            .write(
                draft("b.txt", ArtifactScope::Session, "thread-1"),
                b"two".to_vec(),
            )
            .await
            .unwrap();
        assert_eq!(next.id, "art-2");
        drop(store);
        assert_eq!(
            LocalArtifactStore::open(dir.path())
                .unwrap()
                .list(&session_ns(), None, 10)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_poisoned_log_hash_cannot_escape_the_store() {
        // A crafted `artifacts.jsonl` (agent-writable / hostile checkout) with a
        // traversal-laden sha256 must NOT let a later delete/read operate on a
        // file outside blobs/. It fails the open closed.
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("victim.txt");
        std::fs::write(&outside, "do not delete me").unwrap();

        let store_root = dir.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let evil_hash = "../../victim.txt";
        let record = format!(
            "{{\"record\":\"written\",\"metadata\":{{\"id\":\"art-1\",\"name\":\"x\",\
             \"media_type\":\"text/plain\",\"size\":1,\"sha256\":\"{evil_hash}\",\
             \"scope\":\"session\",\"namespace\":\"thread-1\",\"created_at_ms\":0}}}}\n"
        );
        std::fs::write(store_root.join("artifacts.jsonl"), record).unwrap();

        // Open fails closed on the tampered hash.
        assert!(LocalArtifactStore::open(&store_root).is_err());
        // The victim file outside the store is untouched.
        assert!(outside.exists());

        // And blob_path itself refuses a traversal hash directly.
        let clean = LocalArtifactStore::open(dir.path().join("clean")).unwrap();
        assert!(clean.blob_path("../../victim.txt").is_err());
        assert!(clean.blob_path("not-hex").is_err());
        assert!(clean.blob_path(&"a".repeat(63)).is_err());
        assert!(clean.blob_path(&"a".repeat(64)).is_ok());
    }

    #[tokio::test]
    async fn an_oversized_blob_is_rejected_without_loading_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::open(dir.path()).unwrap();
        let meta = store
            .write(
                draft("a.txt", ArtifactScope::Session, "thread-1"),
                b"real".to_vec(),
            )
            .await
            .unwrap();
        // Replace the blob with a sparse file far larger than the cap: the
        // capped read rejects it on size before hashing, never allocating it.
        let blob = dir.path().join("blobs").join(&meta.sha256);
        let file = std::fs::File::create(&blob).unwrap();
        file.set_len(MAX_CONTENT_BYTES as u64 * 4).unwrap();
        drop(file);
        let err = store.read("art-1", &session_ns(), 0, 64).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
        assert!(err.to_string().contains("integrity"));
    }

    #[tokio::test]
    async fn a_corrupted_blob_fails_the_read_integrity_check() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::open(dir.path()).unwrap();
        let meta = store
            .write(
                draft("a.txt", ArtifactScope::Session, "thread-1"),
                b"real bytes".to_vec(),
            )
            .await
            .unwrap();
        // Tamper with the blob after the write (manual edit / corruption).
        std::fs::write(dir.path().join("blobs").join(&meta.sha256), b"TAMPERED").unwrap();
        // The read refuses rather than serve content that no longer matches
        // its citable hash.
        let err = store.read("art-1", &session_ns(), 0, 64).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
        assert!(err.to_string().contains("integrity"));
    }
}
