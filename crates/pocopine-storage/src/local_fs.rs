use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::server::{StorageActor, StorageBackend, StorageBoxFuture, StorageContext};
use crate::{
    CompleteUpload, InitiateUpload, ObjectRef, SafeObjectKey, StorageError, StorageKey,
    StorageResult, TransferPlan, UploadSession, UploadSessionId, UploadSessionStatus,
    UploadStrategy,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredUploadSession {
    public: UploadSession,
    owner: StorageActor,
    storage_key: StorageKey,
    visibility: crate::ObjectVisibility,
    request_metadata: std::collections::BTreeMap<String, String>,
    object: Option<ObjectRef>,
}

/// Local filesystem storage backend.
///
/// Completed objects are stored under `root` by [`SafeObjectKey`]. Upload
/// metadata and temporary bytes live under `root/.pocopine-storage/sessions`.
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
        if stored.public.status == UploadSessionStatus::Open {
            stored.public.next_offset = Some(self.temp_len(session)?);
        }
        Ok(stored)
    }

    fn write_session(
        &self,
        session: &UploadSessionId,
        stored: &StoredUploadSession,
    ) -> StorageResult<()> {
        let dir = self.session_dir(session);
        fs::create_dir_all(&dir)
            .map_err(|err| StorageError::backend(format!("create upload session dir: {err}")))?;
        let path = self.session_meta_path(session);
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(stored)
            .map_err(|err| StorageError::backend(format!("encode upload metadata: {err}")))?;
        fs::write(&tmp, bytes)
            .map_err(|err| StorageError::backend(format!("write upload metadata: {err}")))?;
        fs::rename(&tmp, &path)
            .map_err(|err| StorageError::backend(format!("commit upload metadata: {err}")))?;
        Ok(())
    }

    fn temp_len(&self, session: &UploadSessionId) -> StorageResult<u64> {
        match fs::metadata(self.session_tmp_path(session)) {
            Ok(metadata) => Ok(metadata.len()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(err) => Err(StorageError::backend(format!(
                "read upload temp file: {err}"
            ))),
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
                .map_err(|err| StorageError::backend(format!("create storage root: {err}")))?;
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
            fs::create_dir_all(&dir).map_err(|err| {
                StorageError::backend(format!("create upload session dir: {err}"))
            })?;
            File::create(self.session_tmp_path(&id))
                .map_err(|err| StorageError::backend(format!("create upload temp file: {err}")))?;
            let stored = StoredUploadSession {
                public: session.clone(),
                owner: ctx.actor.clone(),
                storage_key: request.storage_key,
                visibility: request.policy.visibility,
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
            ensure_open(&stored)?;
            if stored.public.strategy != UploadStrategy::Sequential {
                return Err(StorageError::unsupported(
                    "local filesystem backend only supports sequential proxy upload",
                ));
            }
            let expected = self.temp_len(&session)?;
            if expected != offset {
                return Err(StorageError::offset_mismatch(expected, offset));
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.session_tmp_path(&session))
                .map_err(|err| StorageError::backend(format!("open upload temp file: {err}")))?;
            file.write_all(&bytes)
                .map_err(|err| StorageError::backend(format!("append upload temp file: {err}")))?;
            file.flush()
                .map_err(|err| StorageError::backend(format!("flush upload temp file: {err}")))?;
            stored.public.next_offset = Some(expected + bytes.len() as u64);
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
            ensure_open(&stored)?;
            let actual = self.temp_len(&request.session)?;
            if let Some(expected) = stored.public.size {
                if actual != expected {
                    return Err(StorageError::policy_rejected(format!(
                        "upload is incomplete: expected {expected} bytes, got {actual}"
                    )));
                }
            }

            let final_path = self.object_path(&stored.storage_key.key);
            if let Some(parent) = final_path.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    StorageError::backend(format!("create object parent directory: {err}"))
                })?;
            }
            move_or_copy(self.session_tmp_path(&request.session), &final_path)?;

            let mut metadata = stored.storage_key.metadata.0.clone();
            metadata.extend(stored.request_metadata.clone());
            let object = ObjectRef {
                backend: self.name.to_string(),
                scope: stored.public.scope.clone(),
                key: stored.storage_key.key.to_string(),
                version: None,
                etag: None,
                checksum: request.checksum,
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
                Err(err) => Err(StorageError::backend(format!(
                    "remove upload session: {err}"
                ))),
            }
        })
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
    if ctx.actor == stored.owner {
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
        | UploadSessionStatus::Completing => Err(StorageError::UploadClosed {
            session: stored.public.id.to_string(),
        }),
    }
}

fn move_or_copy(from: PathBuf, to: &Path) -> StorageResult<()> {
    match fs::rename(&from, to) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            fs::copy(&from, to)
                .map_err(|err| StorageError::backend(format!("copy completed object: {err}")))?;
            fs::remove_file(&from).map_err(|err| {
                StorageError::backend(format!(
                    "remove upload temp after copy failed rename ({rename_err}): {err}"
                ))
            })?;
            Ok(())
        }
    }
}

fn map_not_found<F>(err: std::io::Error, not_found: F) -> StorageError
where
    F: FnOnce() -> StorageError,
{
    if err.kind() == std::io::ErrorKind::NotFound {
        not_found()
    } else {
        StorageError::backend(format!("read local upload metadata: {err}"))
    }
}
