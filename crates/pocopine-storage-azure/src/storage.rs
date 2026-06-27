use std::sync::Arc;

use std::collections::{HashMap, HashSet};

use azure_core::http::{Etag, NoFormat, RequestContent};
use azure_storage_blob::models::{
    BlobClientDeleteOptions, BlobClientDownloadOptions, BlobClientGetPropertiesResultHeaders,
    BlobClientUploadOptions, BlockBlobClientCommitBlockListOptions,
    BlockBlobClientCommitBlockListResultHeaders, BlockListType, BlockLookupList,
    DeleteSnapshotsOptionType, HttpRange,
};
use azure_storage_blob::{BlobContainerClient, BlobServiceClient};
use bytes::Bytes;
use futures_util::StreamExt;
use pocopine_storage::backend_common::{
    UploadConcurrencyRegistry, UploadSessionLockCleanup, UploadSessionLockRegistry,
    checked_new_offset, ensure_open, ensure_owner, ensure_size_limit,
    ensure_upload_length_can_be_set, expires_at, select_upload_mode,
};
use pocopine_storage::checksum::{
    StreamingChecksum, checksum_algorithm_to_compute, ensure_supported_checksum_policy,
    precheck_checksum, validate_complete_checksum_precomputed,
};
use pocopine_storage::{
    BackendCapabilities, ChecksumAlgorithm, CompleteUpload, InitiateUpload, ObjectBody,
    ObjectChecksum, ObjectRead, ObjectRef, ObjectVisibility, SafeObjectKey, StorageActor,
    StorageBackend, StorageBoxFuture, StorageContext, StorageError, StorageResult, TransferPlan,
    UploadBody, UploadSession, UploadSessionId, UploadSessionStatus, UploadStrategy,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::layout::{AzureKeyLayout, DEFAULT_INTERNAL_PREFIX};
use crate::state::{
    AbortSessionRead, AzureObjectAttrs, AzureObjectBytes, AzureObjectMetadata, AzureObjectWrite,
    NativeUploadState, StoredUploadSession, decode_session_object,
};
use crate::util::{
    azure_error, ensure_completable, is_azure_already_exists, is_azure_not_found,
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

/// Azure allows at most 50,000 committed blocks per blob. Blocks are sized so the
/// count never exceeds this for an upload up to the backend's size cap.
const AZURE_MAX_BLOCKS: u64 = 50_000;

/// Floor for a coalesced block, to bound the block count and per-append churn.
const MIN_BLOCK_SIZE: u64 = 1024 * 1024;

/// Session-isolated, fixed-width block id for a block index.
///
/// Azure block ids must be <= 64 base64 bytes and equal-length within a single
/// commit. A 96-bit token derived from the session id namespaces the (otherwise
/// per-blob) uncommitted block space so a different upload targeting the same
/// destination key can never contribute blocks to our commit. The id is stable
/// per (session, index) so a retried `PATCH` re-stages the same block.
fn block_id(session: &UploadSessionId, index: u64) -> Vec<u8> {
    let token = &pocopine_crypto::sha256_hex(session.as_str().as_bytes())[..24];
    format!("{token}{index:012x}").into_bytes()
}

/// Block size for a session whose maximum object size is `max_bytes`. Scaled
/// (rounded to a whole MiB) so the block count stays within [`AZURE_MAX_BLOCKS`];
/// never below [`MIN_BLOCK_SIZE`]. For the default 64 MiB cap this is 1 MiB.
fn block_size_for(max_bytes: u64) -> u64 {
    let by_count = max_bytes
        .div_ceil(AZURE_MAX_BLOCKS)
        .div_ceil(MIN_BLOCK_SIZE)
        * MIN_BLOCK_SIZE;
    by_count.max(MIN_BLOCK_SIZE)
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
    part_concurrency: UploadConcurrencyRegistry,
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
            part_concurrency: UploadConcurrencyRegistry::new(),
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

    /// Override the maximum object size accepted by this backend.
    ///
    /// The cap also sizes the per-session block width (`block_size_for`) so the
    /// committed block count stays within Azure's per-blob limit. The default is
    /// 64 MiB.
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
        self.get_object_bytes_with_limit(&key, self.session_metadata_read_limit())
            .await
    }

    /// Read cap for `session.json`. Native block sessions buffer their tail inline
    /// (base64), so the cap must accommodate one block plus the fixed metadata,
    /// above the bare [`MAX_SESSION_METADATA_BYTES`] used for the non-tail fields.
    fn session_metadata_read_limit(&self) -> u64 {
        block_size_for(self.max_proxy_upload_bytes)
            .saturating_mul(2)
            .saturating_add(MAX_SESSION_METADATA_BYTES)
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
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|err| azure_error("read blob body", err))?;
            let remaining = collect_limit.saturating_sub(bytes.len());
            if chunk.len() > remaining {
                bytes.extend_from_slice(&chunk[..remaining]);
                break;
            }
            bytes.extend_from_slice(&chunk);
            if bytes.len() == collect_limit {
                break;
            }
        }
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
        let content_type = response
            .content_type()
            .map_err(|err| azure_error("read blob content type", err))?;
        Ok(AzureObjectMetadata {
            size,
            etag,
            content_type,
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
    async fn stage_block(
        &self,
        object_key: &str,
        session: &UploadSessionId,
        index: u64,
        bytes: Bytes,
    ) -> StorageResult<()> {
        let id = block_id(session, index);
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
            latest: Some((0..count).map(|index| block_id(session, index)).collect()),
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
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());

        // Coalesce sub-block chunks so the committed block count stays within
        // Azure's 50,000-block limit. The tail is durable in `stored` until the
        // single atomic metadata write below.
        let tail =
            std::mem::take(&mut stored.native.block_mut().expect("block state").pending_tail);
        let combined: Bytes = if tail.is_empty() {
            bytes
        } else {
            let mut combined = tail;
            combined.extend_from_slice(&bytes);
            Bytes::from(combined)
        };
        let block_size = block_size_for(stored.max_bytes) as usize;
        let mut pos = 0usize;
        while combined.len() - pos >= block_size {
            let index = stored.native.block().expect("block state").next_index;
            let part = combined.slice(pos..pos + block_size);
            self.stage_block(&object_key, session, index, part).await?;
            if let Some(state) = stored.native.block_mut() {
                state.next_index = index + 1;
                state.flushed_len += block_size as u64;
            }
            pos += block_size;
        }
        if let Some(state) = stored.native.block_mut() {
            state.pending_tail = combined.slice(pos..).to_vec();
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
        if let Some(expected) = stored.public.size
            && total != expected
        {
            return Err(StorageError::policy_rejected(format!(
                "upload is incomplete: expected {expected} bytes, got {total}"
            )));
        }
        ensure_size_limit(stored.max_bytes, stored.public.size, total)?;
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());

        // complete_with_blocks owns session-state changes on failure: it reopens
        // for pre-commit errors, and terminally aborts on a post-commit checksum
        // mismatch (the consumed blocks cannot be re-committed).
        let (written, checksum) = self
            .complete_with_blocks(session, &mut stored, &object_key, provided_checksum)
            .await?;

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
                // Foreign blob (session still Open here): reject without
                // overwriting.
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
            // Flush the final remainder as the last block. Failures here are
            // pre-commit (blocks not yet consumed), so reopen for retry.
            let tail = stored
                .native
                .block()
                .expect("block state")
                .pending_tail
                .clone();
            if !tail.is_empty() {
                let index = stored.native.block().expect("block state").next_index;
                let tail_len = tail.len() as u64;
                if let Err(err) = self
                    .stage_block(object_key, session, index, Bytes::from(tail))
                    .await
                {
                    self.reopen_if_object_absent(session, stored, object_key)
                        .await;
                    return Err(err);
                }
                if let Some(state) = stored.native.block_mut() {
                    state.next_index = index + 1;
                    state.flushed_len += tail_len;
                    state.pending_tail = Vec::new();
                }
                if let Err(err) = self.write_session(session, stored).await {
                    self.reopen_if_object_absent(session, stored, object_key)
                        .await;
                    return Err(err);
                }
            }
            let count = stored.native.block().expect("block state").next_index;
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
                    // if it is ours, otherwise reject and reopen for the owner.
                    match self.get_object_attrs(object_key).await {
                        Ok(Some(attrs))
                            if attrs.owner_session.as_deref() == Some(session.as_str()) =>
                        {
                            AzureObjectWrite {
                                etag: attrs.etag,
                                version_id: attrs.version_id,
                            }
                        }
                        Ok(_) => {
                            self.reopen_if_object_absent(session, stored, object_key)
                                .await;
                            return Err(StorageError::policy_rejected(format!(
                                "Azure blob key already exists: {object_key}"
                            )));
                        }
                        Err(err) => {
                            self.reopen_if_object_absent(session, stored, object_key)
                                .await;
                            return Err(err);
                        }
                    }
                }
                Err(err) => {
                    // Commit failed without consuming the blocks; reopen unless our
                    // blob is actually live (a lost response), in which case a
                    // retry adopts it.
                    self.reopen_if_object_absent(session, stored, object_key)
                        .await;
                    return Err(err);
                }
            }
        };

        let checksum = self
            .verify_committed_checksum_or_abort(session, stored, object_key, provided_checksum)
            .await?;
        Ok((written, checksum))
    }

    /// Validate a just-committed blob against the checksum policy by streaming it
    /// (only when a checksum is required/supplied). On mismatch the blocks are
    /// already consumed by the commit, so the upload cannot be re-committed: the
    /// invalid blob is deleted and the session is marked terminally `Aborted`
    /// (the client must re-initiate). A failure of either cleanup step is
    /// surfaced — a live invalid blob, or a `Completing` session with consumed
    /// blocks, is worse than the rejection.
    async fn verify_committed_checksum_or_abort(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        object_key: &str,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<Option<ObjectChecksum>> {
        match checksum_algorithm_to_compute(&stored.checksum_policy, provided_checksum.as_ref()) {
            Some(algorithm) => {
                // A transient read failure leaves the blob live and the session
                // Completing, so a retry adopts and re-verifies it.
                let computed = self.stream_object_checksum(object_key, algorithm).await?;
                match validate_complete_checksum_precomputed(
                    &stored.checksum_policy,
                    Some(computed),
                    provided_checksum,
                ) {
                    Ok(checksum) => Ok(checksum),
                    Err(err) => {
                        self.delete_object_if_exists(object_key)
                            .await
                            .map_err(|delete_err| {
                                tracing::error!(
                                    target: "pocopine.log",
                                    event_name = "pocopine.storage.azure_invalid_object_cleanup_failed",
                                    object_key = %object_key,
                                    checksum_error = %err,
                                    delete_error = %delete_err,
                                );
                                delete_err
                            })?;
                        stored.public.status = UploadSessionStatus::Aborted;
                        self.write_session(session, stored)
                            .await
                            .map_err(|write_err| {
                                tracing::error!(
                                    target: "pocopine.log",
                                    event_name = "pocopine.storage.azure_abort_persist_failed",
                                    object_key = %object_key,
                                    checksum_error = %err,
                                    write_error = %write_err,
                                );
                                write_err
                            })?;
                        // The session is now terminal (`Aborted`): no further part
                        // upload or abort will arrive, so release its concurrency
                        // pool here rather than leaking the entry. (A no-op for the
                        // sequential path, which never registers a semaphore.)
                        self.part_concurrency.forget(session);
                        Err(err)
                    }
                }
            }
            None => validate_complete_checksum_precomputed(
                &stored.checksum_policy,
                None,
                provided_checksum,
            ),
        }
    }

    // --- native block-blob by-number (server-mediated multipart) --------------

    /// Verify every block `0..count` for a by-number multipart upload is staged
    /// (uncommitted). A missing block means its part was never uploaded, so the
    /// upload is incomplete. Block sizes are not summed: the declared length is
    /// the authoritative total (each part's exact length is enforced at upload).
    async fn ensure_blocks_staged(
        &self,
        object_key: &str,
        session: &UploadSessionId,
        count: u64,
    ) -> StorageResult<()> {
        let response = self
            .container
            .blob_client(object_key)
            .block_blob_client()
            .get_block_list(BlockListType::Uncommitted, None)
            .await
            .map_err(|err| {
                if is_azure_not_found(&err) {
                    // No staged blocks at all — part 1 is missing.
                    StorageError::policy_rejected("upload is incomplete: missing multipart part 1")
                } else {
                    azure_error("get block list", err)
                }
            })?;
        let block_list = response
            .into_model()
            .map_err(|err| azure_error("decode block list", err))?;
        let staged: HashSet<Vec<u8>> = block_list
            .uncommitted_blocks
            .unwrap_or_default()
            .into_iter()
            .filter_map(|block| block.name)
            .collect();
        for index in 0..count {
            if !staged.contains(&block_id(session, index)) {
                return Err(StorageError::policy_rejected(format!(
                    "upload is incomplete: missing multipart part {}",
                    index + 1
                )));
            }
        }
        Ok(())
    }

    /// Assemble a by-number multipart upload (the [`StorageBackend::upload_part`]
    /// path). Blocks are staged by part number against the destination blob and
    /// the commit references a fixed `0..count` range derived from the declared
    /// size — so concurrent, out-of-order part uploads are race-free.
    async fn complete_block_by_number(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        provided_checksum: Option<ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        precheck_checksum(&stored.checksum_policy, provided_checksum.as_ref())?;
        let expected_size = stored.public.size.ok_or_else(|| {
            StorageError::policy_rejected(
                "multipart upload requires a declared length before completion",
            )
        })?;
        // Enforce the scope size limit (a direct backend caller may have declared
        // a size above `max_bytes`; `initiate` only checks the backend cap), as
        // the S3/GCS multipart completions do.
        ensure_size_limit(stored.max_bytes, stored.public.size, expected_size)?;
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        let block_size = block_size_for(stored.max_bytes);
        let count = if expected_size == 0 {
            0
        } else {
            expected_size.div_ceil(block_size)
        };

        // Idempotent re-complete: adopt our own already-committed blob, reject a
        // foreign one. (For the absent case the commit below is the authority.)
        if let Some(attrs) = self.get_object_attrs(&object_key).await? {
            if attrs.owner_session.as_deref() == Some(session.as_str()) {
                let written = AzureObjectWrite {
                    etag: attrs.etag,
                    version_id: attrs.version_id,
                };
                let checksum = self
                    .verify_committed_checksum_or_abort(
                        session,
                        &mut stored,
                        &object_key,
                        provided_checksum,
                    )
                    .await?;
                return self
                    .finalize_block(session, stored, expected_size, written, checksum)
                    .await;
            }
            // Foreign blob at the destination. Reopen first (mirroring the later
            // conflict paths) so a session a prior attempt left `Completing` is not
            // stuck — the owner can then abort/clean up rather than being wedged.
            self.reopen_if_object_absent(session, &mut stored, &object_key)
                .await;
            return Err(StorageError::policy_rejected(format!(
                "Azure blob key already exists: {object_key}"
            )));
        }

        // All referenced blocks must be staged, or the commit would silently drop
        // a missing one. (Empty uploads commit a zero-block list.) No commit has
        // happened yet, so on failure reopen a session that a prior attempt left
        // `Completing` (e.g. a crash before commit, then an uncommitted block was
        // GC'd) — otherwise it would be stuck, rejecting both part uploads and
        // abort. `reopen_if_object_absent` only reopens when the blob is truly
        // absent/foreign, so an already-committed object is never reopened.
        if count > 0
            && let Err(err) = self.ensure_blocks_staged(&object_key, session, count).await
        {
            self.reopen_if_object_absent(session, &mut stored, &object_key)
                .await;
            return Err(err);
        }

        // Fence: persist Completing before the commit so a concurrent abort is
        // refused and a crash leaves a recoverable (re-adoptable) session.
        if stored.public.status == UploadSessionStatus::Open
            || stored.completion_object_key.as_deref() != Some(object_key.as_str())
        {
            stored.public.status = UploadSessionStatus::Completing;
            stored.public.next_offset = Some(expected_size);
            stored.completion_object_key = Some(object_key.clone());
            self.write_session(session, &mut stored).await?;
        }

        let written = match self
            .commit_block_list(
                &object_key,
                count,
                stored.public.content_type.as_deref(),
                session,
                true,
            )
            .await
        {
            Ok(written) => written,
            Err(StorageError::Conflict { .. }) => {
                // A blob appeared at the key during completion; adopt only if ours.
                match self.get_object_attrs(&object_key).await {
                    Ok(Some(attrs)) if attrs.owner_session.as_deref() == Some(session.as_str()) => {
                        AzureObjectWrite {
                            etag: attrs.etag,
                            version_id: attrs.version_id,
                        }
                    }
                    Ok(_) => {
                        self.reopen_if_object_absent(session, &mut stored, &object_key)
                            .await;
                        return Err(StorageError::policy_rejected(format!(
                            "Azure blob key already exists: {object_key}"
                        )));
                    }
                    Err(err) => {
                        self.reopen_if_object_absent(session, &mut stored, &object_key)
                            .await;
                        return Err(err);
                    }
                }
            }
            Err(err) => {
                self.reopen_if_object_absent(session, &mut stored, &object_key)
                    .await;
                return Err(err);
            }
        };

        let checksum = self
            .verify_committed_checksum_or_abort(
                session,
                &mut stored,
                &object_key,
                provided_checksum,
            )
            .await?;
        self.finalize_block(session, stored, expected_size, written, checksum)
            .await
    }

    /// Finalize a by-number multipart session: record the object and persist.
    async fn finalize_block(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        total: u64,
        written: AzureObjectWrite,
        checksum: Option<ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        let object = self.object_ref(&stored, total, written, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(total);
        stored.object = Some(object.clone());
        self.write_session(session, &mut stored).await?;
        Ok(object)
    }
}

impl StorageBackend for AzureBlobStorageBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Sequential proxy (the coalescing-tail block path, the default) and
        // server-mediated native multipart (the by-number `upload_part` path) are
        // both assembled with `Put Block` + `Put Block List`.
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
            // By-number multipart needs the total length up front: blocks are
            // staged by part number and the commit references a fixed `0..N` block
            // range derived from the declared size.
            if strategy == UploadStrategy::Multipart && request.size.is_none() {
                return Err(StorageError::policy_rejected(
                    "multipart uploads require a declared length (size) at initiation",
                ));
            }
            let max_bytes = request.policy.max_bytes.min(self.max_proxy_upload_bytes);
            // For the by-number path, pin the block size and cap the block count;
            // the sequential path keeps the policy's chunk plan unchanged.
            let (plan, session_part_size) = if strategy == UploadStrategy::Multipart {
                let block_size = block_size_for(max_bytes);
                // min == preferred == max: every non-final part is exactly
                // `block_size` (the last is the remainder), matching `upload_part`.
                let plan = TransferPlan {
                    min_part_size: Some(block_size),
                    preferred_part_size: Some(block_size),
                    max_part_size: Some(block_size),
                    max_parts: Some(max_bytes.div_ceil(block_size).max(1) as u32),
                    max_concurrent_parts: request.policy.max_concurrent_parts.max(1),
                    resumable: true,
                };
                (plan, Some(block_size))
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
            // Native block sessions keep an authoritative offset in the session
            // metadata, so no provider round-trip is needed to report progress.
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
            self.append_block(&session, stored, offset, bytes).await
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
            // Part numbers are 1-based; the route enforces this, but the trait is
            // directly callable, so guard before the `number - 1` index below.
            if number == 0 {
                return Err(StorageError::policy_rejected(
                    "multipart part number must be at least 1",
                ));
            }
            let (object_key, block_size, expected_part_len, public) = {
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
                        "Azure backend part upload requires the multipart strategy",
                    ));
                }
                // Fixed block layout: parts `1..count` are exactly `block_size`,
                // the last is the remainder. Pinning each part's length makes the
                // committed blob's size exactly the declared total and caps the
                // block count.
                let block_size = block_size_for(stored.max_bytes);
                let declared = stored.public.size.ok_or_else(|| {
                    StorageError::policy_rejected("multipart upload is missing a declared length")
                })?;
                let count = declared.div_ceil(block_size).max(1);
                if u64::from(number) > count {
                    return Err(StorageError::policy_rejected(format!(
                        "part number {number} exceeds the maximum of {count} parts for this upload"
                    )));
                }
                let expected_part_len = if u64::from(number) == count {
                    declared - (count - 1) * block_size
                } else {
                    block_size
                };
                let object_key = self.layout.object_key(stored.storage_key.key.as_str());
                (
                    object_key,
                    block_size,
                    expected_part_len,
                    stored.public.clone(),
                )
            };

            // Enforce the advertised concurrency, then re-check the captured expiry
            // (the permit wait can outlast it).
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

            // Azure stages a whole block per part, so the bounded part is
            // collected (peak = one block per concurrent upload). Reject a part
            // whose actual length is not its fixed slot length so the committed
            // total is exactly the declared size.
            let bytes = body.collect_capped(block_size).await?;
            if bytes.len() as u64 != expected_part_len {
                return Err(StorageError::policy_rejected(format!(
                    "part {number} must be exactly {expected_part_len} bytes, got {}",
                    bytes.len()
                )));
            }

            // Re-acquire the lock and re-validate Open immediately before staging,
            // holding the lock across the stage. This fences against a concurrent
            // complete/abort (both take the lock): the block either stages before
            // they run or this re-read sees the closed session and stages nothing.
            // `stage_block` re-staging the same index overwrites, so a re-PUT is
            // idempotent.
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
            let _guard = lock.lock().await;
            let stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            ensure_open(&stored.public)?;
            if stored.public.strategy != UploadStrategy::Multipart {
                return Err(StorageError::unsupported(
                    "Azure backend part upload requires the multipart strategy",
                ));
            }
            self.stage_block(&object_key, &session, u64::from(number - 1), bytes)
                .await?;
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
                self.complete_block_by_number(&session, stored, request.checksum)
                    .await
            } else {
                self.complete_block(&session, stored, request.checksum)
                    .await
            };
            // Drop the concurrency pool only on success; a failed attempt leaves
            // the session resumable and must keep its semaphore.
            if result.is_ok() {
                self.part_concurrency.forget(&session);
            }
            result
        })
    }

    fn read_object<'a>(
        &'a self,
        _ctx: &'a StorageContext,
        scope: &'a str,
        key: SafeObjectKey,
        max_bytes: u64,
    ) -> StorageBoxFuture<'a, ObjectRead> {
        Box::pin(async move {
            let object_key = self.layout.object_key(key.as_str());
            let metadata =
                self.get_object_metadata(&object_key)
                    .await
                    .map_err(|err| match err {
                        StorageError::UnknownUploadSession { .. } => {
                            StorageError::unknown_object(key.to_string())
                        }
                        err => err,
                    })?;
            let declared_size = metadata.size;
            let metadata_etag = metadata.etag.clone();
            let metadata_content_type = metadata.content_type.clone();
            let size = declared_size
                .ok_or_else(|| StorageError::backend("Azure blob size is unavailable"))?;
            if size > max_bytes {
                return Err(StorageError::payload_too_large(max_bytes));
            }
            let mut options = BlobClientDownloadOptions::default();
            if let Some(etag) = &metadata.etag {
                options.if_match = Some(Etag::from(etag.clone()));
            }
            let response = self
                .container
                .blob_client(&object_key)
                .download(Some(options))
                .await
                .map_err(|err| {
                    if is_azure_not_found(&err) {
                        StorageError::unknown_object(key.to_string())
                    } else if is_azure_precondition_failed(&err) {
                        StorageError::conflict("Azure blob changed while reading")
                    } else {
                        azure_error("download completed blob", err)
                    }
                })?;
            let etag = response
                .properties
                .etag
                .map(|etag| etag.to_string())
                .or(metadata_etag);
            let content_type = response.properties.content_type.or(metadata_content_type);
            let stream = response.body.map(|chunk| {
                chunk.map_err(|err| std::io::Error::other(format!("Azure read blob body: {err}")))
            });
            Ok(ObjectRead {
                object: ObjectRef {
                    backend: self.name.to_string(),
                    scope: scope.to_string(),
                    key: key.to_string(),
                    version: None,
                    etag,
                    checksum: None,
                    content_type,
                    size,
                    visibility: ObjectVisibility::Private,
                    metadata: Default::default(),
                },
                body: ObjectBody::from_stream_with_limit(stream, max_bytes),
            })
        })
    }

    fn delete_completed_object<'a>(
        &'a self,
        _ctx: &'a StorageContext,
        key: SafeObjectKey,
    ) -> StorageBoxFuture<'a, ()> {
        Box::pin(async move {
            self.delete_object_if_exists(&self.layout.object_key(key.as_str()))
                .await
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
                    // Stop concurrent part uploads: queued and future ones fail
                    // immediately instead of staging blocks after the abort.
                    // (Uncommitted blocks are never committed, so Azure
                    // garbage-collects them.)
                    if stored.public.strategy == UploadStrategy::Multipart {
                        let max = stored.public.plan.max_concurrent_parts.max(1);
                        self.part_concurrency
                            .semaphore(&session, max as usize)
                            .close();
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
