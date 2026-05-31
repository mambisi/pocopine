use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use uuid::Uuid;

use crate::backend_common::{
    checked_new_offset, ensure_open, ensure_owner, ensure_size_limit,
    ensure_upload_length_can_be_set, expires_at, object_ref, refresh_expired, select_upload_mode,
};
use crate::checksum::{ensure_supported_checksum_policy, validate_complete_checksum};
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
#[derive(Clone)]
pub struct MemoryStorageBackend {
    name: &'static str,
    inner: Arc<Mutex<Inner>>,
}

impl std::fmt::Debug for MemoryStorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStorageBackend")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
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

    /// Remove expired in-memory upload sessions.
    pub fn sweep_expired_uploads(&self) -> StorageResult<usize> {
        let mut inner = self.lock()?;
        let before = inner.sessions.len();
        inner.sessions.retain(|_, stored| {
            refresh_expired_public(stored);
            stored.public.status != UploadSessionStatus::Expired
        });
        Ok(before.saturating_sub(inner.sessions.len()))
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
            let mut inner = self.lock()?;
            let stored = inner
                .sessions
                .get_mut(session.as_str())
                .ok_or_else(|| StorageError::unknown_upload_session(session.to_string()))?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            refresh_expired_public(stored);
            let mut public = stored.public.clone();
            if public.status == UploadSessionStatus::Open {
                public.next_offset = Some(stored.bytes.len() as u64);
            }
            Ok(public)
        })
    }

    fn set_upload_length<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        size: u64,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let mut inner = self.lock()?;
            let stored = inner
                .sessions
                .get_mut(session.as_str())
                .ok_or_else(|| StorageError::unknown_upload_session(session.to_string()))?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            refresh_expired_public(stored);
            let committed_offset = stored.bytes.len() as u64;
            ensure_upload_length_can_be_set(
                stored.max_bytes,
                &stored.public,
                committed_offset,
                size,
            )?;
            stored.public.size = Some(size);
            stored.public.next_offset = Some(committed_offset);
            Ok(stored.public.clone())
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
            ensure_owner(&ctx.actor, &stored.owner)?;
            refresh_expired_public(stored);
            ensure_open(&stored.public)?;
            if stored.public.strategy != UploadStrategy::Sequential {
                return Err(StorageError::unsupported(
                    "memory backend only supports sequential proxy upload",
                ));
            }
            let expected = stored.bytes.len() as u64;
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
                ensure_owner(&ctx.actor, &stored.owner)?;
                refresh_expired_public(stored);
                if let Some(object) = &stored.object {
                    return Ok(object.clone());
                }
                ensure_open(&stored.public)?;
                if let Some(size) = stored.public.size {
                    let actual = stored.bytes.len() as u64;
                    if actual != size {
                        return Err(StorageError::policy_rejected(format!(
                            "upload is incomplete: expected {size} bytes, got {actual}"
                        )));
                    }
                }
                ensure_size_limit(
                    stored.max_bytes,
                    stored.public.size,
                    stored.bytes.len() as u64,
                )?;
                let checksum = validate_complete_checksum(
                    &stored.checksum_policy,
                    &stored.bytes,
                    request.checksum,
                )?;

                let key = stored.storage_key.key.to_string();
                let object = object_ref(
                    self.name,
                    &stored.public,
                    &stored.storage_key,
                    stored.visibility,
                    &stored.request_metadata,
                    stored.bytes.len() as u64,
                    checksum,
                );
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
            ensure_owner(&ctx.actor, &stored.owner)?;
            inner.sessions.remove(session.as_str());
            Ok(())
        })
    }
}

fn refresh_expired_public(stored: &mut StoredUpload) {
    if refresh_expired(&mut stored.public) {
        stored.public.next_offset = Some(stored.bytes.len() as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_dump_in_memory_objects() {
        let backend = MemoryStorageBackend::new();
        backend
            .inner
            .lock()
            .unwrap()
            .objects
            .insert("secret-key".to_string(), b"secret-bytes".to_vec());

        let debug = format!("{backend:?}");

        assert!(debug.contains("MemoryStorageBackend"));
        assert!(!debug.contains("secret-key"));
        assert!(!debug.contains("secret-bytes"));
        assert!(!debug.contains("objects"));
    }
}
