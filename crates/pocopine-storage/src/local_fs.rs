use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use bytes::Bytes;
use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use crate::backend_common::{
    checked_new_offset, ensure_open, ensure_owner, ensure_size_limit,
    ensure_upload_length_can_be_set, expires_at, object_ref, refresh_expired, select_upload_mode,
};
use crate::checksum::{ensure_supported_checksum_policy, validate_complete_checksum};
use crate::server::{StorageActor, StorageBackend, StorageBoxFuture, StorageContext};
use crate::{
    ChecksumPolicy, CompleteUpload, InitiateUpload, ObjectRef, SafeObjectKey, StorageError,
    StorageKey, StorageResult, TransferPlan, UploadSession, UploadSessionId, UploadSessionStatus,
    UploadStrategy,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredUploadSession {
    public: UploadSession,
    owner: StorageActor,
    storage_key: StorageKey,
    visibility: crate::ObjectVisibility,
    max_bytes: u64,
    checksum_policy: ChecksumPolicy,
    request_metadata: std::collections::BTreeMap<String, String>,
    object: Option<ObjectRef>,
}

/// Local filesystem storage backend.
///
/// Completed objects are stored under `root` by [`SafeObjectKey`]. Upload
/// metadata and temporary bytes live under `root/.pocopine-storage/sessions`.
/// Session metadata includes the storage actor, so multi-tenant hosts should
/// use separate backend roots for separate tenant trust boundaries.
///
/// Mutating operations on the same session id are serialized through a
/// per-session async mutex, and the underlying `std::fs` I/O runs on
/// `spawn_blocking` workers so the tokio runtime stays responsive even when
/// the disk is slow.
#[derive(Clone)]
pub struct LocalFsStorageBackend {
    name: &'static str,
    root: PathBuf,
    // Per-session async locks. Cloned cheaply via Arc; the inner StdMutex
    // protects only the map and is held briefly.
    session_locks: Arc<StdMutex<HashMap<String, Arc<TokioMutex<()>>>>>,
}

impl std::fmt::Debug for LocalFsStorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalFsStorageBackend")
            .field("name", &self.name)
            .field("root", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl LocalFsStorageBackend {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::named("local_fs", root)
    }

    pub fn named(name: &'static str, root: impl Into<PathBuf>) -> Self {
        Self {
            name,
            root: root.into(),
            session_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    fn session_lock(&self, session: &UploadSessionId) -> Arc<TokioMutex<()>> {
        let mut locks = self
            .session_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(session.as_str().to_string())
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    }

    fn drop_session_lock(&self, session: &UploadSessionId) {
        let mut locks = self
            .session_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Only remove if the entry is no longer in use (strong_count == 2:
        // one in the map, one on our local Arc). Other in-flight callers
        // would otherwise lose their serialization guarantee.
        if let Some(existing) = locks.get(session.as_str()) {
            if Arc::strong_count(existing) <= 2 {
                locks.remove(session.as_str());
            }
        }
    }

    /// Remove expired upload-session directories from disk.
    pub fn sweep_expired_uploads(&self) -> StorageResult<usize> {
        let root = self.sessions_root();
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(err) => return Err(local_io_error("read upload sessions dir", err)),
        };

        let mut removed = 0;
        for entry in entries {
            let entry = entry.map_err(|err| local_io_error("read session dir", err))?;
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            let Ok(session) = UploadSessionId::new(name) else {
                continue;
            };
            let stored = self.read_session(&session)?;
            if stored.public.status == UploadSessionStatus::Expired {
                fs::remove_dir_all(self.session_dir(&session))
                    .map_err(|err| local_io_error("remove expired upload", err))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn sessions_root(&self) -> PathBuf {
        self.root.join(".pocopine-storage").join("sessions")
    }

    fn session_dir(&self, session: &UploadSessionId) -> PathBuf {
        self.sessions_root().join(session.as_str())
    }

    fn session_meta_path(&self, session: &UploadSessionId) -> PathBuf {
        self.session_dir(session).join("session.json")
    }

    fn session_tmp_path(&self, session: &UploadSessionId) -> PathBuf {
        self.session_dir(session).join("bytes.tmp")
    }

    fn object_path(&self, key: &SafeObjectKey) -> PathBuf {
        key.as_str()
            .split('/')
            .fold(self.root.clone(), |mut path, segment| {
                path.push(segment);
                path
            })
    }

    fn read_session(&self, session: &UploadSessionId) -> StorageResult<StoredUploadSession> {
        let path = self.session_meta_path(session);
        let bytes = fs::read(&path).map_err(|err| {
            map_not_found(err, || {
                StorageError::unknown_upload_session(session.to_string())
            })
        })?;
        let mut stored: StoredUploadSession = serde_json::from_slice(&bytes)
            .map_err(|err| StorageError::backend(format!("read local upload metadata: {err}")))?;
        refresh_expired(&mut stored.public);
        Ok(stored)
    }

    fn write_session(
        &self,
        session: &UploadSessionId,
        stored: &StoredUploadSession,
    ) -> StorageResult<()> {
        let dir = self.session_dir(session);
        fs::create_dir_all(&dir).map_err(|err| local_io_error("create upload session dir", err))?;
        let path = self.session_meta_path(session);
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(stored)
            .map_err(|err| StorageError::backend(format!("encode upload metadata: {err}")))?;
        fs::write(&tmp, bytes).map_err(|err| local_io_error("write upload metadata", err))?;
        fs::rename(&tmp, &path).map_err(|err| local_io_error("commit upload metadata", err))?;
        Ok(())
    }

    fn temp_len(&self, session: &UploadSessionId) -> StorageResult<u64> {
        match fs::metadata(self.session_tmp_path(session)) {
            Ok(metadata) => Ok(metadata.len()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(err) => Err(local_io_error("read upload temp file", err)),
        }
    }
}

impl StorageBackend for LocalFsStorageBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn initiate_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: InitiateUpload,
    ) -> StorageBoxFuture<'a, UploadSession> {
        let backend = self.clone();
        let actor = ctx.actor.clone();
        Box::pin(async move {
            run_blocking(move || backend.initiate_upload_blocking(actor, request)).await
        })
    }

    fn inspect_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, UploadSession> {
        let backend = self.clone();
        let actor = ctx.actor.clone();
        Box::pin(async move {
            let lock = backend.session_lock(&session);
            let _guard = lock.lock().await;
            run_blocking(move || backend.inspect_upload_blocking(&actor, &session)).await
        })
    }

    fn set_upload_length<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        size: u64,
    ) -> StorageBoxFuture<'a, UploadSession> {
        let backend = self.clone();
        let actor = ctx.actor.clone();
        Box::pin(async move {
            let lock = backend.session_lock(&session);
            let _guard = lock.lock().await;
            run_blocking(move || backend.set_upload_length_blocking(&actor, &session, size)).await
        })
    }

    fn append_upload_bytes<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        offset: u64,
        bytes: Bytes,
    ) -> StorageBoxFuture<'a, UploadSession> {
        let backend = self.clone();
        let actor = ctx.actor.clone();
        Box::pin(async move {
            let lock = backend.session_lock(&session);
            let _guard = lock.lock().await;
            run_blocking(move || {
                backend.append_upload_bytes_blocking(&actor, &session, offset, bytes)
            })
            .await
        })
    }

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, ObjectRef> {
        let backend = self.clone();
        let actor = ctx.actor.clone();
        Box::pin(async move {
            let lock = backend.session_lock(&request.session);
            let _guard = lock.lock().await;
            run_blocking(move || backend.complete_upload_blocking(&actor, request)).await
        })
    }

    fn abort_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, ()> {
        let backend = self.clone();
        let actor = ctx.actor.clone();
        Box::pin(async move {
            let lock = backend.session_lock(&session);
            let guard = lock.lock().await;
            let outcome = run_blocking({
                let backend = backend.clone();
                let session = session.clone();
                move || backend.abort_upload_blocking(&actor, &session)
            })
            .await;
            // Drop the lock before pruning the map entry so the inner
            // `Arc<TokioMutex>` strong count drops to (map=1, our copy=1)
            // and `drop_session_lock` can reclaim it.
            drop(guard);
            backend.drop_session_lock(&session);
            outcome
        })
    }
}

impl LocalFsStorageBackend {
    fn initiate_upload_blocking(
        &self,
        actor: StorageActor,
        request: InitiateUpload,
    ) -> StorageResult<UploadSession> {
        fs::create_dir_all(&self.root).map_err(|err| local_io_error("create storage root", err))?;
        let strategy = select_upload_mode(request.requested_strategy, self.capabilities())?;
        ensure_supported_checksum_policy(&request.policy.checksum)?;
        let id = UploadSessionId::new(Uuid::new_v4().to_string())?;
        let session = UploadSession {
            id: id.clone(),
            scope: request.scope.clone(),
            file_name: request.file_name.clone(),
            size: request.size,
            content_type: request.content_type.clone(),
            metadata: request.metadata.clone(),
            strategy,
            status: UploadSessionStatus::Open,
            next_offset: Some(0),
            part_size: request.policy.preferred_chunk_size,
            plan: TransferPlan {
                min_part_size: request.policy.min_part_size,
                preferred_part_size: request.policy.preferred_chunk_size,
                max_part_size: request.policy.max_part_size,
                max_parts: request.policy.max_parts,
                max_concurrent_parts: 1,
                resumable: true,
            },
            uploaded_parts: Vec::new(),
            expires_at: expires_at(request.policy.expires_after),
        };
        let dir = self.session_dir(&id);
        fs::create_dir_all(&dir).map_err(|err| local_io_error("create upload session dir", err))?;
        File::create(self.session_tmp_path(&id))
            .map_err(|err| local_io_error("create upload temp file", err))?;
        let stored = StoredUploadSession {
            public: session.clone(),
            owner: actor,
            storage_key: request.storage_key,
            visibility: request.policy.visibility,
            max_bytes: request.policy.max_bytes,
            checksum_policy: request.policy.checksum,
            request_metadata: request.metadata,
            object: None,
        };
        self.write_session(&id, &stored)?;
        Ok(session)
    }

    fn inspect_upload_blocking(
        &self,
        actor: &StorageActor,
        session: &UploadSessionId,
    ) -> StorageResult<UploadSession> {
        let stored = self.read_session(session)?;
        ensure_owner(actor, &stored.owner)?;
        Ok(stored.public)
    }

    fn set_upload_length_blocking(
        &self,
        actor: &StorageActor,
        session: &UploadSessionId,
        size: u64,
    ) -> StorageResult<UploadSession> {
        let mut stored = self.read_session(session)?;
        ensure_owner(actor, &stored.owner)?;
        self.persist_expired_if_needed(session, &stored)?;
        let committed_offset = stored.public.next_offset.unwrap_or(0);
        self.reconcile_temp_len(session, committed_offset)?;
        ensure_upload_length_can_be_set(stored.max_bytes, &stored.public, committed_offset, size)?;
        stored.public.size = Some(size);
        self.write_session(session, &stored)?;
        Ok(stored.public)
    }

    fn append_upload_bytes_blocking(
        &self,
        actor: &StorageActor,
        session: &UploadSessionId,
        offset: u64,
        bytes: Bytes,
    ) -> StorageResult<UploadSession> {
        let mut stored = self.read_session(session)?;
        ensure_owner(actor, &stored.owner)?;
        self.persist_expired_if_needed(session, &stored)?;
        ensure_open(&stored.public)?;
        if stored.public.strategy != UploadStrategy::Sequential {
            return Err(StorageError::unsupported(
                "local filesystem backend only supports sequential proxy upload",
            ));
        }
        let expected = stored.public.next_offset.unwrap_or(0);
        if expected != offset {
            tracing::debug!(
                target: "pocopine.log",
                event_name = "pocopine.storage.offset_mismatch",
                session = %session,
                expected,
                provided = offset,
            );
            return Err(StorageError::offset_mismatch(expected, offset));
        }
        let new_offset = checked_new_offset(offset, bytes.len())?;
        ensure_size_limit(stored.max_bytes, stored.public.size, new_offset)?;
        self.reconcile_temp_len(session, expected)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.session_tmp_path(session))
            .map_err(|err| local_io_error("open upload temp file", err))?;
        file.write_all(&bytes)
            .map_err(|err| local_io_error("append upload temp file", err))?;
        file.flush()
            .map_err(|err| local_io_error("flush upload temp file", err))?;
        stored.public.next_offset = Some(new_offset);
        self.write_session(session, &stored)?;
        Ok(stored.public)
    }

    fn complete_upload_blocking(
        &self,
        actor: &StorageActor,
        request: CompleteUpload,
    ) -> StorageResult<ObjectRef> {
        let mut stored = self.read_session(&request.session)?;
        ensure_owner(actor, &stored.owner)?;
        if let Some(object) = &stored.object {
            return Ok(object.clone());
        }
        self.persist_expired_if_needed(&request.session, &stored)?;
        ensure_open(&stored.public)?;
        let final_path = self.object_path(&stored.storage_key.key);

        let actual = stored.public.next_offset.unwrap_or(0);
        self.reconcile_temp_len(&request.session, actual)?;
        if let Some(expected) = stored.public.size {
            if actual != expected {
                return Err(StorageError::policy_rejected(format!(
                    "upload is incomplete: expected {expected} bytes, got {actual}"
                )));
            }
        }
        ensure_size_limit(stored.max_bytes, stored.public.size, actual)?;
        let uploaded_bytes = fs::read(self.session_tmp_path(&request.session))
            .map_err(|err| local_io_error("read upload temp file", err))?;
        let checksum =
            validate_complete_checksum(&stored.checksum_policy, &uploaded_bytes, request.checksum)?;

        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| local_io_error("create object parent directory", err))?;
        }
        commit_completed_object(self.session_tmp_path(&request.session), &final_path)?;

        let object = self.object_ref(&stored, actual, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(actual);
        stored.object = Some(object.clone());
        self.write_session(&request.session, &stored)?;
        Ok(object)
    }

    fn abort_upload_blocking(
        &self,
        actor: &StorageActor,
        session: &UploadSessionId,
    ) -> StorageResult<()> {
        match self.read_session(session) {
            Ok(stored) => ensure_owner(actor, &stored.owner)?,
            Err(StorageError::UnknownUploadSession { .. }) => return Ok(()),
            Err(err) => return Err(err),
        }
        match fs::remove_dir_all(self.session_dir(session)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(local_io_error("remove upload session", err)),
        }
    }
}

async fn run_blocking<F, T>(f: F) -> StorageResult<T>
where
    F: FnOnce() -> StorageResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| StorageError::backend(format!("blocking task join failed: {err}")))?
}

impl LocalFsStorageBackend {
    fn persist_expired_if_needed(
        &self,
        session: &UploadSessionId,
        stored: &StoredUploadSession,
    ) -> StorageResult<()> {
        if stored.public.status == UploadSessionStatus::Expired {
            self.write_session(session, stored)?;
        }
        Ok(())
    }

    fn reconcile_temp_len(&self, session: &UploadSessionId, trusted_len: u64) -> StorageResult<()> {
        let path = self.session_tmp_path(session);
        let actual = self.temp_len(session)?;
        if actual == trusted_len {
            return Ok(());
        }
        if actual > trusted_len {
            tracing::warn!(
                target: "pocopine.log",
                event_name = "pocopine.storage.truncate_uncommitted_bytes",
                session = %session,
                actual,
                trusted = trusted_len,
            );
            let file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|err| local_io_error("open upload temp file", err))?;
            file.set_len(trusted_len)
                .map_err(|err| local_io_error("truncate upload temp file", err))?;
            return Ok(());
        }
        Err(StorageError::backend(format!(
            "upload temp file is shorter than committed metadata: expected {trusted_len} bytes, got {actual}"
        )))
    }

    fn object_ref(
        &self,
        stored: &StoredUploadSession,
        size: u64,
        checksum: Option<crate::ObjectChecksum>,
    ) -> ObjectRef {
        object_ref(
            self.name,
            &stored.public,
            &stored.storage_key,
            stored.visibility,
            &stored.request_metadata,
            size,
            checksum,
        )
    }
}

fn commit_completed_object(from: PathBuf, to: &Path) -> StorageResult<()> {
    match fs::rename(&from, to) {
        Ok(()) => {
            if let Some(parent) = to.parent() {
                sync_dir(parent);
            }
            Ok(())
        }
        Err(rename_err) => {
            tracing::warn!(
                target: "pocopine.log",
                event_name = "pocopine.storage.rename_completed_object_fallback",
                error = %rename_err,
            );
            let partial = partial_object_path(to);
            fs::copy(&from, &partial)
                .map_err(|err| local_io_error("copy completed object", err))?;
            sync_file(&partial)?;
            fs::rename(&partial, to)
                .map_err(|err| local_io_error("commit completed object", err))?;
            if let Some(parent) = to.parent() {
                sync_dir(parent);
            }
            if let Err(err) = fs::remove_file(&from) {
                tracing::warn!(
                    target: "pocopine.log",
                    event_name = "pocopine.storage.remove_completed_temp_failed",
                    error = %err,
                );
            }
            Ok(())
        }
    }
}

fn partial_object_path(to: &Path) -> PathBuf {
    let file_name = to
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("object");
    to.with_file_name(format!(".{file_name}.{}.part", Uuid::new_v4()))
}

fn sync_file(path: &Path) -> StorageResult<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|err| local_io_error("sync completed object", err))
}

fn sync_dir(path: &Path) {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}

fn map_not_found<F>(err: std::io::Error, not_found: F) -> StorageError
where
    F: FnOnce() -> StorageError,
{
    if err.kind() == std::io::ErrorKind::NotFound {
        not_found()
    } else {
        local_io_error("read local upload metadata", err)
    }
}

fn local_io_error(operation: &'static str, err: std::io::Error) -> StorageError {
    let kind = err.kind();
    let raw_os_error = err.raw_os_error();
    tracing::error!(
        target: "pocopine.log",
        event_name = "pocopine.storage.local_fs_error",
        operation,
        error = %err,
        ?kind,
        ?raw_os_error,
    );
    let raw = raw_os_error
        .map(|code| format!(" (os error {code})"))
        .unwrap_or_default();
    StorageError::backend(format!("{operation}: {kind:?}{raw}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_expose_root_or_session_locks() {
        let backend = LocalFsStorageBackend::new("/tmp/pocopine-secret-storage-root");

        let debug = format!("{backend:?}");

        assert!(debug.contains("LocalFsStorageBackend"));
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("/tmp/pocopine-secret-storage-root"));
        assert!(!debug.contains("session_locks"));
    }
}
