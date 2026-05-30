use std::sync::Arc;

use std::collections::HashMap;

use azure_core::http::{Etag, NoFormat, RequestContent};
use azure_storage_blob::models::{
    BlobClientDeleteOptions, BlobClientDownloadOptions, BlobClientGetPropertiesResultHeaders,
    BlobClientUploadOptions, BlockBlobClientCommitBlockListOptions,
    BlockBlobClientCommitBlockListResultHeaders, BlockLookupList, DeleteSnapshotsOptionType,
    HttpRange,
};
use azure_storage_blob::{BlobContainerClient, BlobServiceClient};
use bytes::Bytes;
use futures_util::StreamExt;
use pocopine_storage::backend_common::{
    checked_new_offset, ensure_open, ensure_owner, ensure_size_limit,
    ensure_upload_length_can_be_set, expires_at, selected_strategy, UploadSessionLockCleanup,
    UploadSessionLockRegistry,
};
use pocopine_storage::checksum::{
    checksum_algorithm_to_compute, ensure_supported_checksum_policy, precheck_checksum,
    validate_complete_checksum, validate_complete_checksum_precomputed, StreamingChecksum,
};
use pocopine_storage::{
    ChecksumAlgorithm, CompleteUpload, InitiateUpload, ObjectChecksum, ObjectRef, StorageActor,
    StorageBackend, StorageBoxFuture, StorageContext, StorageError, StorageResult, TransferPlan,
    UploadSession, UploadSessionId, UploadSessionStatus, UploadStrategy,
};
use uuid::Uuid;

use crate::layout::{AzureKeyLayout, DEFAULT_INTERNAL_PREFIX};
use crate::state::{
    decode_session_object, AbortSessionRead, AzureObjectAttrs, AzureObjectBytes,
    AzureObjectMetadata, AzureObjectWrite, NativeUploadState, StoredUploadSession,
};
use crate::util::{
    azure_error, bytes_match_at, ensure_completable, is_azure_already_exists, is_azure_not_found,
    is_azure_precondition_failed, map_session_write_error, usize_from_u64,
};

const DEFAULT_BACKEND_NAME: &str = "azure";
const DEFAULT_MAX_PROXY_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SESSION_METADATA_BYTES: u64 = 256 * 1024;

/// Blob custom-metadata key stamping the owning upload session onto the final
/// committed blob, so a retry can recognize its own object after a crash.
/// Azure metadata names must be valid C# identifiers (no hyphens), hence the
/// underscores.
const SESSION_OWNER_META_KEY: &str = "pocopine_upload_session";

/// Deterministic, fixed-width block id for a block index. Azure requires every
/// block id in a blob to be the same (pre-base64) length, and the id is stable
/// per index so a retried `PATCH` re-stages the same block.
fn block_id(index: u64) -> Vec<u8> {
    format!("{index:016x}").into_bytes()
}

#[derive(Clone, Debug)]
enum BlobWritePrecondition {
    IfNotExists,
    IfMatch(String),
}

/// Storage backend backed by Azure Blob Storage.
///
/// Pass a pre-built [`BlobContainerClient`] so applications can configure
/// authentication, SAS URLs, emulators, custom transports, retry policy, and
/// tracing through the official Azure SDK.
#[derive(Clone)]
pub struct AzureBlobStorageBackend {
    name: &'static str,
    container: Arc<BlobContainerClient>,
    layout: AzureKeyLayout,
    max_proxy_upload_bytes: u64,
    session_locks: UploadSessionLockRegistry,
}

impl std::fmt::Debug for AzureBlobStorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureBlobStorageBackend")
            .field("name", &self.name)
            .field("container", &self.layout.container_name)
            .field("container_url", &self.layout.redacted_container_url())
            .field("object_prefix", &self.layout.object_prefix)
            .field("internal_prefix", &self.layout.internal_prefix)
            .field("max_proxy_upload_bytes", &self.max_proxy_upload_bytes)
            .finish_non_exhaustive()
    }
}

impl AzureBlobStorageBackend {
    /// Build a backend named `azure` for the given container.
    pub fn new(container: BlobContainerClient) -> StorageResult<Self> {
        Self::named(DEFAULT_BACKEND_NAME, container)
    }

    /// Build a backend named `azure` by deriving a container client from an
    /// Azure Blob service client.
    pub fn from_service(
        service: BlobServiceClient,
        container_name: impl AsRef<str>,
    ) -> StorageResult<Self> {
        if container_name.as_ref().trim().is_empty() {
            return Err(StorageError::policy_rejected(
                "Azure container name must not be empty",
            ));
        }
        Self::new(service.blob_container_client(container_name.as_ref()))
    }

    /// Build a backend with an explicit registry name.
    pub fn named(name: &'static str, container: BlobContainerClient) -> StorageResult<Self> {
        let layout =
            AzureKeyLayout::new(container.url(), None, DEFAULT_INTERNAL_PREFIX.to_string())?;
        Ok(Self {
            name,
            container: Arc::new(container),
            layout,
            max_proxy_upload_bytes: DEFAULT_MAX_PROXY_UPLOAD_BYTES,
            session_locks: UploadSessionLockRegistry::new(),
        })
    }

    /// Store completed blobs below a container prefix.
    ///
    /// The internal session prefix is also nested under this prefix to keep all
    /// Pocopine-owned blobs together.
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
    /// The default is 64 MiB because each append rewrites the staged blob and
    /// completion loads the staged bytes before the final Azure write.
    pub fn with_max_proxy_upload_bytes(mut self, max_bytes: u64) -> StorageResult<Self> {
        if max_bytes == 0 {
            return Err(StorageError::policy_rejected(
                "Azure max proxy upload bytes must be greater than zero",
            ));
        }
        self.max_proxy_upload_bytes = max_bytes;
        Ok(self)
    }

    pub fn container(&self) -> &str {
        &self.layout.container_name
    }

    pub fn container_url(&self) -> &str {
        &self.layout.container_url
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
            Ok(object) => {
                let stored = decode_session_object(object).map_err(|err| {
                    StorageError::backend(format!("read Azure upload metadata: {err}"))
                })?;
                if stored.meta_etag.is_none() {
                    return Err(StorageError::backend(
                        "Azure upload metadata ETag is unavailable",
                    ));
                }
                Ok(stored)
            }
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
                        event_name = "pocopine.storage.azure_corrupt_upload_metadata",
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
    ) -> StorageResult<AzureObjectBytes> {
        let key = self.layout.session_meta_key(session);
        self.get_object_bytes_with_limit(&key, MAX_SESSION_METADATA_BYTES)
            .await
    }

    async fn create_session(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
    ) -> StorageResult<()> {
        self.write_session_with_precondition(session, stored, BlobWritePrecondition::IfNotExists)
            .await
    }

    async fn write_session(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
    ) -> StorageResult<()> {
        let etag = stored
            .meta_etag
            .clone()
            .ok_or_else(|| StorageError::backend("Azure upload metadata ETag is unavailable"))?;
        self.write_session_with_precondition(session, stored, BlobWritePrecondition::IfMatch(etag))
            .await
    }

    async fn write_session_with_precondition(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        precondition: BlobWritePrecondition,
    ) -> StorageResult<()> {
        let key = self.layout.session_meta_key(session);
        let bytes = serde_json::to_vec_pretty(stored)
            .map_err(|err| StorageError::backend(format!("encode Azure upload metadata: {err}")))?;
        let written = self
            .put_object_bytes(
                &key,
                Bytes::from(bytes),
                Some("application/json"),
                Some(precondition),
            )
            .await
            .map_err(map_session_write_error)?;
        stored.meta_etag = written.etag;
        Ok(())
    }

    async fn get_staged_bytes(
        &self,
        session: &UploadSessionId,
        read_limit: u64,
    ) -> StorageResult<AzureObjectBytes> {
        let key = self.layout.session_bytes_key(session);
        match self.get_object_bytes_with_limit(&key, read_limit).await {
            Ok(bytes) => Ok(bytes),
            Err(StorageError::UnknownUploadSession { .. }) => Ok(AzureObjectBytes::empty()),
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
                event_name = "pocopine.storage.azure_staged_bytes_truncated",
                session = %session,
                actual,
                trusted = trusted_len,
            );
            staged.bytes.truncate(usize_from_u64(trusted_len)?);
            self.put_staged_bytes(
                session,
                Bytes::from(staged.bytes.clone()),
                staged.etag.map(BlobWritePrecondition::IfMatch),
            )
            .await?;
            return Ok(staged.bytes);
        }
        if actual == trusted_len {
            return Ok(staged.bytes);
        }
        Err(StorageError::backend(format!(
            "Azure staged upload blob is shorter than committed metadata: expected {trusted_len} bytes, got {actual}"
        )))
    }

    async fn ensure_staged_not_shorter_than_metadata(
        &self,
        session: &UploadSessionId,
        trusted_len: u64,
    ) -> StorageResult<()> {
        let read_limit = trusted_len
            .checked_add(1)
            .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))?;
        let staged = self.get_staged_bytes(session, read_limit).await?;
        let actual = staged.bytes.len() as u64;
        if actual < trusted_len {
            return Err(StorageError::backend(format!(
                "Azure staged upload blob is shorter than committed metadata: expected {trusted_len} bytes, got {actual}"
            )));
        }
        Ok(())
    }

    async fn put_staged_bytes(
        &self,
        session: &UploadSessionId,
        bytes: Bytes,
        precondition: Option<BlobWritePrecondition>,
    ) -> StorageResult<AzureObjectWrite> {
        let key = self.layout.session_bytes_key(session);
        self.put_object_bytes(&key, bytes, Some("application/octet-stream"), precondition)
            .await
            .map_err(map_session_write_error)
    }

    async fn get_object_bytes_with_limit(
        &self,
        key: &str,
        max_bytes: u64,
    ) -> StorageResult<AzureObjectBytes> {
        let metadata = self.get_object_metadata(key).await?;
        self.download_object_bytes_with_limit(key, max_bytes, metadata)
            .await
    }

    async fn download_object_bytes_with_limit(
        &self,
        key: &str,
        max_bytes: u64,
        metadata: AzureObjectMetadata,
    ) -> StorageResult<AzureObjectBytes> {
        if metadata.size == Some(0) {
            return Ok(AzureObjectBytes {
                bytes: Vec::new(),
                etag: metadata.etag,
                version_id: metadata.version_id,
                truncated: false,
            });
        }
        let probe_limit = max_bytes
            .checked_add(1)
            .ok_or_else(|| StorageError::policy_rejected("blob read limit overflowed"))?;
        let read_len = metadata
            .size
            .map_or(probe_limit, |size| size.min(probe_limit));
        let mut options = BlobClientDownloadOptions {
            range: Some(HttpRange::new(0, read_len)),
            ..Default::default()
        };
        if let Some(etag) = &metadata.etag {
            options.if_match = Some(Etag::from(etag.clone()));
        }
        let response = self
            .container
            .blob_client(key)
            .download(Some(options))
            .await
            .map_err(|err| {
                if is_azure_not_found(&err) {
                    StorageError::unknown_upload_session(key.to_string())
                } else if is_azure_precondition_failed(&err) {
                    StorageError::conflict("Azure blob changed while reading")
                } else {
                    azure_error("download blob", err)
                }
            })?;
        let collect_limit = usize_from_u64(probe_limit)?;
        let mut body = response.body;
        let mut bytes = Vec::with_capacity(usize_from_u64(read_len.min(probe_limit))?);
        let mut body_exceeded_limit = false;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|err| azure_error("read blob body", err))?;
            let remaining = collect_limit.saturating_sub(bytes.len());
            if chunk.len() > remaining {
                bytes.extend_from_slice(&chunk[..remaining]);
                body_exceeded_limit = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
            if bytes.len() == collect_limit {
                body_exceeded_limit = true;
                break;
            }
        }
        let truncated =
            metadata.size.map(|size| size > max_bytes).unwrap_or(false) || body_exceeded_limit;
        if bytes.len() > usize_from_u64(max_bytes)? {
            bytes.truncate(usize_from_u64(max_bytes)?);
        }
        Ok(AzureObjectBytes {
            bytes,
            etag: response
                .properties
                .etag
                .map(|etag| etag.to_string())
                .or(metadata.etag),
            version_id: response.properties.version_id.or(metadata.version_id),
            truncated,
        })
    }

    async fn get_object_metadata(&self, key: &str) -> StorageResult<AzureObjectMetadata> {
        let response = self
            .container
            .blob_client(key)
            .get_properties(None)
            .await
            .map_err(|err| {
                if is_azure_not_found(&err) {
                    StorageError::unknown_upload_session(key.to_string())
                } else {
                    azure_error("read blob metadata", err)
                }
            })?;
        let size = response
            .content_length()
            .map_err(|err| azure_error("read blob content length", err))?;
        let etag = response
            .etag()
            .map_err(|err| azure_error("read blob etag", err))?
            .map(|etag| etag.to_string());
        let version_id = response
            .version_id()
            .map_err(|err| azure_error("read blob version id", err))?;
        Ok(AzureObjectMetadata {
            size,
            etag,
            version_id,
        })
    }

    async fn put_object_bytes(
        &self,
        key: &str,
        bytes: Bytes,
        content_type: Option<&str>,
        precondition: Option<BlobWritePrecondition>,
    ) -> StorageResult<AzureObjectWrite> {
        let mut options = BlobClientUploadOptions::default();
        if let Some(content_type) = content_type {
            options.blob_content_type = Some(content_type.to_string());
        }
        if let Some(precondition) = precondition {
            match precondition {
                BlobWritePrecondition::IfNotExists => {
                    options.if_none_match = Some(Etag::from("*"));
                }
                BlobWritePrecondition::IfMatch(etag) => {
                    options.if_match = Some(Etag::from(etag));
                }
            }
        }
        let content: RequestContent<Bytes, NoFormat> = bytes.into();
        let response = self
            .container
            .blob_client(key)
            .upload(content, Some(options))
            .await
            .map_err(|err| {
                if is_azure_precondition_failed(&err) || is_azure_already_exists(&err) {
                    StorageError::conflict("Azure blob write precondition failed")
                } else {
                    azure_error("write blob", err)
                }
            })?;
        Ok(AzureObjectWrite {
            etag: response.etag.map(|etag| etag.to_string()),
            version_id: response.version_id,
        })
    }

    async fn put_completed_object(
        &self,
        key: &str,
        bytes: Bytes,
        content_type: Option<&str>,
    ) -> StorageResult<AzureObjectWrite> {
        match self.compare_existing_completed_object(key, &bytes).await {
            Ok(Some(existing)) => return Ok(existing),
            Ok(None) => {
                return Err(StorageError::policy_rejected(format!(
                    "Azure blob key already exists with different bytes: {key}"
                )));
            }
            Err(StorageError::UnknownUploadSession { .. }) => {}
            Err(err) => return Err(err),
        }
        match self
            .put_object_bytes(
                key,
                bytes.clone(),
                content_type,
                Some(BlobWritePrecondition::IfNotExists),
            )
            .await
        {
            Ok(written) => Ok(written),
            Err(StorageError::Conflict { .. }) => {
                match self.compare_existing_completed_object(key, &bytes).await {
                    Ok(Some(existing)) => Ok(existing),
                    Ok(None) | Err(StorageError::UnknownUploadSession { .. }) => {
                        Err(StorageError::policy_rejected(format!(
                            "Azure blob key already exists: {key}"
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
    ) -> StorageResult<Option<AzureObjectWrite>> {
        let metadata = self.get_object_metadata(key).await?;
        if let Some(size) = metadata.size {
            if size != bytes.len() as u64 {
                return Ok(None);
            }
        }
        let metadata_etag = metadata.etag.clone();
        let metadata_version_id = metadata.version_id.clone();
        let existing = self
            .download_object_bytes_with_limit(key, bytes.len() as u64, metadata)
            .await?;
        if existing.truncated || existing.bytes.as_slice() != bytes.as_ref() {
            return Ok(None);
        }
        Ok(Some(AzureObjectWrite {
            etag: existing.etag.or(metadata_etag),
            version_id: existing.version_id.or(metadata_version_id),
        }))
    }

    async fn delete_object(&self, key: &str) -> StorageResult<()> {
        let options = BlobClientDeleteOptions {
            delete_snapshots: Some(DeleteSnapshotsOptionType::Include),
            ..Default::default()
        };
        self.container
            .blob_client(key)
            .delete(Some(options))
            .await
            .map_err(|err| {
                if is_azure_not_found(&err) {
                    StorageError::unknown_upload_session(key.to_string())
                } else {
                    azure_error("delete blob", err)
                }
            })?;
        Ok(())
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

    async fn cleanup_staged_bytes_best_effort(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
    ) {
        if let Err(err) = self.cleanup_staged_bytes(session, stored).await {
            tracing::warn!(
                target: "pocopine.log",
                event_name = "pocopine.storage.azure_cleanup_pending",
                session = %session,
                error = %err,
            );
        }
    }

    async fn rollback_completion_after_error(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        error: StorageError,
    ) -> StorageError {
        stored.public.status = UploadSessionStatus::Open;
        stored.completion_object_key = None;
        match self.write_session(session, stored).await {
            Ok(()) => error,
            Err(rollback_error) => {
                tracing::error!(
                    target: "pocopine.log",
                    event_name = "pocopine.storage.azure_completion_rollback_failed",
                    session = %session,
                    original_error = %error,
                    rollback_error = %rollback_error,
                );
                StorageError::backend(format!(
                    "Azure complete upload failed ({error}); rollback failed ({rollback_error})"
                ))
            }
        }
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
        written: AzureObjectWrite,
        checksum: Option<ObjectChecksum>,
    ) -> ObjectRef {
        let mut metadata = stored.storage_key.metadata.0.clone();
        metadata.extend(stored.request_metadata.clone());
        ObjectRef {
            backend: self.name.to_string(),
            scope: stored.public.scope.clone(),
            key: stored.storage_key.key.to_string(),
            version: written.version_id,
            etag: written.etag,
            checksum,
            content_type: stored.public.content_type.clone(),
            size,
            visibility: stored.visibility,
            metadata,
        }
    }

    // --- native block-blob helpers --------------------------------------------

    /// Stage one block (`Put Block`). Re-staging the same index overwrites the
    /// uncommitted block, so retries are idempotent.
    async fn stage_block(&self, object_key: &str, index: u64, bytes: Bytes) -> StorageResult<()> {
        let id = block_id(index);
        let len = bytes.len() as u64;
        let content: RequestContent<Bytes, NoFormat> = bytes.into();
        self.container
            .blob_client(object_key)
            .block_blob_client()
            .stage_block(&id, len, content, None)
            .await
            .map_err(|err| azure_error("stage block", err))?;
        Ok(())
    }

    /// Assemble blocks `0..count` into the destination blob (`Put Block List`),
    /// stamping the owning session and (optionally) refusing to overwrite.
    async fn commit_block_list(
        &self,
        object_key: &str,
        count: u64,
        content_type: Option<&str>,
        session: &UploadSessionId,
        no_overwrite: bool,
    ) -> StorageResult<AzureObjectWrite> {
        let blocklist = BlockLookupList {
            latest: Some((0..count).map(block_id).collect()),
            ..Default::default()
        };
        let mut options = BlockBlobClientCommitBlockListOptions {
            metadata: Some(HashMap::from([(
                SESSION_OWNER_META_KEY.to_string(),
                session.as_str().to_string(),
            )])),
            ..Default::default()
        };
        if let Some(content_type) = content_type {
            options.blob_content_type = Some(content_type.to_string());
        }
        if no_overwrite {
            options.if_none_match = Some(Etag::from("*"));
        }
        let content = blocklist
            .try_into()
            .map_err(|err| azure_error("encode block list", err))?;
        let response = self
            .container
            .blob_client(object_key)
            .block_blob_client()
            .commit_block_list(content, Some(options))
            .await
            .map_err(|err| {
                if is_azure_precondition_failed(&err) || is_azure_already_exists(&err) {
                    StorageError::conflict("Azure blob write precondition failed")
                } else {
                    azure_error("commit block list", err)
                }
            })?;
        Ok(AzureObjectWrite {
            etag: response
                .etag()
                .map_err(|err| azure_error("read commit etag", err))?
                .map(|etag| etag.to_string()),
            version_id: response
                .version_id()
                .map_err(|err| azure_error("read commit version id", err))?,
        })
    }

    /// Fetch a blob's etag/version plus the ownership marker in custom metadata.
    /// `None` when the blob does not exist.
    async fn get_object_attrs(&self, key: &str) -> StorageResult<Option<AzureObjectAttrs>> {
        match self.container.blob_client(key).get_properties(None).await {
            Ok(response) => {
                let etag = response
                    .etag()
                    .map_err(|err| azure_error("read blob etag", err))?
                    .map(|etag| etag.to_string());
                let version_id = response
                    .version_id()
                    .map_err(|err| azure_error("read blob version id", err))?;
                let owner_session = response
                    .metadata()
                    .map_err(|err| azure_error("read blob metadata", err))?
                    .get(SESSION_OWNER_META_KEY)
                    .cloned();
                Ok(Some(AzureObjectAttrs {
                    etag,
                    version_id,
                    owner_session,
                }))
            }
            Err(err) if is_azure_not_found(&err) => Ok(None),
            Err(err) => Err(azure_error("read blob metadata", err)),
        }
    }

    /// Stream the committed blob through `algorithm` without buffering it.
    async fn stream_object_checksum(
        &self,
        key: &str,
        algorithm: ChecksumAlgorithm,
    ) -> StorageResult<ObjectChecksum> {
        let response = self
            .container
            .blob_client(key)
            .download(None)
            .await
            .map_err(|err| azure_error("download blob for checksum", err))?;
        let mut body = response.body;
        let mut hasher = StreamingChecksum::new(algorithm);
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|err| azure_error("read blob body for checksum", err))?;
            hasher.update(&chunk);
        }
        Ok(hasher.finalize())
    }

    /// Reopen a failed completion ONLY when the destination blob is definitively
    /// absent or foreign (see the GCS backend for the rationale). An
    /// indeterminate lookup keeps the session `Completing`.
    async fn reopen_if_object_absent(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        object_key: &str,
    ) {
        let absent_or_foreign = match self.get_object_attrs(object_key).await {
            Ok(None) => true,
            Ok(Some(attrs)) => attrs.owner_session.as_deref() != Some(session.as_str()),
            Err(_) => false,
        };
        if absent_or_foreign {
            stored.public.status = UploadSessionStatus::Open;
            stored.completion_object_key = None;
            let _ = self.write_session(session, stored).await;
        }
    }

    // --- native block-blob upload flow ----------------------------------------

    async fn append_block(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        offset: u64,
        bytes: Bytes,
    ) -> StorageResult<UploadSession> {
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
        let new_offset = checked_new_offset(offset, bytes.len())?;
        ensure_size_limit(stored.max_bytes, stored.public.size, new_offset)?;
        if bytes.is_empty() {
            return Ok(stored.public);
        }
        let index = stored.native.block().expect("block state").next_index;
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        // Stage the block first; the index is only recorded once the metadata
        // write below lands, so a crash leaves the durable offset unchanged and a
        // retry re-stages the same (idempotent) block.
        self.stage_block(&object_key, index, bytes).await?;
        if let Some(state) = stored.native.block_mut() {
            state.next_index = index + 1;
        }
        stored.public.next_offset = Some(new_offset);
        self.write_session(session, &mut stored).await?;
        Ok(stored.public)
    }

    async fn complete_block(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        let total = stored.public.next_offset.unwrap_or(0);
        if let Some(expected) = stored.public.size {
            if total != expected {
                return Err(StorageError::policy_rejected(format!(
                    "upload is incomplete: expected {expected} bytes, got {total}"
                )));
            }
        }
        ensure_size_limit(stored.max_bytes, stored.public.size, total)?;
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());

        let (written, checksum) = match self
            .complete_with_blocks(session, &mut stored, &object_key, provided_checksum)
            .await
        {
            Ok(pair) => pair,
            Err(err) => {
                self.reopen_if_object_absent(session, &mut stored, &object_key)
                    .await;
                return Err(err);
            }
        };

        let object = self.object_ref(&stored, total, written, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(total);
        stored.object = Some(object.clone());
        self.write_session(session, &mut stored).await?;
        Ok(object)
    }

    /// Finalize a block session. The caller reopens the (possibly `Completing`)
    /// session on any error.
    async fn complete_with_blocks(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        object_key: &str,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<(AzureObjectWrite, Option<ObjectChecksum>)> {
        // Reject a missing-required / disallowed-algorithm checksum before
        // committing, while the session is still resumable.
        precheck_checksum(&stored.checksum_policy, provided_checksum.as_ref())?;
        let total = stored.public.next_offset.unwrap_or(0);
        let count = stored.native.block().expect("block state").next_index;

        // HEAD on every attempt: adopt our own already-committed blob (matched by
        // the ownership marker), reject a foreign one. `If-None-Match: *` on the
        // commit closes the TOCTOU window.
        let written = if let Some(attrs) = self.get_object_attrs(object_key).await? {
            if attrs.owner_session.as_deref() == Some(session.as_str()) {
                AzureObjectWrite {
                    etag: attrs.etag,
                    version_id: attrs.version_id,
                }
            } else {
                return Err(StorageError::policy_rejected(format!(
                    "Azure blob key already exists: {object_key}"
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
            match self
                .commit_block_list(
                    object_key,
                    count,
                    stored.public.content_type.as_deref(),
                    session,
                    true,
                )
                .await
            {
                Ok(written) => written,
                Err(StorageError::Conflict { .. }) => {
                    // A blob appeared at the key during completion; adopt it only
                    // if it is ours.
                    match self.get_object_attrs(object_key).await? {
                        Some(attrs) if attrs.owner_session.as_deref() == Some(session.as_str()) => {
                            AzureObjectWrite {
                                etag: attrs.etag,
                                version_id: attrs.version_id,
                            }
                        }
                        _ => {
                            return Err(StorageError::policy_rejected(format!(
                                "Azure blob key already exists: {object_key}"
                            )))
                        }
                    }
                }
                Err(err) => return Err(err),
            }
        };

        let checksum = match checksum_algorithm_to_compute(
            &stored.checksum_policy,
            provided_checksum.as_ref(),
        ) {
            Some(algorithm) => {
                let computed = self.stream_object_checksum(object_key, algorithm).await?;
                match validate_complete_checksum_precomputed(
                    &stored.checksum_policy,
                    Some(computed),
                    provided_checksum,
                ) {
                    Ok(checksum) => checksum,
                    Err(err) => {
                        // Remove the committed-but-invalid blob; surface a delete
                        // failure (a live invalid object is worse).
                        self.delete_object(object_key).await.map_err(|delete_err| {
                            tracing::error!(
                                target: "pocopine.log",
                                event_name = "pocopine.storage.azure_invalid_object_cleanup_failed",
                                object_key = %object_key,
                                checksum_error = %err,
                                delete_error = %delete_err,
                            );
                            delete_err
                        })?;
                        return Err(err);
                    }
                }
            }
            None => validate_complete_checksum_precomputed(
                &stored.checksum_policy,
                None,
                provided_checksum,
            )?,
        };
        Ok((written, checksum))
    }

    // --- legacy staged-object rewrite flow (pre-block sessions) ---------------

    async fn append_legacy(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        offset: u64,
        bytes: Bytes,
    ) -> StorageResult<UploadSession> {
        let expected = stored.public.next_offset.unwrap_or(0);
        let new_offset = checked_new_offset(offset, bytes.len())?;
        let read_limit = expected
            .max(new_offset)
            .checked_add(1)
            .ok_or_else(|| StorageError::policy_rejected("upload byte offset overflowed"))?;
        let mut staged_object = self.get_staged_bytes(session, read_limit).await?;
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
                self.write_session(session, &mut stored).await?;
                return Ok(stored.public);
            }
            tracing::warn!(
                target: "pocopine.log",
                event_name = "pocopine.storage.azure_staged_bytes_truncated",
                session = %session,
                actual = staged_len,
                trusted = expected,
            );
            staged_object.bytes.truncate(usize_from_u64(expected)?);
            let written = self
                .put_staged_bytes(
                    session,
                    Bytes::from(staged_object.bytes.clone()),
                    staged_object
                        .etag
                        .clone()
                        .map(BlobWritePrecondition::IfMatch),
                )
                .await?;
            staged_object.etag = written.etag;
        } else if staged_len < expected {
            return Err(StorageError::backend(format!(
                "Azure staged upload blob is shorter than committed metadata: expected {expected} bytes, got {staged_len}"
            )));
        }
        ensure_size_limit(stored.max_bytes, stored.public.size, new_offset)?;
        staged_object.bytes.extend_from_slice(&bytes);
        self.put_staged_bytes(
            session,
            Bytes::from(staged_object.bytes),
            staged_object.etag.map(BlobWritePrecondition::IfMatch),
        )
        .await?;
        stored.public.next_offset = Some(new_offset);
        self.write_session(session, &mut stored).await?;
        Ok(stored.public)
    }

    async fn complete_legacy(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        let trusted = stored.public.next_offset.unwrap_or(0);
        let staged = self.reconcile_staged_bytes(session, trusted).await?;
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
            validate_complete_checksum(&stored.checksum_policy, &staged, provided_checksum)?;
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        if stored.public.status == UploadSessionStatus::Open
            || stored.public.next_offset != Some(actual)
            || stored.completion_object_key.as_deref() != Some(object_key.as_str())
        {
            stored.public.status = UploadSessionStatus::Completing;
            stored.public.next_offset = Some(actual);
            stored.completion_object_key = Some(object_key.clone());
            self.write_session(session, &mut stored).await?;
        }
        let staged = Bytes::from(staged);
        let written = match self
            .put_completed_object(&object_key, staged, stored.public.content_type.as_deref())
            .await
        {
            Ok(written) => written,
            Err(err) => {
                return Err(self
                    .rollback_completion_after_error(session, &mut stored, err)
                    .await);
            }
        };
        let object = self.object_ref(&stored, actual, written, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(actual);
        stored.object = Some(object.clone());
        stored.cleanup_pending = true;
        self.write_session(session, &mut stored).await?;
        self.cleanup_staged_bytes(session, &mut stored).await?;
        Ok(object)
    }
}

impl StorageBackend for AzureBlobStorageBackend {
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
                native: NativeUploadState::Block(Default::default()),
                meta_etag: None,
            };
            // Native sessions stage blocks directly against the destination blob;
            // there is no separate staged `bytes.tmp` object to create up front.
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
            // Native block sessions keep an authoritative offset in the metadata;
            // only the legacy staged-object path reconciles against storage.
            if stored.public.status == UploadSessionStatus::Open && stored.native.block().is_none()
            {
                self.ensure_staged_not_shorter_than_metadata(
                    &session,
                    stored.public.next_offset.unwrap_or(0),
                )
                .await?;
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
            if stored.native.block().is_none() {
                self.ensure_staged_not_shorter_than_metadata(&session, committed_offset)
                    .await?;
            }
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
                    "Azure backend currently supports sequential proxy uploads only",
                ));
            }
            if stored.native.block().is_some() {
                self.append_block(&session, stored, offset, bytes).await
            } else {
                self.append_legacy(&session, stored, offset, bytes).await
            }
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
                self.cleanup_staged_bytes_best_effort(&request.session, &mut stored)
                    .await;
                return Ok(object);
            }
            self.persist_expired_if_needed(&request.session, &mut stored)
                .await?;
            ensure_completable(&stored.public)?;
            let session = request.session.clone();
            if stored.native.block().is_some() {
                self.complete_block(&session, stored, request.checksum)
                    .await
            } else {
                self.complete_legacy(&session, stored, request.checksum)
                    .await
            }
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
                            "corrupt Azure upload metadata can only be aborted by a system actor",
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

#[cfg(test)]
mod tests {
    use azure_core::http::Url;
    use azure_storage_blob::BlobContainerClient;

    use super::*;

    #[test]
    fn debug_redacts_container_sas_query() {
        let url =
            Url::parse("https://account.blob.core.windows.net/pocopine?sv=2024-01-01&sig=secret")
                .unwrap();
        let container =
            BlobContainerClient::new(url, None, None).expect("build Azure container client");
        let backend = AzureBlobStorageBackend::new(container).unwrap();

        let debug = format!("{backend:?}");

        assert!(debug.contains("AzureBlobStorageBackend"));
        assert!(debug.contains("https://account.blob.core.windows.net/pocopine"));
        assert!(!debug.contains("sig="));
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("?sv="));
    }
}
