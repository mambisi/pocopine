use std::collections::BTreeMap;

use time::OffsetDateTime;

use crate::server::StorageActor;
use crate::{
    ObjectChecksum, ObjectRef, ObjectVisibility, StorageError, StorageKey, StorageResult,
    UploadSession, UploadSessionStatus, UploadStrategy,
};

pub(crate) fn selected_strategy(strategy: UploadStrategy) -> StorageResult<UploadStrategy> {
    match strategy {
        UploadStrategy::Auto | UploadStrategy::Sequential => Ok(UploadStrategy::Sequential),
        UploadStrategy::SingleRequest | UploadStrategy::Multipart => {
            Err(StorageError::unsupported(
                "only sequential proxy uploads are implemented in pocopine-storage PR 1",
            ))
        }
    }
}

pub(crate) fn expires_at(duration: std::time::Duration) -> OffsetDateTime {
    OffsetDateTime::now_utc()
        + time::Duration::seconds(duration.as_secs().min(i64::MAX as u64) as i64)
}

pub(crate) fn ensure_owner(actor: &StorageActor, owner: &StorageActor) -> StorageResult<()> {
    if actor.same_owner(owner) {
        Ok(())
    } else {
        Err(StorageError::forbidden(
            "upload session belongs to a different storage actor",
        ))
    }
}

pub(crate) fn ensure_open(session: &UploadSession) -> StorageResult<()> {
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

pub(crate) fn refresh_expired(session: &mut UploadSession) -> bool {
    if session.status == UploadSessionStatus::Open
        && OffsetDateTime::now_utc() >= session.expires_at
    {
        session.status = UploadSessionStatus::Expired;
        true
    } else {
        false
    }
}

pub(crate) fn checked_new_offset(offset: u64, byte_count: usize) -> StorageResult<u64> {
    offset
        .checked_add(byte_count as u64)
        .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))
}

pub(crate) fn ensure_size_limit(
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

pub(crate) fn ensure_upload_length_can_be_set(
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
