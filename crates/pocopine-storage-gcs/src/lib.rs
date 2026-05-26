//! Google Cloud Storage backend adapter for `pocopine-storage`.
//!
//! This crate implements Pocopine's current bounded sequential proxy upload
//! contract using the official `google-cloud-storage` clients. Completed
//! objects are written by `SafeObjectKey`, while upload-session metadata and
//! staging bytes live under an internal prefix in the same bucket.
//!
//! The adapter rewrites the staged object on each append and keeps only
//! in-process per-session locks, so route a given upload session to one server
//! replica or wait for a future provider-side multipart backend before using it
//! for large or horizontally written uploads.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex as StdMutex};

use bytes::Bytes;
use google_cloud_gax::options::RequestOptionsBuilder;
use google_cloud_storage::client::{Storage, StorageControl};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
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

const DEFAULT_BACKEND_NAME: &str = "gcs";
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
    session_locks: Arc<StdMutex<HashMap<String, Arc<TokioMutex<()>>>>>,
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
            session_locks: Arc::new(StdMutex::new(HashMap::new())),
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
            .map_err(|err| StorageError::backend(format!("read gcs upload metadata: {err}")))?;
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
            .map_err(|err| StorageError::backend(format!("encode gcs upload metadata: {err}")))?;
        self.put_object_bytes(&key, Bytes::from(bytes), Some("application/json"), None)
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
                event_name = "pocopine.storage.gcs_staged_bytes_ahead",
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
            "GCS staged upload object is shorter than committed metadata: expected {trusted_len} bytes, got {actual}"
        )))
    }

    async fn put_staged_bytes(&self, session: &UploadSessionId, bytes: Bytes) -> StorageResult<()> {
        let key = self.layout.session_bytes_key(session);
        self.put_object_bytes(&key, bytes, Some("application/octet-stream"), None)
            .await
            .map(|_| ())
    }

    async fn get_object_bytes(&self, key: &str) -> StorageResult<Vec<u8>> {
        self.get_object_bytes_with_metadata(key)
            .await
            .map(|metadata| metadata.bytes)
    }

    async fn get_object_bytes_with_metadata(&self, key: &str) -> StorageResult<GcsObjectBytes> {
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
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .next()
            .await
            .transpose()
            .map_err(|err| gcs_error("read object body", err))?
        {
            bytes.extend_from_slice(&chunk);
        }
        Ok(GcsObjectBytes {
            bytes,
            etag,
            generation,
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
        })
    }

    async fn put_completed_object(
        &self,
        key: &str,
        bytes: Bytes,
        content_type: Option<&str>,
    ) -> StorageResult<GcsObjectWrite> {
        match self.get_object_bytes_with_metadata(key).await {
            Ok(existing) => {
                if existing.bytes.as_slice() == bytes.as_ref() {
                    return Ok(GcsObjectWrite {
                        etag: existing.etag,
                        generation: existing.generation,
                    });
                }
                return Err(StorageError::policy_rejected(format!(
                    "GCS object key already exists with different bytes: {key}"
                )));
            }
            Err(StorageError::UnknownUploadSession { .. }) => {}
            Err(err) => return Err(err),
        }
        match self
            .put_object_bytes(key, bytes, content_type, Some(0))
            .await
        {
            Ok(written) => Ok(written),
            Err(StorageError::PolicyRejected { .. }) => Err(StorageError::policy_rejected(
                format!("GCS object key already exists: {key}"),
            )),
            Err(err) => Err(err),
        }
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

struct GcsObjectBytes {
    bytes: Vec<u8>,
    etag: Option<String>,
    generation: Option<String>,
}

struct GcsObjectWrite {
    etag: Option<String>,
    generation: Option<String>,
}

#[derive(Clone)]
enum GcsControl {
    Google(StorageControl),
    Json(GcsJsonControl),
}

impl GcsControl {
    async fn delete_object(&self, layout: &GcsKeyLayout, key: &str) -> StorageResult<()> {
        match self {
            Self::Google(control) => delete_object_with_google_control(control, layout, key).await,
            Self::Json(control) => control.delete_object(layout, key).await,
        }
    }
}

#[derive(Clone)]
struct GcsJsonControl {
    endpoint: String,
    http: reqwest::Client,
}

impl GcsJsonControl {
    fn new(endpoint: String) -> StorageResult<Self> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        if endpoint.is_empty() {
            return Err(StorageError::policy_rejected(
                "GCS emulator endpoint must not be empty",
            ));
        }
        Ok(Self {
            endpoint,
            http: reqwest::Client::new(),
        })
    }

    async fn delete_object(&self, layout: &GcsKeyLayout, key: &str) -> StorageResult<()> {
        let url = format!(
            "{}/storage/v1/b/{}/o/{}",
            self.endpoint,
            encode_uri_component(layout.bucket()),
            encode_uri_component(key)
        );
        let response = self
            .http
            .delete(url)
            .send()
            .await
            .map_err(|err| gcs_error("delete object", err))?;
        if response.status().as_u16() == 404 {
            return Err(StorageError::unknown_upload_session(key.to_string()));
        }
        if !response.status().is_success() {
            return Err(StorageError::backend(format!(
                "GCS delete object: HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }
}

async fn delete_object_with_google_control(
    control: &StorageControl,
    layout: &GcsKeyLayout,
    key: &str,
) -> StorageResult<()> {
    control
        .delete_object()
        .set_bucket(layout.bucket_resource())
        .set_object(key)
        .with_idempotency(true)
        .send()
        .await
        .map_err(|err| {
            if is_gcs_not_found(&err) {
                StorageError::unknown_upload_session(key.to_string())
            } else {
                gcs_error("delete object", err)
            }
        })?;
    Ok(())
}

struct SessionLockCleanup<'a> {
    backend: &'a GcsStorageBackend,
    session: UploadSessionId,
}

impl<'a> SessionLockCleanup<'a> {
    fn new(backend: &'a GcsStorageBackend, session: &UploadSessionId) -> Self {
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
            if let Err(err) = self.put_staged_bytes(&id, Bytes::new()).await {
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
                    "GCS backend currently supports sequential proxy uploads only",
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
            self.put_staged_bytes(&session, Bytes::from(staged)).await?;
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
            let written = self
                .put_completed_object(
                    &object_key,
                    Bytes::from(staged),
                    stored.public.content_type.as_deref(),
                )
                .await?;
            let object = self.object_ref(&stored, actual, written, checksum);
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
            self.delete_object_if_exists(&bytes_key).await?;
            if known_session {
                self.delete_object_if_exists(&meta_key).await?;
            }
            Ok(())
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GcsKeyLayout {
    bucket: String,
    bucket_resource: String,
    object_prefix: Option<String>,
    internal_prefix_base: String,
    internal_prefix: String,
}

impl GcsKeyLayout {
    fn new(
        bucket: String,
        object_prefix: Option<String>,
        internal_prefix: String,
    ) -> StorageResult<Self> {
        let bucket = bucket.trim().to_string();
        if bucket.is_empty() {
            return Err(StorageError::policy_rejected(
                "GCS bucket name must not be empty",
            ));
        }
        let bucket = bucket
            .strip_prefix("projects/_/buckets/")
            .unwrap_or(bucket.as_str())
            .to_string();
        if bucket.contains('/') {
            return Err(StorageError::policy_rejected(
                "GCS bucket name must be a bucket id or projects/_/buckets/{bucket}",
            ));
        }
        let bucket_resource = format!("projects/_/buckets/{bucket}");
        let object_prefix = object_prefix.and_then(normalize_prefix);
        let internal_prefix_base = normalize_prefix(internal_prefix).ok_or_else(|| {
            StorageError::policy_rejected("GCS internal prefix must not be empty")
        })?;
        let internal_prefix =
            join_optional_prefix(object_prefix.as_deref(), internal_prefix_base.as_str());
        Ok(Self {
            bucket,
            bucket_resource,
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

    fn bucket_resource(&self) -> &str {
        &self.bucket_resource
    }

    fn bucket(&self) -> &str {
        &self.bucket
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

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn encode_uri_component(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn is_gcs_precondition_failed(err: &google_cloud_storage::Error) -> bool {
    err.http_status_code().is_some_and(|status| status == 412)
}

fn is_gcs_not_found(err: &google_cloud_storage::Error) -> bool {
    err.http_status_code().is_some_and(|status| status == 404)
}

fn gcs_error(operation: &'static str, err: impl std::fmt::Display) -> StorageError {
    tracing::error!(
        target: "pocopine.log",
        event_name = "pocopine.storage.gcs_error",
        operation,
        error = %err,
    );
    StorageError::backend(format!("GCS {operation}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_layout_keeps_internal_objects_out_of_app_keyspace() {
        let layout = GcsKeyLayout::new(
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
        assert_eq!(layout.bucket_resource(), "projects/_/buckets/bucket");
    }

    #[test]
    fn prefix_and_internal_prefix_are_order_independent() {
        let first = GcsKeyLayout::new(
            "bucket".to_string(),
            None,
            DEFAULT_INTERNAL_PREFIX.to_string(),
        )
        .unwrap()
        .with_internal_prefix("custom/sessions".to_string())
        .unwrap()
        .with_prefix("tenant-a".to_string())
        .unwrap();
        let second = GcsKeyLayout::new(
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
    fn accepts_bucket_resource_name() {
        let layout = GcsKeyLayout::new(
            "projects/_/buckets/my-bucket".to_string(),
            None,
            DEFAULT_INTERNAL_PREFIX.to_string(),
        )
        .unwrap();
        assert_eq!(layout.bucket, "my-bucket");
        assert_eq!(layout.bucket_resource, "projects/_/buckets/my-bucket");
    }

    #[test]
    fn empty_bucket_is_rejected() {
        assert!(
            GcsKeyLayout::new("  ".to_string(), None, DEFAULT_INTERNAL_PREFIX.to_string()).is_err()
        );
    }
}
