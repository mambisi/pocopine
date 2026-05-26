//! S3-compatible backend adapter for `pocopine-storage`.
//!
//! This crate intentionally implements Pocopine's current sequential proxy
//! upload contract. Completed objects are written to the configured bucket by
//! `SafeObjectKey`, while upload-session metadata and staging bytes live under
//! an internal prefix in the same bucket.
//!
//! For S3-compatible services such as MinIO, build an `aws_sdk_s3::Client`
//! with the provider-specific endpoint/path-style settings and pass it to
//! [`S3StorageBackend::new`].

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex as StdMutex};

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use bytes::Bytes;
use pocopine_storage::{
    ChecksumAlgorithm, ChecksumPolicy, CompleteUpload, InitiateUpload, ObjectChecksum, ObjectRef,
    ObjectVisibility, StorageActor, StorageBackend, StorageBoxFuture, StorageContext, StorageError,
    StorageKey, StorageResult, TransferPlan, UploadSession, UploadSessionId, UploadSessionStatus,
    UploadStrategy,
};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

const DEFAULT_BACKEND_NAME: &str = "s3";
const DEFAULT_INTERNAL_PREFIX: &str = "__pocopine/storage/sessions";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredUploadSession {
    public: UploadSession,
    owner: StorageActor,
    storage_key: StorageKey,
    visibility: ObjectVisibility,
    max_bytes: u64,
    checksum_policy: ChecksumPolicy,
    request_metadata: BTreeMap<String, String>,
    object: Option<ObjectRef>,
}

/// Storage backend backed by an S3-compatible object store.
///
/// The adapter stores temporary upload bytes as an internal object and rewrites
/// that object on each sequential `PATCH`. That keeps the first S3 backend
/// compatible with Pocopine's existing resumable upload protocol, but it is
/// not the high-throughput/direct multipart path. A later direct/multipart
/// backend can add provider-side multipart uploads without changing the public
/// `StorageBackend` contract.
#[derive(Clone)]
pub struct S3StorageBackend {
    name: &'static str,
    client: Client,
    layout: S3KeyLayout,
    session_locks: Arc<StdMutex<HashMap<String, Arc<TokioMutex<()>>>>>,
}

impl std::fmt::Debug for S3StorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3StorageBackend")
            .field("name", &self.name)
            .field("bucket", &self.layout.bucket)
            .field("object_prefix", &self.layout.object_prefix)
            .field("internal_prefix", &self.layout.internal_prefix)
            .finish_non_exhaustive()
    }
}

impl S3StorageBackend {
    /// Build a backend named `s3` for the given bucket.
    pub fn new(client: Client, bucket: impl Into<String>) -> StorageResult<Self> {
        Self::named(DEFAULT_BACKEND_NAME, client, bucket)
    }

    /// Build a backend with an explicit registry name.
    pub fn named(
        name: &'static str,
        client: Client,
        bucket: impl Into<String>,
    ) -> StorageResult<Self> {
        Ok(Self {
            name,
            client,
            layout: S3KeyLayout::new(bucket.into(), None, DEFAULT_INTERNAL_PREFIX.to_string())?,
            session_locks: Arc::new(StdMutex::new(HashMap::new())),
        })
    }

    /// Store completed objects below a bucket prefix.
    ///
    /// The internal session prefix is also nested under this prefix to keep all
    /// Pocopine-owned keys together.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> StorageResult<Self> {
        self.layout = self.layout.with_prefix(prefix.into())?;
        Ok(self)
    }

    /// Override the internal upload-session prefix.
    pub fn with_internal_prefix(mut self, prefix: impl Into<String>) -> StorageResult<Self> {
        self.layout = self.layout.with_internal_prefix(prefix.into())?;
        Ok(self)
    }

    pub fn bucket(&self) -> &str {
        &self.layout.bucket
    }

    pub fn object_prefix(&self) -> Option<&str> {
        self.layout.object_prefix.as_deref()
    }

    pub fn internal_prefix(&self) -> &str {
        &self.layout.internal_prefix
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
        if let Some(existing) = locks.get(session.as_str()) {
            if Arc::strong_count(existing) <= 2 {
                locks.remove(session.as_str());
            }
        }
    }

    async fn read_session(&self, session: &UploadSessionId) -> StorageResult<StoredUploadSession> {
        let key = self.layout.session_meta_key(session);
        let bytes = match self.get_object_bytes(&key).await {
            Ok(bytes) => bytes,
            Err(StorageError::UnknownUploadSession { .. }) => {
                return Err(StorageError::unknown_upload_session(session.to_string()));
            }
            Err(err) => return Err(err),
        };
        let mut stored: StoredUploadSession = serde_json::from_slice(&bytes)
            .map_err(|err| StorageError::backend(format!("read s3 upload metadata: {err}")))?;
        refresh_expired(&mut stored.public);
        Ok(stored)
    }

    async fn write_session(
        &self,
        session: &UploadSessionId,
        stored: &StoredUploadSession,
    ) -> StorageResult<()> {
        let key = self.layout.session_meta_key(session);
        let bytes = serde_json::to_vec_pretty(stored)
            .map_err(|err| StorageError::backend(format!("encode s3 upload metadata: {err}")))?;
        self.put_object_bytes(&key, bytes, Some("application/json"))
            .await
            .map(|_| ())
    }

    async fn get_staged_bytes(&self, session: &UploadSessionId) -> StorageResult<Vec<u8>> {
        let key = self.layout.session_bytes_key(session);
        match self.get_object_bytes(&key).await {
            Ok(bytes) => Ok(bytes),
            Err(StorageError::UnknownUploadSession { .. }) => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }

    async fn reconcile_staged_bytes(
        &self,
        session: &UploadSessionId,
        trusted_len: u64,
    ) -> StorageResult<Vec<u8>> {
        let mut staged = self.get_staged_bytes(session).await?;
        let actual = staged.len() as u64;
        if actual == trusted_len {
            return Ok(staged);
        }
        if actual > trusted_len {
            tracing::warn!(
                target: "pocopine.log",
                event_name = "pocopine.storage.truncate_uncommitted_s3_bytes",
                session = %session,
                actual,
                trusted = trusted_len,
            );
            staged.truncate(trusted_len as usize);
            self.put_staged_bytes(session, staged.clone()).await?;
            return Ok(staged);
        }
        Err(StorageError::backend(format!(
            "S3 staged upload object is shorter than committed metadata: expected {trusted_len} bytes, got {actual}"
        )))
    }

    async fn put_staged_bytes(
        &self,
        session: &UploadSessionId,
        bytes: Vec<u8>,
    ) -> StorageResult<()> {
        let key = self.layout.session_bytes_key(session);
        self.put_object_bytes(&key, bytes, Some("application/octet-stream"))
            .await
            .map(|_| ())
    }

    async fn get_object_bytes(&self, key: &str) -> StorageResult<Vec<u8>> {
        let output = self
            .client
            .get_object()
            .bucket(&self.layout.bucket)
            .key(key)
            .send()
            .await
            .map_err(|err| {
                if is_s3_not_found(&err) {
                    StorageError::unknown_upload_session(key.to_string())
                } else {
                    s3_error("get object", err)
                }
            })?;
        let bytes = output
            .body
            .collect()
            .await
            .map_err(|err| s3_error("read object body", err))?
            .into_bytes();
        Ok(bytes.to_vec())
    }

    async fn put_object_bytes(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: Option<&str>,
    ) -> StorageResult<Option<String>> {
        let mut request = self
            .client
            .put_object()
            .bucket(&self.layout.bucket)
            .key(key)
            .body(ByteStream::from(bytes));
        if let Some(content_type) = content_type {
            request = request.content_type(content_type);
        }
        let output = request
            .send()
            .await
            .map_err(|err| s3_error("put object", err))?;
        Ok(output.e_tag.map(normalize_etag))
    }

    async fn delete_object(&self, key: &str) -> StorageResult<()> {
        self.client
            .delete_object()
            .bucket(&self.layout.bucket)
            .key(key)
            .send()
            .await
            .map_err(|err| s3_error("delete object", err))?;
        Ok(())
    }

    async fn persist_expired_if_needed(
        &self,
        session: &UploadSessionId,
        stored: &StoredUploadSession,
    ) -> StorageResult<()> {
        if stored.public.status == UploadSessionStatus::Expired {
            self.write_session(session, stored).await?;
        }
        Ok(())
    }

    fn object_ref(
        &self,
        stored: &StoredUploadSession,
        size: u64,
        etag: Option<String>,
        checksum: Option<ObjectChecksum>,
    ) -> ObjectRef {
        let mut metadata = stored.storage_key.metadata.0.clone();
        metadata.extend(stored.request_metadata.clone());
        ObjectRef {
            backend: self.name.to_string(),
            scope: stored.public.scope.clone(),
            key: stored.storage_key.key.to_string(),
            version: None,
            etag,
            checksum,
            content_type: stored.public.content_type.clone(),
            size,
            visibility: stored.visibility,
            metadata,
        }
    }
}

impl StorageBackend for S3StorageBackend {
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
            self.put_staged_bytes(&id, Vec::new()).await?;
            self.write_session(&id, &stored).await?;
            Ok(session)
        })
    }

    fn inspect_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let lock = self.session_lock(&session);
            let _guard = lock.lock().await;
            let stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            Ok(stored.public)
        })
    }

    fn set_upload_length<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        size: u64,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let lock = self.session_lock(&session);
            let _guard = lock.lock().await;
            let mut stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            self.persist_expired_if_needed(&session, &stored).await?;
            let committed_offset = stored.public.next_offset.unwrap_or(0);
            let staged_len = self
                .reconcile_staged_bytes(&session, committed_offset)
                .await?
                .len() as u64;
            ensure_upload_length_can_be_set(stored.max_bytes, &stored.public, staged_len, size)?;
            stored.public.size = Some(size);
            stored.public.next_offset = Some(staged_len);
            self.write_session(&session, &stored).await?;
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
            let lock = self.session_lock(&session);
            let _guard = lock.lock().await;
            let mut stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            self.persist_expired_if_needed(&session, &stored).await?;
            ensure_open(&stored.public)?;
            if stored.public.strategy != UploadStrategy::Sequential {
                return Err(StorageError::unsupported(
                    "S3 backend currently supports sequential proxy uploads only",
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
            let mut staged = self.reconcile_staged_bytes(&session, expected).await?;
            let new_offset = checked_new_offset(offset, bytes.len())?;
            ensure_size_limit(stored.max_bytes, stored.public.size, new_offset)?;
            staged.extend_from_slice(&bytes);
            self.put_staged_bytes(&session, staged).await?;
            stored.public.next_offset = Some(new_offset);
            self.write_session(&session, &stored).await?;
            Ok(stored.public)
        })
    }

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, ObjectRef> {
        Box::pin(async move {
            let lock = self.session_lock(&request.session);
            let _guard = lock.lock().await;
            let mut stored = self.read_session(&request.session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            if let Some(object) = &stored.object {
                return Ok(object.clone());
            }
            self.persist_expired_if_needed(&request.session, &stored)
                .await?;
            ensure_open(&stored.public)?;
            let actual = stored.public.next_offset.unwrap_or(0);
            let staged = self
                .reconcile_staged_bytes(&request.session, actual)
                .await?;
            if let Some(expected) = stored.public.size {
                if actual != expected {
                    return Err(StorageError::policy_rejected(format!(
                        "upload is incomplete: expected {expected} bytes, got {actual}"
                    )));
                }
            }
            ensure_size_limit(stored.max_bytes, stored.public.size, actual)?;
            let checksum =
                validate_complete_checksum(&stored.checksum_policy, &staged, request.checksum)?;
            let object_key = self.layout.object_key(stored.storage_key.key.as_str());
            let etag = self
                .put_object_bytes(&object_key, staged, stored.public.content_type.as_deref())
                .await?;
            let object = self.object_ref(&stored, actual, etag, checksum);
            stored.public.status = UploadSessionStatus::Complete;
            stored.public.next_offset = Some(actual);
            stored.object = Some(object.clone());
            self.write_session(&request.session, &stored).await?;
            let _ = self
                .delete_object(&self.layout.session_bytes_key(&request.session))
                .await;
            Ok(object)
        })
    }

    fn abort_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, ()> {
        Box::pin(async move {
            let lock = self.session_lock(&session);
            let guard = lock.lock().await;
            match self.read_session(&session).await {
                Ok(stored) => ensure_owner(&ctx.actor, &stored.owner)?,
                Err(StorageError::UnknownUploadSession { .. }) => return Ok(()),
                Err(err) => return Err(err),
            }
            let meta_key = self.layout.session_meta_key(&session);
            let bytes_key = self.layout.session_bytes_key(&session);
            self.delete_object(&meta_key).await?;
            self.delete_object(&bytes_key).await?;
            drop(guard);
            self.drop_session_lock(&session);
            Ok(())
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct S3KeyLayout {
    bucket: String,
    object_prefix: Option<String>,
    internal_prefix: String,
}

impl S3KeyLayout {
    fn new(
        bucket: String,
        object_prefix: Option<String>,
        internal_prefix: String,
    ) -> StorageResult<Self> {
        let bucket = bucket.trim().to_string();
        if bucket.is_empty() {
            return Err(StorageError::policy_rejected(
                "S3 bucket name must not be empty",
            ));
        }
        let object_prefix = object_prefix.and_then(normalize_prefix);
        let internal_prefix = normalize_prefix(internal_prefix)
            .ok_or_else(|| StorageError::policy_rejected("S3 internal prefix must not be empty"))?;
        Ok(Self {
            bucket,
            object_prefix,
            internal_prefix,
        })
    }

    fn with_prefix(&self, prefix: String) -> StorageResult<Self> {
        let object_prefix = normalize_prefix(prefix);
        let internal_prefix = match &object_prefix {
            Some(prefix) => format!("{prefix}/{DEFAULT_INTERNAL_PREFIX}"),
            None => DEFAULT_INTERNAL_PREFIX.to_string(),
        };
        Self::new(self.bucket.clone(), object_prefix, internal_prefix)
    }

    fn with_internal_prefix(&self, prefix: String) -> StorageResult<Self> {
        Self::new(
            self.bucket.clone(),
            self.object_prefix.clone(),
            prefix.trim_matches('/').to_string(),
        )
    }

    fn object_key(&self, key: &str) -> String {
        join_optional_prefix(self.object_prefix.as_deref(), key)
    }

    fn session_meta_key(&self, session: &UploadSessionId) -> String {
        format!("{}/{}/session.json", self.internal_prefix, session.as_str())
    }

    fn session_bytes_key(&self, session: &UploadSessionId) -> String {
        format!("{}/{}/bytes.tmp", self.internal_prefix, session.as_str())
    }
}

fn selected_strategy(strategy: UploadStrategy) -> StorageResult<UploadStrategy> {
    match strategy {
        UploadStrategy::Auto | UploadStrategy::Sequential => Ok(UploadStrategy::Sequential),
        UploadStrategy::SingleRequest | UploadStrategy::Multipart => {
            Err(StorageError::unsupported(
                "S3 backend currently supports sequential proxy uploads only",
            ))
        }
        _ => Err(StorageError::unsupported(
            "S3 backend currently supports sequential proxy uploads only",
        )),
    }
}

fn expires_at(duration: std::time::Duration) -> OffsetDateTime {
    OffsetDateTime::now_utc()
        + time::Duration::seconds(duration.as_secs().min(i64::MAX as u64) as i64)
}

fn ensure_owner(actor: &StorageActor, owner: &StorageActor) -> StorageResult<()> {
    if actor.same_owner(owner) {
        Ok(())
    } else {
        Err(StorageError::forbidden(
            "upload session belongs to a different storage actor",
        ))
    }
}

fn ensure_open(session: &UploadSession) -> StorageResult<()> {
    match session.status {
        UploadSessionStatus::Open => Ok(()),
        UploadSessionStatus::Complete => Err(StorageError::UploadComplete {
            session: session.id.to_string(),
        }),
        UploadSessionStatus::Aborted
        | UploadSessionStatus::Expired
        | UploadSessionStatus::Completing => Err(StorageError::UploadClosed {
            session: session.id.to_string(),
        }),
        _ => Err(StorageError::UploadClosed {
            session: session.id.to_string(),
        }),
    }
}

fn refresh_expired(session: &mut UploadSession) -> bool {
    if session.status == UploadSessionStatus::Open
        && OffsetDateTime::now_utc() >= session.expires_at
    {
        session.status = UploadSessionStatus::Expired;
        true
    } else {
        false
    }
}

fn checked_new_offset(offset: u64, byte_count: usize) -> StorageResult<u64> {
    offset
        .checked_add(byte_count as u64)
        .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))
}

fn ensure_size_limit(
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

fn ensure_upload_length_can_be_set(
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
        return Err(StorageError::policy_rejected(
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

fn validate_complete_checksum(
    policy: &ChecksumPolicy,
    bytes: &[u8],
    provided: Option<ObjectChecksum>,
) -> StorageResult<Option<ObjectChecksum>> {
    match policy {
        ChecksumPolicy::None => Ok(None),
        ChecksumPolicy::Optional(allowed) => {
            if let Some(checksum) = &provided {
                if !allowed.contains(&checksum.algorithm) {
                    return Err(StorageError::policy_rejected(
                        "checksum algorithm is not allowed",
                    ));
                }
                let computed = compute_checksum(checksum.algorithm, bytes)?;
                if !checksum.value.eq_ignore_ascii_case(&computed.value) {
                    return Err(StorageError::policy_rejected(
                        "upload checksum does not match uploaded bytes",
                    ));
                }
                return Ok(Some(computed));
            }
            Ok(None)
        }
        ChecksumPolicy::Required(algorithm) => {
            let provided = provided.ok_or_else(|| {
                StorageError::policy_rejected("required upload checksum is missing")
            })?;
            if provided.algorithm != *algorithm {
                return Err(StorageError::policy_rejected(
                    "required upload checksum algorithm does not match policy",
                ));
            }
            let computed = compute_checksum(*algorithm, bytes)?;
            if !provided.value.eq_ignore_ascii_case(&computed.value) {
                return Err(StorageError::policy_rejected(
                    "required upload checksum does not match uploaded bytes",
                ));
            }
            Ok(Some(computed))
        }
        _ => Err(StorageError::unsupported(
            "unsupported checksum policy for S3 backend",
        )),
    }
}

fn compute_checksum(algorithm: ChecksumAlgorithm, bytes: &[u8]) -> StorageResult<ObjectChecksum> {
    match algorithm {
        ChecksumAlgorithm::Sha256 => {
            let digest = Sha256::digest(bytes);
            let mut value = String::with_capacity(digest.len() * 2);
            for byte in digest {
                use std::fmt::Write as _;
                write!(&mut value, "{byte:02x}")
                    .map_err(|err| StorageError::backend(format!("format checksum: {err}")))?;
            }
            Ok(ObjectChecksum { algorithm, value })
        }
        ChecksumAlgorithm::Crc32c | ChecksumAlgorithm::Md5 => Err(StorageError::unsupported(
            "only sha256 checksum verification is implemented for S3 backend checksums",
        )),
        _ => Err(StorageError::unsupported(
            "only sha256 checksum verification is implemented for S3 backend checksums",
        )),
    }
}

fn normalize_prefix(prefix: String) -> Option<String> {
    let prefix = prefix.trim().trim_matches('/');
    if prefix.is_empty() {
        None
    } else {
        Some(prefix.to_string())
    }
}

fn join_optional_prefix(prefix: Option<&str>, key: &str) -> String {
    match prefix {
        Some(prefix) => format!("{prefix}/{}", key.trim_start_matches('/')),
        None => key.trim_start_matches('/').to_string(),
    }
}

fn normalize_etag(etag: String) -> String {
    etag.trim_matches('"').to_string()
}

fn is_s3_not_found(err: &impl std::fmt::Display) -> bool {
    let message = err.to_string();
    message.contains("NoSuchKey")
        || message.contains("NotFound")
        || message.contains("404")
        || message.contains("status code: 404")
}

fn s3_error(operation: &'static str, err: impl std::fmt::Display) -> StorageError {
    tracing::error!(
        target: "pocopine.log",
        event_name = "pocopine.storage.s3_error",
        operation,
        error = %err,
    );
    StorageError::backend(format!("S3 {operation}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_layout_keeps_internal_objects_out_of_app_keyspace() {
        let layout = S3KeyLayout::new(
            "bucket".to_string(),
            Some("tenant-a/".to_string()),
            "tenant-a/__pocopine/storage/sessions".to_string(),
        )
        .unwrap();
        let session = UploadSessionId::new("session-1").unwrap();

        assert_eq!(
            layout.object_key("files/avatar.png"),
            "tenant-a/files/avatar.png"
        );
        assert_eq!(
            layout.session_meta_key(&session),
            "tenant-a/__pocopine/storage/sessions/session-1/session.json"
        );
        assert_eq!(
            layout.session_bytes_key(&session),
            "tenant-a/__pocopine/storage/sessions/session-1/bytes.tmp"
        );
    }

    #[test]
    fn empty_bucket_is_rejected() {
        assert!(
            S3KeyLayout::new("  ".to_string(), None, DEFAULT_INTERNAL_PREFIX.to_string()).is_err()
        );
    }

    #[test]
    fn etag_quotes_are_removed() {
        assert_eq!(normalize_etag("\"abc123\"".to_string()), "abc123");
    }
}
