use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use time::OffsetDateTime;
use tokio::sync::Mutex as TokioMutex;

use crate::server::StorageActor;
use crate::{
    ObjectChecksum, ObjectRef, ObjectVisibility, StorageError, StorageKey, StorageResult,
    UploadSession, UploadSessionId, UploadSessionStatus, UploadStrategy,
};

const MAX_UPLOAD_EXPIRES_AFTER_SECS: u64 = 100 * 365 * 24 * 60 * 60;

#[derive(Clone, Default)]
pub struct UploadSessionLockRegistry {
    locks: Arc<StdMutex<HashMap<String, Arc<TokioMutex<()>>>>>,
}

impl std::fmt::Debug for UploadSessionLockRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadSessionLockRegistry")
            .finish_non_exhaustive()
    }
}

impl UploadSessionLockRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lock(&self, session: &UploadSessionId) -> Arc<TokioMutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(session.as_str().to_string())
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    }

    pub fn drop_if_unused(&self, session: &UploadSessionId) {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = locks.get(session.as_str()) {
            if Arc::strong_count(existing) <= 2 {
                locks.remove(session.as_str());
            }
        }
    }
}

pub struct UploadSessionLockCleanup<'a> {
    registry: &'a UploadSessionLockRegistry,
    session: UploadSessionId,
}

impl<'a> UploadSessionLockCleanup<'a> {
    pub fn new(registry: &'a UploadSessionLockRegistry, session: &UploadSessionId) -> Self {
        Self {
            registry,
            session: session.clone(),
        }
    }
}

impl Drop for UploadSessionLockCleanup<'_> {
    fn drop(&mut self) {
        self.registry.drop_if_unused(&self.session);
    }
}

pub fn selected_strategy(strategy: UploadStrategy) -> StorageResult<UploadStrategy> {
    match strategy {
        UploadStrategy::Auto | UploadStrategy::Sequential => Ok(UploadStrategy::Sequential),
        UploadStrategy::SingleRequest | UploadStrategy::Multipart => {
            Err(StorageError::unsupported(
                "only sequential proxy uploads are implemented in pocopine-storage PR 1",
            ))
        }
    }
}

pub fn expires_at(duration: std::time::Duration) -> OffsetDateTime {
    let capped = duration.as_secs().min(MAX_UPLOAD_EXPIRES_AFTER_SECS);
    let now = OffsetDateTime::now_utc();
    now.checked_add(time::Duration::seconds(capped as i64))
        .unwrap_or(now)
}

pub fn ensure_owner(actor: &StorageActor, owner: &StorageActor) -> StorageResult<()> {
    if actor.same_owner(owner) {
        Ok(())
    } else {
        Err(StorageError::forbidden(
            "upload session belongs to a different storage actor",
        ))
    }
}

pub fn ensure_open(session: &UploadSession) -> StorageResult<()> {
    match session.status {
        UploadSessionStatus::Open => Ok(()),
        UploadSessionStatus::Complete => Err(StorageError::UploadComplete {
            session: session.id.to_string(),
        }),
        UploadSessionStatus::Aborted
        | UploadSessionStatus::Expired
        | UploadSessionStatus::Completing => {
            tracing::debug!(
                target: "pocopine.log",
                event_name = "pocopine.storage.upload_closed",
                session = %session.id,
                status = ?session.status,
            );
            Err(StorageError::UploadClosed {
                session: session.id.to_string(),
            })
        }
    }
}

pub fn refresh_expired(session: &mut UploadSession) -> bool {
    if session.status == UploadSessionStatus::Open
        && OffsetDateTime::now_utc() >= session.expires_at
    {
        session.status = UploadSessionStatus::Expired;
        true
    } else {
        false
    }
}

pub fn checked_new_offset(offset: u64, byte_count: usize) -> StorageResult<u64> {
    offset
        .checked_add(byte_count as u64)
        .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))
}

pub fn ensure_size_limit(
    max_bytes: u64,
    declared_size: Option<u64>,
    new_offset: u64,
) -> StorageResult<()> {
    if new_offset > max_bytes {
        return Err(StorageError::policy_rejected(format!(
            "upload exceeds scope max of {max_bytes} bytes"
        )));
    }
    if let Some(size) = declared_size {
        if new_offset > size {
            return Err(StorageError::policy_rejected(format!(
                "upload exceeds declared size of {size} bytes"
            )));
        }
    }
    Ok(())
}

pub fn ensure_upload_length_can_be_set(
    max_bytes: u64,
    session: &UploadSession,
    committed_offset: u64,
    size: u64,
) -> StorageResult<()> {
    ensure_open(session)?;
    if let Some(existing) = session.size {
        if existing == size {
            return Ok(());
        }
        return Err(StorageError::invalid_value(
            "Upload-Length",
            "cannot change upload length after it is set",
        ));
    }
    if size > max_bytes {
        return Err(StorageError::payload_too_large(max_bytes));
    }
    if committed_offset > size {
        return Err(StorageError::policy_rejected(format!(
            "upload length {size} is smaller than the committed offset {committed_offset}"
        )));
    }
    Ok(())
}

pub(crate) fn object_ref(
    backend: &str,
    session: &UploadSession,
    storage_key: &StorageKey,
    visibility: ObjectVisibility,
    request_metadata: &BTreeMap<String, String>,
    size: u64,
    checksum: Option<ObjectChecksum>,
) -> ObjectRef {
    let mut metadata = storage_key.metadata.0.clone();
    metadata.extend(request_metadata.clone());
    ObjectRef {
        backend: backend.to_string(),
        scope: session.scope.clone(),
        key: storage_key.key.to_string(),
        version: None,
        etag: None,
        checksum,
        content_type: session.content_type.clone(),
        size,
        visibility,
        metadata,
    }
}
