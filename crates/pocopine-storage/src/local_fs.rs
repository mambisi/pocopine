use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::checksum::validate_complete_checksum;
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
#[derive(Clone, Debug)]
pub struct LocalFsStorageBackend {
    name: &'static str,
    root: PathBuf,
}

impl LocalFsStorageBackend {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::named("local_fs", root)
    }

    pub fn named(name: &'static str, root: impl Into<PathBuf>) -> Self {
        Self {
            name,
            root: root.into(),
        }
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
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
        refresh_expired(&mut stored);
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
        Box::pin(async move {
            fs::create_dir_all(&self.root)
                .map_err(|err| local_io_error("create storage root", err))?;
            let strategy = selected_strategy(request.requested_strategy)?;
            let id = UploadSessionId::new(Uuid::new_v4().to_string())?;
            let session = UploadSession {
                id: id.clone(),
                scope: request.scope.clone(),
                file_name: request.file_name.clone(),
                size: request.size,
                content_type: request.content_type.clone(),
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
            fs::create_dir_all(&dir)
                .map_err(|err| local_io_error("create upload session dir", err))?;
            File::create(self.session_tmp_path(&id))
                .map_err(|err| local_io_error("create upload temp file", err))?;
            let stored = StoredUploadSession {
                public: session.clone(),
                owner: ctx.actor.clone(),
                storage_key: request.storage_key,
                visibility: request.policy.visibility,
                max_bytes: request.policy.max_bytes,
                checksum_policy: request.policy.checksum,
                request_metadata: request.metadata,
                object: None,
            };
            self.write_session(&id, &stored)?;
            Ok(session)
        })
    }

    fn inspect_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let stored = self.read_session(&session)?;
            ensure_owner(ctx, &stored)?;
            Ok(stored.public)
        })
    }

    fn append_upload_bytes<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        offset: u64,
        bytes: Bytes,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let mut stored = self.read_session(&session)?;
            ensure_owner(ctx, &stored)?;
            self.persist_expired_if_needed(&session, &stored)?;
            ensure_open(&stored)?;
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
            ensure_size_limit(&stored, new_offset)?;
            self.reconcile_temp_len(&session, expected)?;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.session_tmp_path(&session))
                .map_err(|err| local_io_error("open upload temp file", err))?;
            file.write_all(&bytes)
                .map_err(|err| local_io_error("append upload temp file", err))?;
            file.flush()
                .map_err(|err| local_io_error("flush upload temp file", err))?;
            stored.public.next_offset = Some(new_offset);
            self.write_session(&session, &stored)?;
            Ok(stored.public)
        })
    }

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, ObjectRef> {
        Box::pin(async move {
            let mut stored = self.read_session(&request.session)?;
            ensure_owner(ctx, &stored)?;
            if let Some(object) = &stored.object {
                return Ok(object.clone());
            }
            self.persist_expired_if_needed(&request.session, &stored)?;
            ensure_open(&stored)?;
            let final_path = self.object_path(&stored.storage_key.key);
            if !self.session_tmp_path(&request.session).exists() && final_path.exists() {
                let actual = fs::metadata(&final_path)
                    .map_err(|err| local_io_error("read completed object", err))?
                    .len();
                return self.finish_existing_completed_object(
                    &request.session,
                    &mut stored,
                    &final_path,
                    actual,
                    request.checksum,
                );
            }

            let actual = stored.public.next_offset.unwrap_or(0);
            self.reconcile_temp_len(&request.session, actual)?;
            if let Some(expected) = stored.public.size {
                if actual != expected {
                    return Err(StorageError::policy_rejected(format!(
                        "upload is incomplete: expected {expected} bytes, got {actual}"
                    )));
                }
            }
            ensure_size_limit(&stored, actual)?;
            let uploaded_bytes = fs::read(self.session_tmp_path(&request.session))
                .map_err(|err| local_io_error("read upload temp file", err))?;
            let checksum = validate_complete_checksum(
                &stored.checksum_policy,
                &uploaded_bytes,
                request.checksum,
            )?;

            if let Some(parent) = final_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| local_io_error("create object parent directory", err))?;
            }
            commit_completed_object(self.session_tmp_path(&request.session), &final_path)?;

            let mut metadata = stored.storage_key.metadata.0.clone();
            metadata.extend(stored.request_metadata.clone());
            let object = ObjectRef {
                backend: self.name.to_string(),
                scope: stored.public.scope.clone(),
                key: stored.storage_key.key.to_string(),
                version: None,
                etag: None,
                checksum,
                content_type: stored.public.content_type.clone(),
                size: actual,
                visibility: stored.visibility,
                metadata,
            };
            stored.public.status = UploadSessionStatus::Complete;
            stored.public.next_offset = Some(actual);
            stored.object = Some(object.clone());
            self.write_session(&request.session, &stored)?;
            Ok(object)
        })
    }

    fn abort_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, ()> {
        Box::pin(async move {
            match self.read_session(&session) {
                Ok(stored) => ensure_owner(ctx, &stored)?,
                Err(StorageError::UnknownUploadSession { .. }) => return Ok(()),
                Err(err) => return Err(err),
            }
            match fs::remove_dir_all(self.session_dir(&session)) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(local_io_error("remove upload session", err)),
            }
        })
    }
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

    fn finish_existing_completed_object(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        final_path: &Path,
        actual: u64,
        checksum: Option<crate::ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        if let Some(expected) = stored.public.size {
            if actual != expected {
                return Err(StorageError::policy_rejected(format!(
                    "completed object size mismatch: expected {expected} bytes, got {actual}"
                )));
            }
        }
        ensure_size_limit(stored, actual)?;
        let object_bytes =
            fs::read(final_path).map_err(|err| local_io_error("read completed object", err))?;
        let checksum =
            validate_complete_checksum(&stored.checksum_policy, &object_bytes, checksum)?;
        let object = self.object_ref(stored, actual, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(actual);
        stored.object = Some(object.clone());
        self.write_session(session, stored)?;
        Ok(object)
    }

    fn object_ref(
        &self,
        stored: &StoredUploadSession,
        size: u64,
        checksum: Option<crate::ObjectChecksum>,
    ) -> ObjectRef {
        let mut metadata = stored.storage_key.metadata.0.clone();
        metadata.extend(stored.request_metadata.clone());
        ObjectRef {
            backend: self.name.to_string(),
            scope: stored.public.scope.clone(),
            key: stored.storage_key.key.to_string(),
            version: None,
            etag: None,
            checksum,
            content_type: stored.public.content_type.clone(),
            size,
            visibility: stored.visibility,
            metadata,
        }
    }
}

fn selected_strategy(strategy: UploadStrategy) -> StorageResult<UploadStrategy> {
    match strategy {
        UploadStrategy::Auto | UploadStrategy::Sequential => Ok(UploadStrategy::Sequential),
        UploadStrategy::SingleRequest | UploadStrategy::Multipart => {
            Err(StorageError::unsupported(
                "only sequential proxy uploads are implemented in pocopine-storage PR 1",
            ))
        }
    }
}

fn expires_at(duration: std::time::Duration) -> OffsetDateTime {
    OffsetDateTime::now_utc()
        + time::Duration::seconds(duration.as_secs().min(i64::MAX as u64) as i64)
}

fn ensure_owner(ctx: &StorageContext, stored: &StoredUploadSession) -> StorageResult<()> {
    if ctx.actor.same_owner(&stored.owner) {
        Ok(())
    } else {
        Err(StorageError::forbidden(
            "upload session belongs to a different storage actor",
        ))
    }
}

fn ensure_open(stored: &StoredUploadSession) -> StorageResult<()> {
    match stored.public.status {
        UploadSessionStatus::Open => Ok(()),
        UploadSessionStatus::Complete => Err(StorageError::UploadComplete {
            session: stored.public.id.to_string(),
        }),
        UploadSessionStatus::Aborted
        | UploadSessionStatus::Expired
        | UploadSessionStatus::Completing => {
            tracing::debug!(
                target: "pocopine.log",
                event_name = "pocopine.storage.upload_closed",
                session = %stored.public.id,
                status = ?stored.public.status,
            );
            Err(StorageError::UploadClosed {
                session: stored.public.id.to_string(),
            })
        }
    }
}

fn refresh_expired(stored: &mut StoredUploadSession) {
    if stored.public.status == UploadSessionStatus::Open
        && OffsetDateTime::now_utc() >= stored.public.expires_at
    {
        stored.public.status = UploadSessionStatus::Expired;
    }
}

fn checked_new_offset(offset: u64, byte_count: usize) -> StorageResult<u64> {
    offset
        .checked_add(byte_count as u64)
        .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))
}

fn ensure_size_limit(stored: &StoredUploadSession, new_offset: u64) -> StorageResult<()> {
    if new_offset > stored.max_bytes {
        return Err(StorageError::policy_rejected(format!(
            "upload exceeds scope max of {} bytes",
            stored.max_bytes
        )));
    }
    if let Some(size) = stored.public.size {
        if new_offset > size {
            return Err(StorageError::policy_rejected(format!(
                "upload exceeds declared size of {size} bytes"
            )));
        }
    }
    Ok(())
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
