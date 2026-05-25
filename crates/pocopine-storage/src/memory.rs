use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::checksum::validate_complete_checksum;
use crate::server::{StorageActor, StorageBackend, StorageBoxFuture, StorageContext};
use crate::{
    ChecksumPolicy, CompleteUpload, InitiateUpload, ObjectRef, StorageError, StorageKey,
    StorageResult, TransferPlan, UploadSession, UploadSessionId, UploadSessionStatus,
    UploadStrategy,
};

#[derive(Clone, Debug)]
struct StoredUpload {
    public: UploadSession,
    owner: StorageActor,
    storage_key: StorageKey,
    visibility: crate::ObjectVisibility,
    max_bytes: u64,
    checksum_policy: ChecksumPolicy,
    request_metadata: std::collections::BTreeMap<String, String>,
    bytes: Vec<u8>,
    object: Option<ObjectRef>,
}

#[derive(Default, Debug)]
struct Inner {
    sessions: HashMap<String, StoredUpload>,
    objects: HashMap<String, Vec<u8>>,
}

/// In-memory storage backend for tests, demos, and single-process examples.
///
/// It mirrors the local filesystem backend's sequential session semantics but
/// does not persist across process restarts.
#[derive(Clone, Debug)]
pub struct MemoryStorageBackend {
    name: &'static str,
    inner: Arc<Mutex<Inner>>,
}

impl Default for MemoryStorageBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStorageBackend {
    pub fn new() -> Self {
        Self::named("memory")
    }

    pub fn named(name: &'static str) -> Self {
        Self {
            name,
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    pub fn object_bytes(&self, key: &str) -> StorageResult<Option<Vec<u8>>> {
        Ok(self.lock()?.objects.get(key).cloned())
    }

    fn lock(&self) -> StorageResult<std::sync::MutexGuard<'_, Inner>> {
        self.inner
            .lock()
            .map_err(|_| StorageError::backend("memory storage lock poisoned"))
    }
}

impl StorageBackend for MemoryStorageBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn initiate_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: InitiateUpload,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
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
            let stored = StoredUpload {
                public: session.clone(),
                owner: ctx.actor.clone(),
                storage_key: request.storage_key,
                visibility: request.policy.visibility,
                max_bytes: request.policy.max_bytes,
                checksum_policy: request.policy.checksum,
                request_metadata: request.metadata,
                bytes: Vec::new(),
                object: None,
            };
            self.lock()?.sessions.insert(id.to_string(), stored);
            Ok(session)
        })
    }

    fn inspect_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let inner = self.lock()?;
            let stored = inner
                .sessions
                .get(session.as_str())
                .ok_or_else(|| StorageError::unknown_upload_session(session.to_string()))?;
            ensure_owner(ctx, stored)?;
            let mut public = stored.public.clone();
            if public.status == UploadSessionStatus::Open {
                public.next_offset = Some(stored.bytes.len() as u64);
            }
            Ok(public)
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
            let mut inner = self.lock()?;
            let stored = inner
                .sessions
                .get_mut(session.as_str())
                .ok_or_else(|| StorageError::unknown_upload_session(session.to_string()))?;
            ensure_owner(ctx, stored)?;
            ensure_open(stored)?;
            if stored.public.strategy != UploadStrategy::Sequential {
                return Err(StorageError::unsupported(
                    "memory backend only supports sequential proxy upload",
                ));
            }
            let expected = stored.bytes.len() as u64;
            if expected != offset {
                return Err(StorageError::offset_mismatch(expected, offset));
            }
            let new_offset = checked_new_offset(offset, bytes.len())?;
            ensure_size_limit(stored, new_offset)?;
            stored.bytes.extend_from_slice(&bytes);
            stored.public.next_offset = Some(stored.bytes.len() as u64);
            Ok(stored.public.clone())
        })
    }

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, ObjectRef> {
        Box::pin(async move {
            let mut inner = self.lock()?;
            let (key, bytes, object) = {
                let stored = inner
                    .sessions
                    .get_mut(request.session.as_str())
                    .ok_or_else(|| {
                        StorageError::unknown_upload_session(request.session.to_string())
                    })?;
                ensure_owner(ctx, stored)?;
                if let Some(object) = &stored.object {
                    return Ok(object.clone());
                }
                ensure_open(stored)?;
                if let Some(size) = stored.public.size {
                    let actual = stored.bytes.len() as u64;
                    if actual != size {
                        return Err(StorageError::policy_rejected(format!(
                            "upload is incomplete: expected {size} bytes, got {actual}"
                        )));
                    }
                }
                ensure_size_limit(stored, stored.bytes.len() as u64)?;
                let checksum = validate_complete_checksum(
                    &stored.checksum_policy,
                    &stored.bytes,
                    request.checksum,
                )?;

                let key = stored.storage_key.key.to_string();
                let mut metadata = stored.storage_key.metadata.0.clone();
                metadata.extend(stored.request_metadata.clone());
                let object = ObjectRef {
                    backend: self.name.to_string(),
                    scope: stored.public.scope.clone(),
                    key: key.clone(),
                    version: None,
                    etag: None,
                    checksum,
                    content_type: stored.public.content_type.clone(),
                    size: stored.bytes.len() as u64,
                    visibility: stored.visibility,
                    metadata,
                };
                stored.public.status = UploadSessionStatus::Complete;
                stored.public.next_offset = Some(stored.bytes.len() as u64);
                stored.object = Some(object.clone());
                (key, stored.bytes.clone(), object)
            };
            inner.objects.insert(key, bytes);
            Ok(object)
        })
    }

    fn abort_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self.lock()?;
            let Some(stored) = inner.sessions.get(session.as_str()) else {
                return Ok(());
            };
            ensure_owner(ctx, stored)?;
            inner.sessions.remove(session.as_str());
            Ok(())
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

fn ensure_owner(ctx: &StorageContext, stored: &StoredUpload) -> StorageResult<()> {
    if ctx.actor == stored.owner {
        Ok(())
    } else {
        Err(StorageError::forbidden(
            "upload session belongs to a different storage actor",
        ))
    }
}

fn ensure_open(stored: &StoredUpload) -> StorageResult<()> {
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

fn checked_new_offset(offset: u64, byte_count: usize) -> StorageResult<u64> {
    offset
        .checked_add(byte_count as u64)
        .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))
}

fn ensure_size_limit(stored: &StoredUpload, new_offset: u64) -> StorageResult<()> {
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
