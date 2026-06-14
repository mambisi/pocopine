use bytes::Bytes;
use google_cloud_storage::client::{Storage, StorageControl};
use pocopine_storage::backend_common::{
    UploadConcurrencyRegistry, UploadSessionLockCleanup, UploadSessionLockRegistry,
    checked_new_offset, ensure_open, ensure_owner, ensure_size_limit,
    ensure_upload_length_can_be_set, expires_at, select_upload_mode,
};
use pocopine_storage::checksum::{
    StreamingChecksum, checksum_algorithm_to_compute, ensure_supported_checksum_policy,
    precheck_checksum, validate_complete_checksum, validate_complete_checksum_precomputed,
};
use pocopine_storage::{
    BackendCapabilities, ChecksumAlgorithm, ChecksumPolicy, CompleteUpload, InitiateUpload,
    ObjectChecksum, ObjectRef, StorageActor, StorageBackend, StorageBoxFuture, StorageContext,
    StorageError, StorageResult, TransferPlan, UploadBody, UploadSession, UploadSessionId,
    UploadSessionStatus, UploadStrategy,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::control::{ComposeRequest, ComposeSource, GcsControl, GcsJsonControl};
use crate::layout::{DEFAULT_INTERNAL_PREFIX, GcsKeyLayout};
use crate::state::{
    AbortSessionRead, GcsComponent, GcsObjectAttrs, GcsObjectBytes, GcsObjectMetadata,
    GcsObjectWrite, NativeUploadState, StoredUploadSession, decode_session_object,
};
use crate::util::{
    ensure_completable, gcs_error, is_gcs_not_found, is_gcs_precondition_failed,
    map_session_write_error, non_empty, positive_generation, usize_from_u64,
};

const DEFAULT_BACKEND_NAME: &str = "gcs";
const DEFAULT_MAX_PROXY_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SESSION_METADATA_BYTES: u64 = 256 * 1024;

/// GCS allows up to 32 source objects per `ComposeObject`. Components are sized
/// so their count never exceeds this, keeping completion to a single compose.
const GCS_MAX_COMPOSE_SOURCES: u64 = 32;

/// Floor for a coalesced component, to avoid many tiny component objects.
const MIN_COMPONENT_SIZE: u64 = 1024 * 1024;

/// Object custom-metadata key stamping the owning upload session onto the final
/// composed object, so a retry can recognize its own object after a crash.
const SESSION_OWNER_META_KEY: &str = "pocopine-upload-session";

/// Component size for a session whose maximum object size is `max_bytes`.
///
/// Scaled (rounded to a whole MiB) so component count stays within
/// [`GCS_MAX_COMPOSE_SOURCES`]; never below [`MIN_COMPONENT_SIZE`]. For the
/// default 64 MiB cap this is 2 MiB.
fn component_size_for(max_bytes: u64) -> u64 {
    let by_count = max_bytes
        .div_ceil(GCS_MAX_COMPOSE_SOURCES)
        .div_ceil(MIN_COMPONENT_SIZE)
        * MIN_COMPONENT_SIZE;
    by_count.max(MIN_COMPONENT_SIZE)
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
    session_locks: UploadSessionLockRegistry,
    part_concurrency: UploadConcurrencyRegistry,
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
            part_concurrency: UploadConcurrencyRegistry::new(),
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
            part_concurrency: UploadConcurrencyRegistry::new(),
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
    /// The default is 64 MiB: appends flush full-size component objects (each
    /// written once) and completion assembles them with a single `ComposeObject`,
    /// so the cap bounds component count and the inline tail buffered per session.
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
        self.get_object_bytes_with_limit(&key, self.session_metadata_read_limit())
            .await
    }

    /// Read cap for `session.json`. Native compose sessions buffer their tail
    /// inline (base64), so the cap must accommodate one component plus the
    /// fixed metadata, well above the bare [`MAX_SESSION_METADATA_BYTES`] used
    /// for the non-tail fields. Derived from the backend's absolute size cap so
    /// it is an upper bound for every session's `component_size_for(max_bytes)`.
    fn session_metadata_read_limit(&self) -> u64 {
        component_size_for(self.max_proxy_upload_bytes)
            .saturating_mul(2)
            .saturating_add(MAX_SESSION_METADATA_BYTES)
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

    /// Fetch full object attributes (including the ownership marker in custom
    /// metadata) via the control layer. Returns `None` if the object is absent.
    async fn get_object_attrs(&self, key: &str) -> StorageResult<Option<GcsObjectAttrs>> {
        self.control
            .get_object_attrs(&self.layout, key, SESSION_OWNER_META_KEY)
            .await
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
        if let Some(size) = metadata.size
            && size != bytes.len() as u64
            && (size != 0 || bytes.is_empty())
        {
            return Ok(None);
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

    // --- native compose helpers ----------------------------------------------

    /// Write one component object.
    ///
    /// Uses a create-only precondition (`if_generation_match: 0`) so a concurrent
    /// or retried attempt at the same index can never clobber a component another
    /// worker already recorded. A component's content is deterministic for its
    /// index (it covers a fixed, append-only byte range), so an already-present
    /// object is correct — adopt its generation instead of overwriting it.
    async fn write_component(
        &self,
        session: &UploadSessionId,
        index: u32,
        bytes: Bytes,
    ) -> StorageResult<GcsComponent> {
        let key = self.layout.session_component_key(session, index);
        let size = bytes.len() as u64;
        let generation = match self
            .put_object_bytes(
                &key,
                bytes.clone(),
                Some("application/octet-stream"),
                Some(0),
            )
            .await
        {
            Ok(written) => written.generation_match,
            Err(StorageError::PolicyRejected { .. }) => {
                // A component already exists at this index (a retry or a
                // concurrent worker). Adopt it only if its bytes match what this
                // append intends to write; otherwise the index is being reused
                // for different content and must not be silently accepted.
                let existing = match self.get_object_bytes_with_limit(&key, size).await {
                    Ok(existing) => existing,
                    // The conflicting component was removed between the create-only
                    // PUT and this read (a concurrent race on an internal key). This
                    // is a write conflict, not a missing user session — don't leak a
                    // 404 for the session.
                    Err(StorageError::UnknownUploadSession { .. }) => {
                        return Err(StorageError::conflict(
                            "GCS upload component changed during a concurrent write",
                        ));
                    }
                    Err(err) => return Err(err),
                };
                if existing.truncated
                    || existing.bytes.len() as u64 != size
                    || existing.bytes.as_slice() != bytes.as_ref()
                {
                    return Err(StorageError::conflict(
                        "GCS upload component already exists with different bytes",
                    ));
                }
                existing.generation_match
            }
            Err(err) => return Err(err),
        };
        Ok(GcsComponent {
            key,
            size,
            generation,
        })
    }

    /// Best-effort deletion of every component object for a session.
    ///
    /// Component indices are contiguous from 0 (sequential, append-only), so this
    /// also removes components written just before a crash but not yet recorded
    /// in the session metadata. Stops at the first missing index.
    async fn delete_component_chain(&self, session: &UploadSessionId) {
        let mut index = 0u32;
        loop {
            let key = self.layout.session_component_key(session, index);
            match self.delete_object(&key).await {
                Ok(()) => index += 1,
                Err(_) => break,
            }
        }
    }

    /// Best-effort deletion of a fixed component-index range `0..count`,
    /// tolerating gaps. By-number multipart parts can arrive out of order, so a
    /// missing index does not mean later ones are absent — unlike the contiguous
    /// [`Self::delete_component_chain`].
    async fn delete_component_range(&self, session: &UploadSessionId, count: u32) {
        for index in 0..count {
            let key = self.layout.session_component_key(session, index);
            let _ = self.delete_object_if_exists(&key).await;
        }
    }

    /// Return a session that reached `Completing` back to `Open` after a
    /// recoverable completion failure, so the caller can retry or abort from a
    /// valid state instead of being stranded. Best-effort.
    async fn reopen_session(&self, session: &UploadSessionId, stored: &mut StoredUploadSession) {
        stored.public.status = UploadSessionStatus::Open;
        stored.completion_object_key = None;
        let _ = self.write_session(session, stored).await;
    }

    /// Reopen a failed completion ONLY when no live object owned by this session
    /// exists at the destination.
    ///
    /// If compose already produced our object (e.g. a later checksum-stream error,
    /// or a lost compose response), reopening would let a subsequent append change
    /// the committed bytes while a retry still adopts the already-live object —
    /// serving stale data. In that case the session stays `Completing` and a retry
    /// adopts-and-finishes idempotently. Reopening is safe only when the object is
    /// absent (compose never landed) or foreign (never ours to serve).
    async fn reopen_if_object_absent(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        object_key: &str,
    ) {
        // Reopen only when the HEAD *definitively* shows the object is absent or
        // foreign. An indeterminate lookup (transient/backend error) must keep the
        // session `Completing`: a live owned object might exist, and reopening
        // would let a later append change the bytes a retry then adopts.
        let absent_or_foreign = match self.get_object_attrs(object_key).await {
            Ok(None) => true,
            Ok(Some(attrs)) => attrs.owner_session.as_deref() != Some(session.as_str()),
            Err(_) => false,
        };
        if absent_or_foreign {
            self.reopen_session(session, stored).await;
        }
    }

    /// Assemble the recorded components into the destination object, stamping the
    /// owning session so a crashed retry can recognize its own object.
    async fn compose_components(
        &self,
        session: &UploadSessionId,
        stored: &StoredUploadSession,
        object_key: &str,
        if_generation_match: Option<i64>,
    ) -> StorageResult<GcsObjectWrite> {
        let state = stored.native.compose().expect("compose state");
        let sources = state
            .components
            .iter()
            .map(|component| ComposeSource {
                key: component.key.clone(),
                generation: component.generation,
            })
            .collect();
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert(
            SESSION_OWNER_META_KEY.to_string(),
            session.as_str().to_string(),
        );
        self.control
            .compose_object(
                &self.layout,
                ComposeRequest {
                    dest_key: object_key,
                    content_type: stored.public.content_type.as_deref(),
                    metadata,
                    sources,
                    if_generation_match,
                },
            )
            .await
    }

    /// Stream the stored object through `algorithm` without buffering it.
    async fn stream_object_checksum(
        &self,
        key: &str,
        algorithm: ChecksumAlgorithm,
    ) -> StorageResult<ObjectChecksum> {
        let mut response = self
            .storage
            .read_object(self.layout.bucket_resource(), key)
            .send()
            .await
            .map_err(|err| gcs_error("read object for checksum", err))?;
        let mut hasher = StreamingChecksum::new(algorithm);
        while let Some(chunk) = response
            .next()
            .await
            .transpose()
            .map_err(|err| gcs_error("read object body for checksum", err))?
        {
            hasher.update(&chunk);
        }
        Ok(hasher.finalize())
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

    fn ensure_requested_size_is_supported(&self, size: Option<u64>) -> StorageResult<()> {
        if let Some(size) = size
            && size > self.max_proxy_upload_bytes
        {
            return Err(StorageError::payload_too_large(self.max_proxy_upload_bytes));
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

    // --- native compose upload flow -------------------------------------------

    async fn append_compose(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        offset: u64,
        bytes: Bytes,
    ) -> StorageResult<UploadSession> {
        // Copy the buffered tail; it stays durable in `stored` until the single
        // atomic session write at the end (so a crash mid-append leaves the
        // durable metadata unchanged and the client re-sends from the old offset).
        let (flushed, tail) = {
            let state = stored.native.compose().expect("compose state");
            (state.flushed_len(), state.pending_tail.clone())
        };
        let expected = flushed + tail.len() as u64;
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
        if bytes.is_empty() {
            return Ok(stored.public);
        }

        let combined: Bytes = if tail.is_empty() {
            bytes
        } else {
            let mut combined = tail;
            combined.extend_from_slice(&bytes);
            Bytes::from(combined)
        };

        // Flush full-size components (each written once); the remainder is the
        // new tail. Component size keeps the count within the single-compose cap.
        let component_size = component_size_for(stored.max_bytes) as usize;
        let mut pos = 0usize;
        while combined.len() - pos >= component_size {
            let index = stored.native.compose().expect("compose state").next_index;
            let part = combined.slice(pos..pos + component_size);
            let component = self.write_component(session, index, part).await?;
            if let Some(state) = stored.native.compose_mut() {
                state.components.push(component);
                state.next_index = index + 1;
            }
            pos += component_size;
        }
        if let Some(state) = stored.native.compose_mut() {
            state.pending_tail = combined.slice(pos..).to_vec();
        }
        stored.public.next_offset = Some(new_offset);
        self.write_session(session, &mut stored).await?;
        Ok(stored.public)
    }

    async fn complete_compose(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        let total = stored.public.next_offset.unwrap_or(0);
        if let Some(expected) = stored.public.size
            && total != expected
        {
            return Err(StorageError::policy_rejected(format!(
                "upload is incomplete: expected {expected} bytes, got {total}"
            )));
        }
        ensure_size_limit(stored.max_bytes, stored.public.size, total)?;
        // Reject a missing-required / disallowed-algorithm checksum before any
        // finalize work, while the session is still resumable.
        precheck_checksum(&stored.checksum_policy, provided_checksum.as_ref())?;

        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        let has_components = !stored
            .native
            .compose()
            .expect("compose state")
            .components
            .is_empty();

        let (written, checksum) = if !has_components {
            // Whole object fits in the tail (or is empty): a single direct PUT,
            // atomic and collision-guarded by `put_completed_object`. The checksum
            // is validated before the write, so a bad checksum leaves the session
            // open and resumable.
            let tail = stored
                .native
                .compose()
                .expect("compose state")
                .pending_tail
                .clone();
            let checksum =
                validate_complete_checksum(&stored.checksum_policy, &tail, provided_checksum)?;
            let written = self
                .put_completed_object(
                    &object_key,
                    Bytes::from(tail),
                    stored.public.content_type.as_deref(),
                )
                .await?;
            (written, checksum)
        } else {
            // Finalize via components/compose. Any failure after the session is
            // marked `Completing` reopens it so the owner can retry or abort
            // rather than be stranded (the reopen is a no-op while still `Open`).
            match self
                .complete_with_components(session, &mut stored, &object_key, provided_checksum)
                .await
            {
                Ok(pair) => pair,
                Err(err) => {
                    self.reopen_if_object_absent(session, &mut stored, &object_key)
                        .await;
                    return Err(err);
                }
            }
        };

        let object = self.object_ref(&stored, total, written, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(total);
        stored.object = Some(object.clone());
        if let Some(state) = stored.native.compose_mut() {
            state.pending_tail = Vec::new();
        }
        self.write_session(session, &mut stored).await?;
        // Component objects are temporary; remove them now the final object is
        // live (the compose copied their bytes into it).
        self.delete_component_chain(session).await;
        Ok(object)
    }

    /// Finalize a session that has flushed components. The caller wraps this so
    /// any error reopens the (possibly `Completing`) session.
    async fn complete_with_components(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        object_key: &str,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<(GcsObjectWrite, Option<ObjectChecksum>)> {
        let total = stored.public.next_offset.unwrap_or(0);
        // HEAD on every attempt: adopt our own already-composed object (matched by
        // the ownership marker), reject a foreign one. Works on stores that ignore
        // compose preconditions; `if_generation_match: 0` closes the TOCTOU window
        // on real GCS.
        let written = if let Some(attrs) = self.get_object_attrs(object_key).await? {
            if attrs.owner_session.as_deref() == Some(session.as_str()) {
                GcsObjectWrite {
                    etag: attrs.etag,
                    generation: attrs.generation.map(|generation| generation.to_string()),
                    generation_match: attrs.generation,
                }
            } else {
                self.delete_component_chain(session).await;
                return Err(StorageError::policy_rejected(format!(
                    "GCS object key already exists: {object_key}"
                )));
            }
        } else {
            if stored.public.status == UploadSessionStatus::Open
                || stored.public.next_offset != Some(total)
                || stored.completion_object_key.as_deref() != Some(object_key)
            {
                stored.public.status = UploadSessionStatus::Completing;
                stored.public.next_offset = Some(total);
                stored.completion_object_key = Some(object_key.to_string());
                self.write_session(session, stored).await?;
            }
            // Flush the final remainder as the last component, then compose.
            let tail = stored
                .native
                .compose()
                .expect("compose state")
                .pending_tail
                .clone();
            if !tail.is_empty() {
                let index = stored.native.compose().expect("compose state").next_index;
                let component = self
                    .write_component(session, index, Bytes::from(tail))
                    .await?;
                if let Some(state) = stored.native.compose_mut() {
                    state.components.push(component);
                    state.next_index = index + 1;
                    state.pending_tail = Vec::new();
                }
                self.write_session(session, stored).await?;
            }
            match self
                .compose_components(session, stored, object_key, Some(0))
                .await
            {
                Ok(written) => written,
                Err(StorageError::PolicyRejected { .. }) => {
                    // An object appeared at the key during completion; adopt it
                    // only if it is ours, otherwise clean up and reject.
                    match self.get_object_attrs(object_key).await? {
                        Some(attrs) if attrs.owner_session.as_deref() == Some(session.as_str()) => {
                            GcsObjectWrite {
                                etag: attrs.etag,
                                generation: attrs
                                    .generation
                                    .map(|generation| generation.to_string()),
                                generation_match: attrs.generation,
                            }
                        }
                        _ => {
                            self.delete_component_chain(session).await;
                            return Err(StorageError::policy_rejected(format!(
                                "GCS object key already exists: {object_key}"
                            )));
                        }
                    }
                }
                Err(err) => return Err(err),
            }
        };

        let checksum = self
            .verify_object_checksum(object_key, &stored.checksum_policy, provided_checksum)
            .await?;
        Ok((written, checksum))
    }

    /// Verify the composed object against the checksum policy by streaming it
    /// back (only when a checksum is required/supplied — the default `None`
    /// policy never reads the object). On mismatch the live object is deleted so
    /// a bad upload is never observable; a delete failure is surfaced, since a
    /// checksum-invalid live object is worse than the error.
    async fn verify_object_checksum(
        &self,
        object_key: &str,
        policy: &ChecksumPolicy,
        provided: Option<ObjectChecksum>,
    ) -> StorageResult<Option<ObjectChecksum>> {
        match checksum_algorithm_to_compute(policy, provided.as_ref()) {
            Some(algorithm) => {
                let computed = self.stream_object_checksum(object_key, algorithm).await?;
                match validate_complete_checksum_precomputed(policy, Some(computed), provided) {
                    Ok(checksum) => Ok(checksum),
                    Err(err) => {
                        self.delete_object(object_key).await.map_err(|delete_err| {
                            tracing::error!(
                                target: "pocopine.log",
                                event_name = "pocopine.storage.gcs_invalid_object_cleanup_failed",
                                object_key = %object_key,
                                checksum_error = %err,
                                delete_error = %delete_err,
                            );
                            delete_err
                        })?;
                        Err(err)
                    }
                }
            }
            None => validate_complete_checksum_precomputed(policy, None, provided),
        }
    }

    // --- native compose by-number (server-mediated multipart) -----------------

    /// Probe the `count` component objects of a by-number multipart upload
    /// (indices `0..count`) for existence, returning each as a compose source
    /// (key + generation) in order. A missing component means the corresponding
    /// part was never uploaded, so the upload is incomplete.
    ///
    /// Sizes are deliberately not summed here: the declared upload length is the
    /// authoritative total (some emulators report a zero object size on metadata
    /// reads), the same way the sequential path trusts its committed offset.
    async fn probe_by_number_components(
        &self,
        session: &UploadSessionId,
        count: u32,
    ) -> StorageResult<Vec<ComposeSource>> {
        let mut sources = Vec::with_capacity(count as usize);
        for index in 0..count {
            let key = self.layout.session_component_key(session, index);
            match self.get_object_metadata(&key).await {
                Ok(metadata) => sources.push(ComposeSource {
                    key,
                    generation: metadata.generation_match,
                }),
                Err(StorageError::UnknownUploadSession { .. }) => {
                    return Err(StorageError::policy_rejected(format!(
                        "upload is incomplete: missing multipart part {}",
                        index + 1
                    )));
                }
                Err(err) => return Err(err),
            }
        }
        Ok(sources)
    }

    /// Write a zero-byte completed object for an empty multipart session, guarding
    /// ownership by the session marker rather than `put_completed_object`'s
    /// byte-match fast path (which would wrongly adopt a *foreign* empty object).
    /// Adopts our own object idempotently, rejects a foreign one, and stamps the
    /// owner marker (create-only) so a retry can adopt it after a crash.
    async fn put_empty_owned_object(
        &self,
        object_key: &str,
        session: &UploadSessionId,
        content_type: Option<&str>,
    ) -> StorageResult<GcsObjectWrite> {
        if let Some(attrs) = self.get_object_attrs(object_key).await? {
            if attrs.owner_session.as_deref() == Some(session.as_str()) {
                return Ok(GcsObjectWrite {
                    etag: attrs.etag,
                    generation: attrs.generation.map(|generation| generation.to_string()),
                    generation_match: attrs.generation,
                });
            }
            return Err(StorageError::policy_rejected(format!(
                "GCS object key already exists: {object_key}"
            )));
        }
        let mut request = self
            .storage
            .write_object(self.layout.bucket_resource(), object_key, Bytes::new())
            .set_if_generation_match(0)
            .set_metadata([(
                SESSION_OWNER_META_KEY.to_string(),
                session.as_str().to_string(),
            )]);
        if let Some(content_type) = content_type {
            request = request.set_content_type(content_type);
        }
        match request.send_unbuffered().await {
            Ok(object) => Ok(GcsObjectWrite {
                etag: non_empty(object.etag),
                generation: if object.generation > 0 {
                    Some(object.generation.to_string())
                } else {
                    None
                },
                generation_match: positive_generation(object.generation),
            }),
            Err(err) if is_gcs_precondition_failed(&err) => {
                // Lost the create race. Adopt if our own session won (idempotent
                // across processes); otherwise it is a foreign collision.
                match self.get_object_attrs(object_key).await? {
                    Some(attrs) if attrs.owner_session.as_deref() == Some(session.as_str()) => {
                        Ok(GcsObjectWrite {
                            etag: attrs.etag,
                            generation: attrs.generation.map(|generation| generation.to_string()),
                            generation_match: attrs.generation,
                        })
                    }
                    _ => Err(StorageError::policy_rejected(format!(
                        "GCS object key already exists: {object_key}"
                    ))),
                }
            }
            Err(err) => Err(gcs_error("write empty object", err)),
        }
    }

    /// Assemble a by-number multipart upload (the [`StorageBackend::upload_part`]
    /// path). Components live as objects keyed by part number, not in the session
    /// state, so they are probed from the provider here — making concurrent,
    /// out-of-order part uploads race-free.
    async fn complete_compose_by_number(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        precheck_checksum(&stored.checksum_policy, provided_checksum.as_ref())?;
        // A by-number upload must declare its length: parts arrive concurrently
        // and `upload_part` writes components without the session lock, so without
        // a declared total a `complete` racing an in-flight part would compose
        // only the components present so far and publish a truncated object.
        let expected_size = stored.public.size.ok_or_else(|| {
            StorageError::policy_rejected(
                "multipart upload requires a declared length before completion",
            )
        })?;
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        let component_size = component_size_for(stored.max_bytes);
        let count = if expected_size == 0 {
            0
        } else {
            expected_size.div_ceil(component_size) as u32
        };

        if count == 0 {
            // Empty object: a single direct PUT (GCS `ComposeObject` requires at
            // least one source). HEAD-guarded by the ownership marker so a foreign
            // empty object at the key is rejected, not adopted.
            let checksum =
                validate_complete_checksum(&stored.checksum_policy, &[], provided_checksum)?;
            // Fence like the compose path: persist `Completing` before publishing
            // so a concurrent abort is refused and a crash after the PUT leaves a
            // recoverable session (a re-complete adopts the owner-stamped object)
            // rather than an orphaned visible object an abort would leave behind.
            if stored.public.status == UploadSessionStatus::Open
                || stored.completion_object_key.as_deref() != Some(object_key.as_str())
            {
                stored.public.status = UploadSessionStatus::Completing;
                stored.public.next_offset = Some(0);
                stored.completion_object_key = Some(object_key.clone());
                self.write_session(session, &mut stored).await?;
            }
            return match self
                .put_empty_owned_object(&object_key, session, stored.public.content_type.as_deref())
                .await
            {
                Ok(written) => {
                    self.finalize_compose(session, stored, 0, written, checksum)
                        .await
                }
                Err(err) => {
                    self.reopen_if_object_absent(session, &mut stored, &object_key)
                        .await;
                    Err(err)
                }
            };
        }

        let sources = self.probe_by_number_components(session, count).await?;
        let total = expected_size;
        ensure_size_limit(stored.max_bytes, stored.public.size, total)?;

        let (written, checksum) = match self
            .compose_by_number(
                &mut stored,
                session,
                &object_key,
                sources,
                total,
                provided_checksum,
            )
            .await
        {
            Ok(pair) => pair,
            Err(err) => {
                self.reopen_if_object_absent(session, &mut stored, &object_key)
                    .await;
                return Err(err);
            }
        };
        self.finalize_compose(session, stored, total, written, checksum)
            .await
    }

    /// Compose the probed component sources into the destination object,
    /// idempotently adopting our own already-composed object and rejecting a
    /// foreign one.
    async fn compose_by_number(
        &self,
        stored: &mut StoredUploadSession,
        session: &UploadSessionId,
        object_key: &str,
        sources: Vec<ComposeSource>,
        total: u64,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<(GcsObjectWrite, Option<ObjectChecksum>)> {
        let written = if let Some(attrs) = self.get_object_attrs(object_key).await? {
            if attrs.owner_session.as_deref() == Some(session.as_str()) {
                GcsObjectWrite {
                    etag: attrs.etag,
                    generation: attrs.generation.map(|generation| generation.to_string()),
                    generation_match: attrs.generation,
                }
            } else {
                return Err(StorageError::policy_rejected(format!(
                    "GCS object key already exists: {object_key}"
                )));
            }
        } else {
            if stored.public.status == UploadSessionStatus::Open
                || stored.completion_object_key.as_deref() != Some(object_key)
            {
                stored.public.status = UploadSessionStatus::Completing;
                stored.public.next_offset = Some(total);
                stored.completion_object_key = Some(object_key.to_string());
                self.write_session(session, stored).await?;
            }
            let mut metadata = std::collections::BTreeMap::new();
            metadata.insert(
                SESSION_OWNER_META_KEY.to_string(),
                session.as_str().to_string(),
            );
            match self
                .control
                .compose_object(
                    &self.layout,
                    ComposeRequest {
                        dest_key: object_key,
                        content_type: stored.public.content_type.as_deref(),
                        metadata,
                        sources,
                        if_generation_match: Some(0),
                    },
                )
                .await
            {
                Ok(written) => written,
                Err(StorageError::PolicyRejected { .. }) => {
                    match self.get_object_attrs(object_key).await? {
                        Some(attrs) if attrs.owner_session.as_deref() == Some(session.as_str()) => {
                            GcsObjectWrite {
                                etag: attrs.etag,
                                generation: attrs
                                    .generation
                                    .map(|generation| generation.to_string()),
                                generation_match: attrs.generation,
                            }
                        }
                        _ => {
                            return Err(StorageError::policy_rejected(format!(
                                "GCS object key already exists: {object_key}"
                            )));
                        }
                    }
                }
                Err(err) => return Err(err),
            }
        };
        let checksum = self
            .verify_object_checksum(object_key, &stored.checksum_policy, provided_checksum)
            .await?;
        Ok((written, checksum))
    }

    /// Finalize a by-number multipart session: record the object, persist, and
    /// remove the now-redundant component objects (the compose copied them).
    async fn finalize_compose(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        total: u64,
        written: GcsObjectWrite,
        checksum: Option<ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        let object = self.object_ref(&stored, total, written, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(total);
        stored.object = Some(object.clone());
        if let Some(state) = stored.native.compose_mut() {
            state.pending_tail = Vec::new();
        }
        self.write_session(session, &mut stored).await?;
        self.delete_component_chain(session).await;
        Ok(object)
    }
}

impl StorageBackend for GcsStorageBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Sequential proxy (the coalescing-tail compose path, the default) and
        // server-mediated native multipart (the by-number `upload_part` path) are
        // both assembled with a single `ComposeObject`.
        BackendCapabilities::default().with_native_multipart()
    }

    fn initiate_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: InitiateUpload,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let strategy = select_upload_mode(request.requested_strategy, self.capabilities())?;
            ensure_supported_checksum_policy(&request.policy.checksum)?;
            self.ensure_policy_is_supported(request.policy.max_bytes)?;
            self.ensure_requested_size_is_supported(request.size)?;
            // By-number multipart needs the total length up front: parts are
            // probed from component objects at completion (not tracked in the
            // session), so without a declared size neither the part count nor
            // completion can be reasoned about.
            if strategy == UploadStrategy::Multipart && request.size.is_none() {
                return Err(StorageError::policy_rejected(
                    "multipart uploads require a declared length (size) at initiation",
                ));
            }
            let max_bytes = request.policy.max_bytes.min(self.max_proxy_upload_bytes);
            // For the by-number path, pin the component size and cap the part
            // count to a single compose (`GCS_MAX_COMPOSE_SOURCES`); the
            // sequential path keeps the policy's chunk plan unchanged.
            let (plan, session_part_size) = if strategy == UploadStrategy::Multipart {
                let component_size = component_size_for(max_bytes);
                // min == preferred == max: every non-final part must be exactly
                // `component_size` (the last is the remainder), which is what
                // `upload_part` enforces — so the advertised plan matches it.
                let plan = TransferPlan {
                    min_part_size: Some(component_size),
                    preferred_part_size: Some(component_size),
                    max_part_size: Some(component_size),
                    max_parts: Some(max_bytes.div_ceil(component_size).max(1) as u32),
                    max_concurrent_parts: request.policy.max_concurrent_parts.max(1),
                    resumable: true,
                };
                (plan, Some(component_size))
            } else {
                let plan = TransferPlan {
                    min_part_size: request.policy.min_part_size,
                    preferred_part_size: request.policy.preferred_chunk_size,
                    max_part_size: request.policy.max_part_size,
                    max_parts: request.policy.max_parts,
                    max_concurrent_parts: 1,
                    resumable: true,
                };
                (plan, request.policy.preferred_chunk_size)
            };
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
                part_size: session_part_size,
                plan,
                uploaded_parts: Vec::new(),
                expires_at: expires_at(request.policy.expires_after),
            };
            let mut stored = StoredUploadSession {
                public: session.clone(),
                owner: ctx.actor.clone(),
                storage_key: request.storage_key,
                visibility: request.policy.visibility,
                max_bytes,
                checksum_policy: request.policy.checksum,
                request_metadata: request.metadata,
                object: None,
                completion_object_key: None,
                native: NativeUploadState::Compose(Default::default()),
                meta_generation: None,
            };
            // The session buffers its tail inline in the metadata, so no
            // separate staging object is created up front.
            self.create_session(&id, &mut stored).await?;
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
            let stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            // Native compose sessions keep an authoritative offset in the
            // metadata (flushed components plus the inline tail), so `inspect`
            // needs no provider round-trip to reconcile.
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
            let committed_offset = stored
                .native
                .compose()
                .map_or(0, |state| state.committed_len());
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
            self.append_compose(&session, stored, offset, bytes).await
        })
    }

    fn upload_part<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        number: u32,
        body: UploadBody,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            // Part numbers are 1-based. The HTTP route enforces this, but the
            // trait method is directly callable, so guard before the `number - 1`
            // index conversion below would underflow.
            if number == 0 {
                return Err(StorageError::policy_rejected(
                    "multipart part number must be at least 1",
                ));
            }
            let (component_size, expected_part_len, public) = {
                let lock = self.session_locks.lock(&session);
                let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
                let _guard = lock.lock().await;
                let mut stored = self.read_session(&session).await?;
                ensure_owner(&ctx.actor, &stored.owner)?;
                self.persist_expired_if_needed(&session, &mut stored)
                    .await?;
                ensure_open(&stored.public)?;
                if stored.public.strategy != UploadStrategy::Multipart {
                    return Err(StorageError::unsupported(
                        "GCS backend part upload requires the multipart strategy",
                    ));
                }
                // A multipart session always has a declared length (enforced at
                // initiation). The component layout is fixed: parts `1..count` are
                // exactly `component_size`, the last part is the remainder. Pinning
                // each part's length makes the composed object's size exactly the
                // declared total without trusting (emulator-unreliable) component
                // metadata sizes, and caps the part count to a single compose.
                let component_size = component_size_for(stored.max_bytes);
                let declared = stored.public.size.ok_or_else(|| {
                    StorageError::policy_rejected("multipart upload is missing a declared length")
                })?;
                let count = declared.div_ceil(component_size).max(1);
                if u64::from(number) > count {
                    return Err(StorageError::policy_rejected(format!(
                        "part number {number} exceeds the maximum of {count} parts for this upload"
                    )));
                }
                let expected_part_len = if u64::from(number) == count {
                    declared - (count - 1) * component_size
                } else {
                    component_size
                };
                (component_size, expected_part_len, stored.public.clone())
            };

            // Enforce the advertised concurrency server-side, then re-check the
            // captured expiry (the permit wait can outlast it).
            let max_concurrent = public.plan.max_concurrent_parts.max(1) as usize;
            let _permit = self
                .part_concurrency
                .semaphore(&session, max_concurrent)
                .acquire_owned()
                .await
                .map_err(|_| StorageError::UploadClosed {
                    session: session.to_string(),
                })?;
            if OffsetDateTime::now_utc() >= public.expires_at {
                return Err(StorageError::UploadClosed {
                    session: session.to_string(),
                });
            }

            // GCS writes a whole component object per part, so the bounded part is
            // collected (peak memory = one component per concurrent upload). Reject
            // a part whose actual length is not its fixed slot length, so the
            // composed total is exactly the declared size.
            let bytes = body.collect_capped(component_size).await?;
            if bytes.len() as u64 != expected_part_len {
                return Err(StorageError::policy_rejected(format!(
                    "part {number} must be exactly {expected_part_len} bytes, got {}",
                    bytes.len()
                )));
            }

            // Re-acquire the session lock and re-validate `Open` immediately before
            // writing the component, holding the lock across the write. This fences
            // against a concurrent complete/abort: those take the same lock, so the
            // write either lands before they run or this re-read sees the closed
            // session and writes nothing — no component is orphaned after cleanup.
            // (The slow body collection above ran without the lock, preserving
            // concurrency; only the bounded write is serialized.)
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
            let _guard = lock.lock().await;
            let stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            ensure_open(&stored.public)?;
            if stored.public.strategy != UploadStrategy::Multipart {
                return Err(StorageError::unsupported(
                    "GCS backend part upload requires the multipart strategy",
                ));
            }
            // The component is keyed by `number - 1`; `write_component` is
            // create-or-adopt-by-bytes, so a re-PUT of the same part is idempotent.
            self.write_component(&session, number - 1, bytes).await?;
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
                self.part_concurrency.forget(&request.session);
                return Ok(object);
            }
            self.persist_expired_if_needed(&request.session, &mut stored)
                .await?;
            ensure_completable(&stored.public)?;
            let session = request.session.clone();
            let result = if stored.public.strategy == UploadStrategy::Multipart {
                self.complete_compose_by_number(&session, stored, request.checksum)
                    .await
            } else {
                self.complete_compose(&session, stored, request.checksum)
                    .await
            };
            // Drop the concurrency pool only once completion succeeds; a failed
            // attempt leaves the session resumable and must keep its semaphore so
            // a later part can't create a fresh one and bypass the limit.
            if result.is_ok() {
                self.part_concurrency.forget(&session);
            }
            result
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
                    // Stop concurrent part uploads (queued and future ones fail
                    // immediately instead of writing components after the abort).
                    if stored.public.strategy == UploadStrategy::Multipart {
                        let max = stored.public.plan.max_concurrent_parts.max(1);
                        self.part_concurrency
                            .semaphore(&session, max as usize)
                            .close();
                    }
                    // Remove every component object so none linger. By-number
                    // parts can be sparse, so sweep the whole declared range
                    // rather than stopping at the first gap.
                    if stored.public.strategy == UploadStrategy::Multipart {
                        let component_size = component_size_for(stored.max_bytes);
                        let count = stored
                            .public
                            .size
                            .map(|size| size.div_ceil(component_size).max(1) as u32)
                            .unwrap_or(0);
                        self.delete_component_range(&session, count).await;
                    } else if stored.native.compose().is_some() {
                        self.delete_component_chain(&session).await;
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
            let meta_deleted = match read_session {
                AbortSessionRead::Known(_) | AbortSessionRead::Corrupt => {
                    self.delete_object_if_exists(&meta_key).await
                }
                AbortSessionRead::Missing => Ok(()),
            };
            meta_deleted?;
            self.part_concurrency.forget(&session);
            Ok(())
        })
    }
}
