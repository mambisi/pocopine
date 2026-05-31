use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use time::OffsetDateTime;
use tokio::sync::{Mutex as TokioMutex, Semaphore};

use crate::server::StorageActor;
use crate::{
    BackendCapabilities, ObjectChecksum, ObjectRef, ObjectVisibility, StorageError, StorageKey,
    StorageResult, UploadSession, UploadSessionId, UploadSessionStatus, UploadStrategy,
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

/// Per-session permit pool bounding how many part uploads stream concurrently
/// for one upload session, enforcing the scope's advertised
/// `max_concurrent_parts` server-side (the client limit is otherwise only a
/// hint). Per-process, like [`UploadSessionLockRegistry`]: horizontally-scaled
/// replicas each enforce the limit independently.
#[derive(Clone, Default)]
pub struct UploadConcurrencyRegistry {
    permits: Arc<StdMutex<HashMap<String, Arc<Semaphore>>>>,
}

impl std::fmt::Debug for UploadConcurrencyRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadConcurrencyRegistry")
            .finish_non_exhaustive()
    }
}

impl UploadConcurrencyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The session's permit pool, created on first use sized to `max_concurrent`
    /// (clamped to at least one). All parts of a session share one pool, so the
    /// size is fixed by the first caller — which is correct since every part of a
    /// session carries the same advertised limit.
    pub fn semaphore(&self, session: &UploadSessionId, max_concurrent: usize) -> Arc<Semaphore> {
        let mut permits = self
            .permits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        permits
            .entry(session.as_str().to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(max_concurrent.max(1))))
            .clone()
    }

    /// Drop a finished session's permit pool (call on complete/abort).
    pub fn forget(&self, session: &UploadSessionId) {
        let mut permits = self
            .permits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        permits.remove(session.as_str());
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

/// Negotiate a requested [`UploadStrategy`] against what the backend supports,
/// returning the concrete strategy the session will run.
///
/// `Auto` resolves to the most capable server-mediated mode the backend
/// advertises (sequential proxy today; native multipart once a backend opts in).
/// A specific request that the backend cannot satisfy is rejected as
/// unsupported. Backends advertise [`BackendCapabilities`] via
/// `StorageBackend::capabilities()`, whose default is sequential-proxy-only — so
/// this preserves the previous behavior for every backend until one opts into
/// more.
pub fn select_upload_mode(
    requested: UploadStrategy,
    caps: BackendCapabilities,
) -> StorageResult<UploadStrategy> {
    match requested {
        UploadStrategy::Auto => {
            if caps.sequential_proxy {
                Ok(UploadStrategy::Sequential)
            } else if caps.native_multipart {
                Ok(UploadStrategy::Multipart)
            } else if caps.single_request {
                Ok(UploadStrategy::SingleRequest)
            } else {
                Err(StorageError::unsupported(
                    "storage backend advertises no upload mode",
                ))
            }
        }
        UploadStrategy::Sequential => caps
            .sequential_proxy
            .then_some(UploadStrategy::Sequential)
            .ok_or_else(|| {
                StorageError::unsupported(
                    "storage backend does not support sequential proxy uploads",
                )
            }),
        UploadStrategy::Multipart => caps
            .native_multipart
            .then_some(UploadStrategy::Multipart)
            .ok_or_else(|| {
                StorageError::unsupported("storage backend does not support multipart uploads")
            }),
        UploadStrategy::SingleRequest => caps
            .single_request
            .then_some(UploadStrategy::SingleRequest)
            .ok_or_else(|| {
                StorageError::unsupported("storage backend does not support single-request uploads")
            }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lock_registry_recovers_from_poisoned_map_mutex() {
        let registry = UploadSessionLockRegistry::new();
        let locks = registry.locks.clone();
        let poisoner = std::thread::spawn(move || {
            let _guard = locks.lock().expect("lock registry map");
            panic!("intentional registry map poison");
        });
        assert!(poisoner.join().is_err());

        let session = UploadSessionId::new("poisoned-map").expect("valid session id");
        let lock = registry.lock(&session);
        {
            let _guard = lock.try_lock().expect("session lock remains usable");
        }
        registry.drop_if_unused(&session);
    }

    #[test]
    fn sequential_only_caps_preserve_legacy_behavior() {
        // The default capabilities a backend advertises before opting into more.
        let caps = BackendCapabilities::default();
        assert_eq!(
            select_upload_mode(UploadStrategy::Auto, caps).unwrap(),
            UploadStrategy::Sequential
        );
        assert_eq!(
            select_upload_mode(UploadStrategy::Sequential, caps).unwrap(),
            UploadStrategy::Sequential
        );
        assert!(matches!(
            select_upload_mode(UploadStrategy::Multipart, caps),
            Err(StorageError::Unsupported { .. })
        ));
        assert!(matches!(
            select_upload_mode(UploadStrategy::SingleRequest, caps),
            Err(StorageError::Unsupported { .. })
        ));
    }

    #[test]
    fn capabilities_gate_each_strategy() {
        let multipart = BackendCapabilities {
            native_multipart: true,
            ..BackendCapabilities::default()
        };
        assert_eq!(
            select_upload_mode(UploadStrategy::Multipart, multipart).unwrap(),
            UploadStrategy::Multipart
        );
        // Auto still prefers sequential proxy when both are available.
        assert_eq!(
            select_upload_mode(UploadStrategy::Auto, multipart).unwrap(),
            UploadStrategy::Sequential
        );

        let single = BackendCapabilities {
            single_request: true,
            ..BackendCapabilities::default()
        };
        assert_eq!(
            select_upload_mode(UploadStrategy::SingleRequest, single).unwrap(),
            UploadStrategy::SingleRequest
        );

        // A backend with no modes rejects everything, including Auto.
        let none = BackendCapabilities {
            sequential_proxy: false,
            ..BackendCapabilities::default()
        };
        assert!(select_upload_mode(UploadStrategy::Auto, none).is_err());
    }
}
