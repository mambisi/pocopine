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
//!
//! This first adapter is deliberately a bounded sequential proxy backend. It
//! keeps staging bytes in memory while appending/completing and relies on
//! in-process per-session locks, so route a given upload session to one server
//! replica or wait for the future provider-side multipart backend before using
//! it for large or horizontally written uploads.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex as StdMutex};

use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::put_object::PutObjectError;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use bytes::Bytes;
use pocopine_storage::backend_common::{
    checked_new_offset, ensure_open, ensure_owner, ensure_size_limit,
    ensure_upload_length_can_be_set, expires_at, refresh_expired, selected_strategy,
};
use pocopine_storage::checksum::{ensure_supported_checksum_policy, validate_complete_checksum};
use pocopine_storage::{
    ChecksumPolicy, CompleteUpload, InitiateUpload, ObjectChecksum, ObjectRef, ObjectVisibility,
    StorageActor, StorageBackend, StorageBoxFuture, StorageContext, StorageError, StorageKey,
    StorageResult, TransferPlan, UploadSession, UploadSessionId, UploadSessionStatus,
    UploadStrategy,
};
use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

const DEFAULT_BACKEND_NAME: &str = "s3";
const DEFAULT_INTERNAL_PREFIX: &str = "__pocopine/storage/sessions";
const DEFAULT_MAX_PROXY_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;

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
    #[serde(default)]
    cleanup_pending: bool,
}

/// Storage backend backed by an S3-compatible object store.
///
/// The adapter stores temporary upload bytes as an internal object and rewrites
/// that object on each sequential `PATCH`. That keeps the first S3 backend
/// compatible with Pocopine's existing resumable upload protocol for bounded
/// uploads, but it is not the high-throughput/direct multipart path. A later
/// direct/multipart backend can add provider-side multipart uploads without
/// changing the public `StorageBackend` contract.
#[derive(Clone)]
pub struct S3StorageBackend {
    name: &'static str,
    client: Client,
    layout: S3KeyLayout,
    max_proxy_upload_bytes: u64,
    session_locks: Arc<StdMutex<HashMap<String, Arc<TokioMutex<()>>>>>,
}

impl std::fmt::Debug for S3StorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3StorageBackend")
            .field("name", &self.name)
            .field("bucket", &self.layout.bucket)
            .field("object_prefix", &self.layout.object_prefix)
            .field("internal_prefix", &self.layout.internal_prefix)
            .field("max_proxy_upload_bytes", &self.max_proxy_upload_bytes)
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
            max_proxy_upload_bytes: DEFAULT_MAX_PROXY_UPLOAD_BYTES,
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

    /// Override the maximum size accepted by this sequential proxy backend.
    ///
    /// The default is 64 MiB because each append rewrites the staged object and
    /// completion loads the staged bytes before the final S3 write. Larger
    /// values are possible, but applications should prefer the future multipart
    /// backend for large uploads.
    pub fn with_max_proxy_upload_bytes(mut self, max_bytes: u64) -> StorageResult<Self> {
        if max_bytes == 0 {
            return Err(StorageError::policy_rejected(
                "S3 max proxy upload bytes must be greater than zero",
            ));
        }
        self.max_proxy_upload_bytes = max_bytes;
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

    pub fn max_proxy_upload_bytes(&self) -> u64 {
        self.max_proxy_upload_bytes
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
        let staged = self.get_staged_bytes(session).await?;
        let actual = staged.len() as u64;
        if actual > trusted_len {
            tracing::warn!(
                target: "pocopine.log",
                event_name = "pocopine.storage.s3_staged_bytes_ahead",
                session = %session,
                actual,
                trusted = trusted_len,
            );
            return Ok(staged);
        }
        if actual == trusted_len {
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
        self.get_object_bytes_with_etag(key)
            .await
            .map(|(bytes, _etag)| bytes)
    }

    async fn get_object_bytes_with_etag(
        &self,
        key: &str,
    ) -> StorageResult<(Vec<u8>, Option<String>)> {
        let output = self
            .client
            .get_object()
            .bucket(&self.layout.bucket)
            .key(key)
            .send()
            .await
            .map_err(|err| {
                if is_get_object_not_found(&err) {
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
        Ok((bytes.to_vec(), output.e_tag.map(normalize_etag)))
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

    async fn put_completed_object(
        &self,
        key: &str,
        bytes: &[u8],
        content_type: Option<&str>,
    ) -> StorageResult<Option<String>> {
        match self.get_object_bytes_with_etag(key).await {
            Ok((existing, etag)) => {
                if existing == bytes {
                    return Ok(etag);
                }
                return Err(StorageError::policy_rejected(format!(
                    "S3 object key already exists with different bytes: {key}"
                )));
            }
            Err(StorageError::UnknownUploadSession { .. }) => {}
            Err(err) => return Err(err),
        }
        let mut request = self
            .client
            .put_object()
            .bucket(&self.layout.bucket)
            .key(key)
            .if_none_match("*")
            .body(ByteStream::from(bytes.to_vec()));
        if let Some(content_type) = content_type {
            request = request.content_type(content_type);
        }
        match request.send().await {
            Ok(output) => Ok(output.e_tag.map(normalize_etag)),
            Err(err) if is_put_precondition_failed(&err) => {
                let (existing, etag) = self.get_object_bytes_with_etag(key).await?;
                if existing == bytes {
                    Ok(etag)
                } else {
                    Err(StorageError::policy_rejected(format!(
                        "S3 object key already exists with different bytes: {key}"
                    )))
                }
            }
            Err(err) => Err(s3_error("put completed object", err)),
        }
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

    async fn cleanup_staged_bytes(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
    ) -> StorageResult<()> {
        if !stored.cleanup_pending {
            return Ok(());
        }
        self.delete_object(&self.layout.session_bytes_key(session))
            .await?;
        stored.cleanup_pending = false;
        self.write_session(session, stored).await
    }

    fn ensure_requested_size_is_supported(&self, size: Option<u64>) -> StorageResult<()> {
        if let Some(size) = size {
            if size > self.max_proxy_upload_bytes {
                return Err(StorageError::payload_too_large(self.max_proxy_upload_bytes));
            }
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

struct SessionLockCleanup<'a> {
    backend: &'a S3StorageBackend,
    session: UploadSessionId,
}

impl<'a> SessionLockCleanup<'a> {
    fn new(backend: &'a S3StorageBackend, session: &UploadSessionId) -> Self {
        Self {
            backend,
            session: session.clone(),
        }
    }
}

impl Drop for SessionLockCleanup<'_> {
    fn drop(&mut self) {
        self.backend.drop_session_lock(&self.session);
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
            ensure_supported_checksum_policy(&request.policy.checksum)?;
            self.ensure_requested_size_is_supported(request.size)?;
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
                max_bytes: request.policy.max_bytes.min(self.max_proxy_upload_bytes),
                checksum_policy: request.policy.checksum,
                request_metadata: request.metadata,
                object: None,
                cleanup_pending: false,
            };
            self.write_session(&id, &stored).await?;
            if let Err(err) = self.put_staged_bytes(&id, Vec::new()).await {
                let _ = self.delete_object(&self.layout.session_meta_key(&id)).await;
                return Err(err);
            }
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
            let _cleanup = SessionLockCleanup::new(self, &session);
            let _guard = lock.lock().await;
            let mut stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            if stored.public.status == UploadSessionStatus::Open {
                let staged = self
                    .reconcile_staged_bytes(&session, stored.public.next_offset.unwrap_or(0))
                    .await?;
                let staged_len = staged.len() as u64;
                if stored.public.next_offset != Some(staged_len) {
                    stored.public.next_offset = Some(staged_len);
                    self.write_session(&session, &stored).await?;
                }
            }
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
            let _cleanup = SessionLockCleanup::new(self, &session);
            let _guard = lock.lock().await;
            self.ensure_requested_size_is_supported(Some(size))?;
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
            let _cleanup = SessionLockCleanup::new(self, &session);
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
            let mut expected = stored.public.next_offset.unwrap_or(0);
            let mut staged = self.reconcile_staged_bytes(&session, expected).await?;
            let staged_len = staged.len() as u64;
            if staged_len > expected {
                expected = staged_len;
                stored.public.next_offset = Some(staged_len);
                self.write_session(&session, &stored).await?;
            }
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
            let _cleanup = SessionLockCleanup::new(self, &request.session);
            let _guard = lock.lock().await;
            let mut stored = self.read_session(&request.session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            if let Some(object) = &stored.object {
                let object = object.clone();
                self.cleanup_staged_bytes(&request.session, &mut stored)
                    .await?;
                return Ok(object);
            }
            self.persist_expired_if_needed(&request.session, &stored)
                .await?;
            ensure_completable(&stored.public)?;
            let trusted = stored.public.next_offset.unwrap_or(0);
            let staged = self
                .reconcile_staged_bytes(&request.session, trusted)
                .await?;
            let actual = staged.len() as u64;
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
            if stored.public.status == UploadSessionStatus::Open
                || stored.public.next_offset != Some(actual)
            {
                stored.public.status = UploadSessionStatus::Completing;
                stored.public.next_offset = Some(actual);
                self.write_session(&request.session, &stored).await?;
            }
            let etag = self
                .put_completed_object(&object_key, &staged, stored.public.content_type.as_deref())
                .await?;
            let object = self.object_ref(&stored, actual, etag, checksum);
            stored.public.status = UploadSessionStatus::Complete;
            stored.public.next_offset = Some(actual);
            stored.object = Some(object.clone());
            stored.cleanup_pending = true;
            self.write_session(&request.session, &stored).await?;
            self.cleanup_staged_bytes(&request.session, &mut stored)
                .await?;
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
            let _cleanup = SessionLockCleanup::new(self, &session);
            let _guard = lock.lock().await;
            let known_session = match self.read_session(&session).await {
                Ok(stored) => {
                    ensure_owner(&ctx.actor, &stored.owner)?;
                    true
                }
                Err(StorageError::UnknownUploadSession { .. }) => false,
                Err(err) => return Err(err),
            };
            let meta_key = self.layout.session_meta_key(&session);
            let bytes_key = self.layout.session_bytes_key(&session);
            self.delete_object(&bytes_key).await?;
            if known_session {
                self.delete_object(&meta_key).await?;
            }
            Ok(())
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct S3KeyLayout {
    bucket: String,
    object_prefix: Option<String>,
    internal_prefix_base: String,
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
        let internal_prefix_base = normalize_prefix(internal_prefix)
            .ok_or_else(|| StorageError::policy_rejected("S3 internal prefix must not be empty"))?;
        let internal_prefix =
            join_optional_prefix(object_prefix.as_deref(), internal_prefix_base.as_str());
        Ok(Self {
            bucket,
            object_prefix,
            internal_prefix_base,
            internal_prefix,
        })
    }

    fn with_prefix(&self, prefix: String) -> StorageResult<Self> {
        let object_prefix = normalize_prefix(prefix);
        Self::new(
            self.bucket.clone(),
            object_prefix,
            self.internal_prefix_base.clone(),
        )
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

fn ensure_completable(session: &UploadSession) -> StorageResult<()> {
    if session.status == UploadSessionStatus::Completing {
        Ok(())
    } else {
        ensure_open(session)
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

fn is_get_object_not_found(err: &SdkError<GetObjectError>) -> bool {
    err.as_service_error()
        .is_some_and(GetObjectError::is_no_such_key)
}

fn is_put_precondition_failed(err: &SdkError<PutObjectError>) -> bool {
    err.as_service_error()
        .and_then(|err| err.meta().code())
        .is_some_and(|code| code == "PreconditionFailed")
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
            DEFAULT_INTERNAL_PREFIX.to_string(),
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
    fn prefix_and_internal_prefix_are_order_independent() {
        let first = S3KeyLayout::new(
            "bucket".to_string(),
            None,
            DEFAULT_INTERNAL_PREFIX.to_string(),
        )
        .unwrap()
        .with_internal_prefix("custom/sessions".to_string())
        .unwrap()
        .with_prefix("tenant-a".to_string())
        .unwrap();
        let second = S3KeyLayout::new(
            "bucket".to_string(),
            None,
            DEFAULT_INTERNAL_PREFIX.to_string(),
        )
        .unwrap()
        .with_prefix("tenant-a".to_string())
        .unwrap()
        .with_internal_prefix("custom/sessions".to_string())
        .unwrap();

        assert_eq!(first.internal_prefix, "tenant-a/custom/sessions");
        assert_eq!(second.internal_prefix, first.internal_prefix);
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
