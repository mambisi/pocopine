use bytes::Bytes;
use google_cloud_storage::client::{Storage, StorageControl};
use pocopine_storage::backend_common::{
    checked_new_offset, ensure_open, ensure_owner, ensure_size_limit,
    ensure_upload_length_can_be_set, expires_at, selected_strategy, UploadSessionLockCleanup,
    UploadSessionLockRegistry,
};
use pocopine_storage::checksum::{ensure_supported_checksum_policy, validate_complete_checksum};
use pocopine_storage::{
    CompleteUpload, InitiateUpload, ObjectChecksum, ObjectRef, StorageActor, StorageBackend,
    StorageBoxFuture, StorageContext, StorageError, StorageResult, TransferPlan, UploadSession,
    UploadSessionId, UploadSessionStatus, UploadStrategy,
};
use uuid::Uuid;

use crate::control::{GcsControl, GcsJsonControl};
use crate::layout::{GcsKeyLayout, DEFAULT_INTERNAL_PREFIX};
use crate::state::{
    decode_session_object, AbortSessionRead, GcsObjectBytes, GcsObjectMetadata, GcsObjectWrite,
    StoredUploadSession,
};
use crate::util::{
    bytes_match_at, ensure_completable, gcs_error, is_gcs_not_found, is_gcs_precondition_failed,
    map_session_write_error, non_empty, positive_generation, usize_from_u64,
};

const DEFAULT_BACKEND_NAME: &str = "gcs";
const DEFAULT_MAX_PROXY_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SESSION_METADATA_BYTES: u64 = 256 * 1024;

/// Storage backend backed by Google Cloud Storage.
///
/// Pass pre-built [`Storage`] and [`StorageControl`] clients so applications can
/// configure authentication, endpoints, tracing, retry policies, and emulators
/// using Google's client builders.
#[derive(Clone)]
pub struct GcsStorageBackend {
    name: &'static str,
    storage: Storage,
    control: GcsControl,
    layout: GcsKeyLayout,
    max_proxy_upload_bytes: u64,
    session_locks: UploadSessionLockRegistry,
}

impl std::fmt::Debug for GcsStorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsStorageBackend")
            .field("name", &self.name)
            .field("bucket", &self.layout.bucket)
            .field("bucket_resource", &self.layout.bucket_resource)
            .field("object_prefix", &self.layout.object_prefix)
            .field("internal_prefix", &self.layout.internal_prefix)
            .field("max_proxy_upload_bytes", &self.max_proxy_upload_bytes)
            .finish_non_exhaustive()
    }
}

impl GcsStorageBackend {
    /// Build a backend named `gcs` for the given bucket.
    ///
    /// `bucket` may be either a plain bucket id or a resource name such as
    /// `projects/_/buckets/my-bucket`.
    pub fn new(
        storage: Storage,
        control: StorageControl,
        bucket: impl Into<String>,
    ) -> StorageResult<Self> {
        Self::named(DEFAULT_BACKEND_NAME, storage, control, bucket)
    }

    /// Build a backend named `gcs` for a GCS JSON API emulator.
    ///
    /// This keeps production code on Google's `StorageControl` client while
    /// allowing local integration tests against emulators such as
    /// `fsouza/fake-gcs-server`, which implement the JSON API but not the GCS
    /// gRPC control API.
    pub fn emulator(
        storage: Storage,
        endpoint: impl Into<String>,
        bucket: impl Into<String>,
    ) -> StorageResult<Self> {
        Self::named_for_emulator(DEFAULT_BACKEND_NAME, storage, endpoint, bucket)
    }

    /// Build a backend with an explicit registry name.
    pub fn named(
        name: &'static str,
        storage: Storage,
        control: StorageControl,
        bucket: impl Into<String>,
    ) -> StorageResult<Self> {
        Ok(Self {
            name,
            storage,
            control: GcsControl::Google(control),
            layout: GcsKeyLayout::new(bucket.into(), None, DEFAULT_INTERNAL_PREFIX.to_string())?,
            max_proxy_upload_bytes: DEFAULT_MAX_PROXY_UPLOAD_BYTES,
            session_locks: UploadSessionLockRegistry::new(),
        })
    }

    /// Build an emulator-backed backend with an explicit registry name.
    pub fn named_for_emulator(
        name: &'static str,
        storage: Storage,
        endpoint: impl Into<String>,
        bucket: impl Into<String>,
    ) -> StorageResult<Self> {
        Ok(Self {
            name,
            storage,
            control: GcsControl::Json(GcsJsonControl::new(endpoint.into())?),
            layout: GcsKeyLayout::new(bucket.into(), None, DEFAULT_INTERNAL_PREFIX.to_string())?,
            max_proxy_upload_bytes: DEFAULT_MAX_PROXY_UPLOAD_BYTES,
            session_locks: UploadSessionLockRegistry::new(),
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
    /// completion loads the staged bytes before the final GCS write.
    pub fn with_max_proxy_upload_bytes(mut self, max_bytes: u64) -> StorageResult<Self> {
        if max_bytes == 0 {
            return Err(StorageError::policy_rejected(
                "GCS max proxy upload bytes must be greater than zero",
            ));
        }
        self.max_proxy_upload_bytes = max_bytes;
        Ok(self)
    }

    pub fn bucket(&self) -> &str {
        &self.layout.bucket
    }

    pub fn bucket_resource(&self) -> &str {
        &self.layout.bucket_resource
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

    async fn read_session(&self, session: &UploadSessionId) -> StorageResult<StoredUploadSession> {
        match self.read_session_object(session).await {
            Ok(object) => decode_session_object(object)
                .map_err(|err| StorageError::backend(format!("read gcs upload metadata: {err}"))),
            Err(StorageError::UnknownUploadSession { .. }) => {
                Err(StorageError::unknown_upload_session(session.to_string()))
            }
            Err(err) => Err(err),
        }
    }

    async fn read_session_for_abort(
        &self,
        session: &UploadSessionId,
    ) -> StorageResult<AbortSessionRead> {
        match self.read_session_object(session).await {
            Ok(object) => match decode_session_object(object) {
                Ok(stored) => Ok(AbortSessionRead::Known(Box::new(stored))),
                Err(err) => {
                    tracing::warn!(
                        target: "pocopine.log",
                        event_name = "pocopine.storage.gcs_corrupt_upload_metadata",
                        session = %session,
                        error = %err,
                    );
                    Ok(AbortSessionRead::Corrupt)
                }
            },
            Err(StorageError::UnknownUploadSession { .. }) => Ok(AbortSessionRead::Missing),
            Err(err) => Err(err),
        }
    }

    async fn read_session_object(
        &self,
        session: &UploadSessionId,
    ) -> StorageResult<GcsObjectBytes> {
        let key = self.layout.session_meta_key(session);
        self.get_object_bytes_with_limit(&key, MAX_SESSION_METADATA_BYTES)
            .await
    }

    async fn create_session(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
    ) -> StorageResult<()> {
        self.write_session_with_precondition(session, stored, Some(0))
            .await
    }

    async fn write_session(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
    ) -> StorageResult<()> {
        let generation = stored.meta_generation.ok_or_else(|| {
            StorageError::conflict("GCS upload metadata generation is unavailable")
        })?;
        self.write_session_with_precondition(session, stored, Some(generation))
            .await
    }

    async fn write_session_with_precondition(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        if_generation_match: Option<i64>,
    ) -> StorageResult<()> {
        let key = self.layout.session_meta_key(session);
        let bytes = serde_json::to_vec_pretty(stored)
            .map_err(|err| StorageError::backend(format!("encode gcs upload metadata: {err}")))?;
        let written = self
            .put_object_bytes(
                &key,
                Bytes::from(bytes),
                Some("application/json"),
                if_generation_match,
            )
            .await
            .map_err(map_session_write_error)?;
        stored.meta_generation = written.generation_match;
        Ok(())
    }

    async fn get_staged_bytes(
        &self,
        session: &UploadSessionId,
        read_limit: u64,
    ) -> StorageResult<GcsObjectBytes> {
        let key = self.layout.session_bytes_key(session);
        match self.get_object_bytes_with_limit(&key, read_limit).await {
            Ok(bytes) => Ok(bytes),
            Err(StorageError::UnknownUploadSession { .. }) => Ok(GcsObjectBytes::empty()),
            Err(err) => Err(err),
        }
    }

    async fn reconcile_staged_bytes(
        &self,
        session: &UploadSessionId,
        trusted_len: u64,
    ) -> StorageResult<Vec<u8>> {
        let read_limit = trusted_len
            .checked_add(1)
            .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))?;
        let mut staged = self.get_staged_bytes(session, read_limit).await?;
        let actual = staged.bytes.len() as u64;
        if actual > trusted_len {
            tracing::warn!(
                target: "pocopine.log",
                event_name = "pocopine.storage.gcs_staged_bytes_truncated",
                session = %session,
                actual,
                trusted = trusted_len,
            );
            staged.bytes.truncate(usize_from_u64(trusted_len)?);
            self.put_staged_bytes(
                session,
                Bytes::from(staged.bytes.clone()),
                staged.generation_match,
            )
            .await?;
            return Ok(staged.bytes);
        }
        if actual == trusted_len {
            return Ok(staged.bytes);
        }
        Err(StorageError::backend(format!(
            "GCS staged upload object is shorter than committed metadata: expected {trusted_len} bytes, got {actual}"
        )))
    }

    async fn put_staged_bytes(
        &self,
        session: &UploadSessionId,
        bytes: Bytes,
        if_generation_match: Option<i64>,
    ) -> StorageResult<()> {
        let key = self.layout.session_bytes_key(session);
        self.put_object_bytes(
            &key,
            bytes,
            Some("application/octet-stream"),
            if_generation_match,
        )
        .await
        .map_err(map_session_write_error)
        .map(|_| ())
    }

    async fn get_object_bytes_with_limit(
        &self,
        key: &str,
        max_bytes: u64,
    ) -> StorageResult<GcsObjectBytes> {
        let mut response = self
            .storage
            .read_object(self.layout.bucket_resource(), key)
            .send()
            .await
            .map_err(|err| {
                if is_gcs_not_found(&err) {
                    StorageError::unknown_upload_session(key.to_string())
                } else {
                    gcs_error("read object", err)
                }
            })?;
        let object = response.object();
        let etag = non_empty(object.etag.clone());
        let generation = if object.generation > 0 {
            Some(object.generation.to_string())
        } else {
            None
        };
        let generation_match = positive_generation(object.generation);
        let mut bytes = Vec::new();
        let max_len = usize_from_u64(max_bytes)?;
        while let Some(chunk) = response
            .next()
            .await
            .transpose()
            .map_err(|err| gcs_error("read object body", err))?
        {
            let exceeds_limit = match bytes.len().checked_add(chunk.len()) {
                Some(len) => len > max_len,
                None => true,
            };
            if exceeds_limit {
                let remaining = max_len.saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..remaining]);
                return Ok(GcsObjectBytes {
                    bytes,
                    etag,
                    generation,
                    generation_match,
                    truncated: true,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(GcsObjectBytes {
            bytes,
            etag,
            generation,
            generation_match,
            truncated: false,
        })
    }

    async fn get_object_metadata(&self, key: &str) -> StorageResult<GcsObjectMetadata> {
        let response = self
            .storage
            .read_object(self.layout.bucket_resource(), key)
            .send()
            .await
            .map_err(|err| {
                if is_gcs_not_found(&err) {
                    StorageError::unknown_upload_session(key.to_string())
                } else {
                    gcs_error("read object metadata", err)
                }
            })?;
        let object = response.object();
        Ok(GcsObjectMetadata {
            size: if object.size >= 0 {
                Some(object.size as u64)
            } else {
                None
            },
            etag: non_empty(object.etag.clone()),
            generation: if object.generation > 0 {
                Some(object.generation.to_string())
            } else {
                None
            },
            generation_match: positive_generation(object.generation),
        })
    }

    async fn put_object_bytes(
        &self,
        key: &str,
        bytes: Bytes,
        content_type: Option<&str>,
        if_generation_match: Option<i64>,
    ) -> StorageResult<GcsObjectWrite> {
        let mut request = self
            .storage
            .write_object(self.layout.bucket_resource(), key, bytes);
        if let Some(content_type) = content_type {
            request = request.set_content_type(content_type);
        }
        if let Some(generation) = if_generation_match {
            request = request.set_if_generation_match(generation);
        }
        let object = request.send_unbuffered().await.map_err(|err| {
            if is_gcs_precondition_failed(&err) {
                StorageError::policy_rejected("GCS object write precondition failed")
            } else {
                gcs_error("write object", err)
            }
        })?;
        Ok(GcsObjectWrite {
            etag: non_empty(object.etag),
            generation: if object.generation > 0 {
                Some(object.generation.to_string())
            } else {
                None
            },
            generation_match: positive_generation(object.generation),
        })
    }

    async fn put_completed_object(
        &self,
        key: &str,
        bytes: Bytes,
        content_type: Option<&str>,
    ) -> StorageResult<GcsObjectWrite> {
        match self.compare_existing_completed_object(key, &bytes).await {
            Ok(Some(existing)) => return Ok(existing),
            Ok(None) => {
                return Err(StorageError::policy_rejected(format!(
                    "GCS object key already exists with different bytes: {key}"
                )));
            }
            Err(StorageError::UnknownUploadSession { .. }) => {}
            Err(err) => return Err(err),
        }
        match self
            .put_object_bytes(key, bytes.clone(), content_type, Some(0))
            .await
        {
            Ok(written) => Ok(written),
            Err(StorageError::PolicyRejected { .. }) => {
                match self.compare_existing_completed_object(key, &bytes).await {
                    Ok(Some(existing)) => Ok(existing),
                    Ok(None) | Err(StorageError::UnknownUploadSession { .. }) => {
                        Err(StorageError::policy_rejected(format!(
                            "GCS object key already exists: {key}"
                        )))
                    }
                    Err(err) => Err(err),
                }
            }
            Err(err) => Err(err),
        }
    }

    async fn compare_existing_completed_object(
        &self,
        key: &str,
        bytes: &Bytes,
    ) -> StorageResult<Option<GcsObjectWrite>> {
        let metadata = self.get_object_metadata(key).await?;
        if let Some(size) = metadata.size {
            if size != bytes.len() as u64 && (size != 0 || bytes.is_empty()) {
                return Ok(None);
            }
        }
        let existing = self
            .get_object_bytes_with_limit(key, bytes.len() as u64)
            .await?;
        if existing.truncated || existing.bytes.as_slice() != bytes.as_ref() {
            return Ok(None);
        }
        Ok(Some(GcsObjectWrite {
            etag: existing.etag.or(metadata.etag),
            generation: existing.generation.or(metadata.generation),
            generation_match: existing.generation_match.or(metadata.generation_match),
        }))
    }

    async fn delete_object(&self, key: &str) -> StorageResult<()> {
        self.control.delete_object(&self.layout, key).await
    }

    async fn delete_object_if_exists(&self, key: &str) -> StorageResult<()> {
        match self.delete_object(key).await {
            Ok(()) | Err(StorageError::UnknownUploadSession { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }

    async fn persist_expired_if_needed(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
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
        self.delete_object_if_exists(&self.layout.session_bytes_key(session))
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

    fn ensure_policy_is_supported(&self, max_bytes: u64) -> StorageResult<()> {
        if max_bytes > self.max_proxy_upload_bytes {
            return Err(StorageError::payload_too_large(self.max_proxy_upload_bytes));
        }
        Ok(())
    }

    fn object_ref(
        &self,
        stored: &StoredUploadSession,
        size: u64,
        written: GcsObjectWrite,
        checksum: Option<ObjectChecksum>,
    ) -> ObjectRef {
        let mut metadata = stored.storage_key.metadata.0.clone();
        metadata.extend(stored.request_metadata.clone());
        ObjectRef {
            backend: self.name.to_string(),
            scope: stored.public.scope.clone(),
            key: stored.storage_key.key.to_string(),
            version: written.generation,
            etag: written.etag,
            checksum,
            content_type: stored.public.content_type.clone(),
            size,
            visibility: stored.visibility,
            metadata,
        }
    }
}

impl StorageBackend for GcsStorageBackend {
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
            self.ensure_policy_is_supported(request.policy.max_bytes)?;
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
            let mut stored = StoredUploadSession {
                public: session.clone(),
                owner: ctx.actor.clone(),
                storage_key: request.storage_key,
                visibility: request.policy.visibility,
                max_bytes: request.policy.max_bytes.min(self.max_proxy_upload_bytes),
                checksum_policy: request.policy.checksum,
                request_metadata: request.metadata,
                object: None,
                completion_object_key: None,
                cleanup_pending: false,
                meta_generation: None,
            };
            self.create_session(&id, &mut stored).await?;
            if let Err(err) = self.put_staged_bytes(&id, Bytes::new(), Some(0)).await {
                let _ = self
                    .delete_object_if_exists(&self.layout.session_meta_key(&id))
                    .await;
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
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
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
                    self.write_session(&session, &mut stored).await?;
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
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
            let _guard = lock.lock().await;
            self.ensure_requested_size_is_supported(Some(size))?;
            let mut stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            self.persist_expired_if_needed(&session, &mut stored)
                .await?;
            let committed_offset = stored.public.next_offset.unwrap_or(0);
            let _staged = self
                .reconcile_staged_bytes(&session, committed_offset)
                .await?;
            ensure_upload_length_can_be_set(
                stored.max_bytes,
                &stored.public,
                committed_offset,
                size,
            )?;
            stored.public.size = Some(size);
            self.write_session(&session, &mut stored).await?;
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
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
            let _guard = lock.lock().await;
            let mut stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            self.persist_expired_if_needed(&session, &mut stored)
                .await?;
            ensure_open(&stored.public)?;
            if stored.public.strategy != UploadStrategy::Sequential {
                return Err(StorageError::unsupported(
                    "GCS backend currently supports sequential proxy uploads only",
                ));
            }
            let expected = stored.public.next_offset.unwrap_or(0);
            let new_offset = checked_new_offset(offset, bytes.len())?;
            let read_limit = expected
                .max(new_offset)
                .checked_add(1)
                .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))?;
            let mut staged_object = self.get_staged_bytes(&session, read_limit).await?;
            let staged_len = staged_object.bytes.len() as u64;
            if expected != offset {
                if new_offset == expected
                    && bytes_match_at(&staged_object.bytes, offset, bytes.as_ref())?
                {
                    return Ok(stored.public);
                }
                tracing::debug!(
                    target: "pocopine.log",
                    event_name = "pocopine.storage.offset_mismatch",
                    session = %session,
                    expected,
                    provided = offset,
                );
                return Err(StorageError::offset_mismatch(expected, offset));
            }
            if staged_len > expected {
                if staged_len == new_offset
                    && bytes_match_at(&staged_object.bytes, expected, bytes.as_ref())?
                {
                    stored.public.next_offset = Some(new_offset);
                    self.write_session(&session, &mut stored).await?;
                    return Ok(stored.public);
                }
                tracing::warn!(
                    target: "pocopine.log",
                    event_name = "pocopine.storage.gcs_staged_bytes_truncated",
                    session = %session,
                    actual = staged_len,
                    trusted = expected,
                );
                staged_object.bytes.truncate(usize_from_u64(expected)?);
                self.put_staged_bytes(
                    &session,
                    Bytes::from(staged_object.bytes.clone()),
                    staged_object.generation_match,
                )
                .await?;
            } else if staged_len < expected {
                return Err(StorageError::backend(format!(
                    "GCS staged upload object is shorter than committed metadata: expected {expected} bytes, got {staged_len}"
                )));
            }
            ensure_size_limit(stored.max_bytes, stored.public.size, new_offset)?;
            staged_object.bytes.extend_from_slice(&bytes);
            self.put_staged_bytes(
                &session,
                Bytes::from(staged_object.bytes),
                staged_object.generation_match,
            )
            .await?;
            stored.public.next_offset = Some(new_offset);
            self.write_session(&session, &mut stored).await?;
            Ok(stored.public)
        })
    }

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, ObjectRef> {
        Box::pin(async move {
            let lock = self.session_locks.lock(&request.session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &request.session);
            let _guard = lock.lock().await;
            let mut stored = self.read_session(&request.session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            if let Some(object) = &stored.object {
                let object = object.clone();
                self.cleanup_staged_bytes(&request.session, &mut stored)
                    .await?;
                return Ok(object);
            }
            self.persist_expired_if_needed(&request.session, &mut stored)
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
                || stored.completion_object_key.as_deref() != Some(object_key.as_str())
            {
                stored.public.status = UploadSessionStatus::Completing;
                stored.public.next_offset = Some(actual);
                stored.completion_object_key = Some(object_key.clone());
                self.write_session(&request.session, &mut stored).await?;
            }
            let staged = Bytes::from(staged);
            let written = match self
                .put_completed_object(&object_key, staged, stored.public.content_type.as_deref())
                .await
            {
                Ok(written) => written,
                Err(err @ StorageError::PolicyRejected { .. }) => {
                    stored.public.status = UploadSessionStatus::Open;
                    stored.completion_object_key = None;
                    let _ = self.write_session(&request.session, &mut stored).await;
                    return Err(err);
                }
                Err(err) => return Err(err),
            };
            let object = self.object_ref(&stored, actual, written, checksum);
            stored.public.status = UploadSessionStatus::Complete;
            stored.public.next_offset = Some(actual);
            stored.object = Some(object.clone());
            stored.cleanup_pending = true;
            self.write_session(&request.session, &mut stored).await?;
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
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
            let _guard = lock.lock().await;
            let read_session = self.read_session_for_abort(&session).await?;
            match &read_session {
                AbortSessionRead::Known(stored) => {
                    ensure_owner(&ctx.actor, &stored.owner)?;
                    if stored.public.status == UploadSessionStatus::Completing {
                        return Err(StorageError::UploadClosed {
                            session: session.to_string(),
                        });
                    }
                }
                AbortSessionRead::Missing => {}
                AbortSessionRead::Corrupt => {
                    if !matches!(&ctx.actor, StorageActor::System(_)) {
                        return Err(StorageError::forbidden(
                            "corrupt GCS upload metadata can only be aborted by a system actor",
                        ));
                    }
                }
            }
            let meta_key = self.layout.session_meta_key(&session);
            let bytes_key = self.layout.session_bytes_key(&session);
            let bytes_deleted = self.delete_object_if_exists(&bytes_key).await;
            let meta_deleted = match read_session {
                AbortSessionRead::Known(_) | AbortSessionRead::Corrupt => {
                    self.delete_object_if_exists(&meta_key).await
                }
                AbortSessionRead::Missing => Ok(()),
            };
            bytes_deleted?;
            meta_deleted?;
            Ok(())
        })
    }
}
