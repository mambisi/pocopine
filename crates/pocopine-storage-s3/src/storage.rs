use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client;
use aws_smithy_types::body::SdkBody;
use bytes::Bytes;
use futures_util::TryStreamExt as _;
use http_body::Frame;
use http_body_util::StreamBody;
use pocopine_storage::backend_common::{
    checked_new_offset, ensure_open, ensure_owner, ensure_size_limit,
    ensure_upload_length_can_be_set, expires_at, refresh_expired, select_upload_mode,
    UploadConcurrencyRegistry, UploadSessionLockCleanup, UploadSessionLockRegistry,
};
use pocopine_storage::checksum::{
    checksum_algorithm_to_compute, ensure_supported_checksum_policy, precheck_checksum,
    validate_complete_checksum, validate_complete_checksum_precomputed, StreamingChecksum,
};
use pocopine_storage::{
    BackendCapabilities, ChecksumAlgorithm, ChecksumPolicy, CompleteUpload, InitiateUpload,
    ObjectChecksum, ObjectRef, StorageBackend, StorageBoxFuture, StorageContext, StorageError,
    StorageResult, TransferPlan, UploadBody, UploadSession, UploadSessionId, UploadSessionStatus,
    UploadStrategy, UploadedPartStatus, UploadedPartView,
};
use time::OffsetDateTime;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::layout::{S3KeyLayout, DEFAULT_INTERNAL_PREFIX};
use crate::state::{NativeUploadState, S3CompletedPart, S3MultipartState, StoredUploadSession};
use crate::util::{
    ensure_completable, is_get_object_not_found, is_head_object_not_found, is_no_such_upload,
    is_precondition_failed, is_put_precondition_failed, normalize_etag, s3_error,
};

const DEFAULT_BACKEND_NAME: &str = "s3";
const DEFAULT_MAX_PROXY_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// S3 minimum part size. Every multipart part except the last must be at least
/// this large, so sequential `PATCH` chunks smaller than this are coalesced in a
/// bounded tail buffer until they reach it.
const S3_MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

/// S3 maximum size of a single part (5 GiB).
const S3_MAX_PART_SIZE: u64 = 5 * 1024 * 1024 * 1024;

/// S3 maximum number of parts in one multipart upload.
const S3_MAX_PARTS: u64 = 10_000;

/// Read granularity for the streaming checksum read-back.
const CHECKSUM_READ_CHUNK: usize = 256 * 1024;

/// Object user-metadata key stamped at `CreateMultipartUpload` with the upload
/// session id. It proves ownership when reconciling an existing key after S3
/// reports `NoSuchUpload` (which can mean either "we already completed" or "the
/// upload was aborted/expired"). Only an object carrying our session id is
/// adopted on recovery.
const SESSION_OWNER_META_KEY: &str = "pocopine-upload-session";

/// Part size to coalesce to for a session whose maximum object size is
/// `max_bytes`.
///
/// Scales up from the 5 MiB floor (rounded to a whole MiB) so the worst-case
/// object stays within S3's 10,000-part limit, and never exceeds the 5 GiB
/// per-part ceiling. For the default 64 MiB cap this is exactly 5 MiB.
fn part_size_for(max_bytes: u64) -> u64 {
    const MIB: u64 = 1024 * 1024;
    let by_part_count = max_bytes.div_ceil(S3_MAX_PARTS).div_ceil(MIB) * MIB;
    by_part_count.clamp(S3_MIN_PART_SIZE, S3_MAX_PART_SIZE)
}

/// Interceptor that forces SigV4 to sign a streamed `UploadPart` with an
/// unsigned payload. The part body is produced lazily over a channel, so its
/// hash cannot be computed before signing; without this the SDK pre-signs a
/// hash that never matches the streamed bytes (`XAmzContentSHA256Mismatch`).
#[derive(Debug, Clone)]
struct UnsignedPayloadInterceptor;

impl aws_smithy_runtime_api::client::interceptors::Intercept for UnsignedPayloadInterceptor {
    fn name(&self) -> &'static str {
        "PocopineUnsignedPayload"
    }

    fn modify_before_signing(
        &self,
        _context: &mut aws_smithy_runtime_api::client::interceptors::context::BeforeTransmitInterceptorContextMut<'_>,
        _runtime_components: &aws_smithy_runtime_api::client::runtime_components::RuntimeComponents,
        cfg: &mut aws_smithy_types::config_bag::ConfigBag,
    ) -> Result<(), aws_smithy_runtime_api::box_error::BoxError> {
        cfg.interceptor_state()
            .store_put(aws_runtime::auth::PayloadSigningOverride::UnsignedPayload);
        Ok(())
    }
}

/// Adapt a streamed part body into an S3 `ByteStream` without buffering it: the
/// bytes flow request → `UploadPart` one chunk at a time, so peak memory stays
/// O(1) per part regardless of part size.
///
/// The inbound axum body stream is `Send` but not `Sync`, while the S3
/// `ByteStream` body requires `Send + Sync`. A bounded channel bridges the two:
/// a pump task forwards frames into the channel (blocking on backpressure when
/// the channel is full, so only a few frames are ever in flight), and the
/// `Sync` receiver side feeds the `ByteStream`.
fn upload_body_to_byte_stream(body: UploadBody) -> ByteStream {
    use futures_util::StreamExt as _;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        let mut stream = body.into_byte_stream();
        while let Some(item) = stream.next().await {
            if tx.send(item).await.is_err() {
                // Receiver (the ByteStream) was dropped — stop pumping.
                break;
            }
        }
    });
    let frames = ReceiverStream::new(rx).map_ok(Frame::data);
    ByteStream::new(SdkBody::from_body_1_x(StreamBody::new(frames)))
}

/// Storage backend backed by an S3-compatible object store.
///
/// Uploads use S3 native multipart: each sequential `PATCH` is appended to a
/// bounded tail buffer and, once the tail reaches [`S3_MIN_PART_SIZE`], flushed
/// to a provider `UploadPart`. The final object is assembled by
/// `CompleteMultipartUpload`. Peak memory is one part regardless of object size,
/// and every byte is sent to S3 exactly once (no per-`PATCH` full-object
/// rewrite). Sessions created by an older build deserialize as
/// [`NativeUploadState::LegacyProxy`] and keep using the staged-object rewrite
/// path until they drain.
#[derive(Clone)]
pub struct S3StorageBackend {
    name: &'static str,
    client: Client,
    layout: S3KeyLayout,
    max_proxy_upload_bytes: u64,
    session_locks: UploadSessionLockRegistry,
    part_concurrency: UploadConcurrencyRegistry,
}

struct HeadObjectInfo {
    etag: Option<String>,
    /// Value of the [`SESSION_OWNER_META_KEY`] user-metadata entry, if present.
    owner_session: Option<String>,
    /// Object size in bytes, used to adopt an already-completed object on retry
    /// without re-listing the (consumed) multipart upload.
    content_length: u64,
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

    /// Override the maximum upload size accepted by this backend.
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
        bytes: Bytes,
        content_type: Option<&str>,
    ) -> StorageResult<Option<String>> {
        match self.get_object_bytes_with_etag(key).await {
            Ok((existing, etag)) => {
                if existing.as_slice() == bytes.as_ref() {
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
            .body(ByteStream::from(bytes));
        if let Some(content_type) = content_type {
            request = request.content_type(content_type);
        }
        match request.send().await {
            Ok(output) => Ok(output.e_tag.map(normalize_etag)),
            Err(err) if is_put_precondition_failed(&err) => Err(StorageError::policy_rejected(
                format!("S3 object key already exists: {key}"),
            )),
            Err(err) => Err(s3_error("put completed object", err)),
        }
    }

    /// Write a zero-byte completed object for a multipart session, guarding
    /// ownership by HEAD rather than the byte-match fast path of
    /// [`Self::put_completed_object`] (which would wrongly adopt a *foreign*
    /// empty object that happens to share the same — empty — bytes). Adopts our
    /// own already-written object idempotently, rejects a foreign one, and
    /// stamps the owner marker so a retry after a crash can adopt it.
    async fn put_empty_owned_object(
        &self,
        object_key: &str,
        session: &UploadSessionId,
        content_type: Option<&str>,
    ) -> StorageResult<Option<String>> {
        match self.head_object(object_key).await? {
            Some(info) if info.owner_session.as_deref() == Some(session.as_str()) => {
                return Ok(info.etag);
            }
            Some(_) => {
                return Err(StorageError::policy_rejected(format!(
                    "S3 object key already exists: {object_key}"
                )));
            }
            None => {}
        }
        let mut request = self
            .client
            .put_object()
            .bucket(&self.layout.bucket)
            .key(object_key)
            .if_none_match("*")
            .metadata(SESSION_OWNER_META_KEY, session.as_str())
            .body(ByteStream::from(Bytes::new()));
        if let Some(content_type) = content_type {
            request = request.content_type(content_type);
        }
        match request.send().await {
            Ok(output) => Ok(output.e_tag.map(normalize_etag)),
            Err(err) if is_put_precondition_failed(&err) => {
                // Lost the create race. The session lock is per-process, so a
                // concurrent process completing this same empty upload may have
                // won: re-HEAD and adopt idempotently when the object carries our
                // session marker; otherwise it is a genuine foreign collision.
                match self.head_object(object_key).await? {
                    Some(info) if info.owner_session.as_deref() == Some(session.as_str()) => {
                        Ok(info.etag)
                    }
                    _ => Err(StorageError::policy_rejected(format!(
                        "S3 object key already exists: {object_key}"
                    ))),
                }
            }
            Err(err) => Err(s3_error("put empty object", err)),
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

    async fn head_object(&self, key: &str) -> StorageResult<Option<HeadObjectInfo>> {
        match self
            .client
            .head_object()
            .bucket(&self.layout.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(output) => {
                let owner_session = output
                    .metadata()
                    .and_then(|metadata| metadata.get(SESSION_OWNER_META_KEY).cloned());
                Ok(Some(HeadObjectInfo {
                    etag: output.e_tag.map(normalize_etag),
                    owner_session,
                    content_length: output.content_length.unwrap_or(0).max(0) as u64,
                }))
            }
            Err(err) if is_head_object_not_found(&err) => Ok(None),
            Err(err) => Err(s3_error("head object", err)),
        }
    }

    // --- native multipart helpers --------------------------------------------

    async fn create_multipart(
        &self,
        object_key: &str,
        content_type: Option<&str>,
        session: &UploadSessionId,
    ) -> StorageResult<String> {
        let mut request = self
            .client
            .create_multipart_upload()
            .bucket(&self.layout.bucket)
            .key(object_key)
            // Stamp ownership so the final object can be attributed to this
            // session when reconciling `NoSuchUpload` on a retry.
            .metadata(SESSION_OWNER_META_KEY, session.as_str());
        if let Some(content_type) = content_type {
            request = request.content_type(content_type);
        }
        let output = request
            .send()
            .await
            .map_err(|err| s3_error("create multipart upload", err))?;
        output.upload_id().map(str::to_string).ok_or_else(|| {
            StorageError::backend("S3 create multipart upload returned no upload id".to_string())
        })
    }

    async fn upload_part(
        &self,
        object_key: &str,
        upload_id: &str,
        part_number: i32,
        body: Bytes,
    ) -> StorageResult<String> {
        let output = self
            .client
            .upload_part()
            .bucket(&self.layout.bucket)
            .key(object_key)
            .upload_id(upload_id)
            .part_number(part_number)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|err| s3_error("upload part", err))?;
        // Keep the ETag verbatim (quotes included) so CompleteMultipartUpload
        // matches what S3 returned for the part.
        output
            .e_tag()
            .map(str::to_string)
            .ok_or_else(|| StorageError::backend("S3 upload part returned no etag".to_string()))
    }

    /// Assemble the final object from its parts.
    ///
    /// Uses `If-None-Match: *` so completion atomically refuses to overwrite an
    /// existing destination key (no TOCTOU window) on real S3. Failure modes:
    ///   - `412 PreconditionFailed`: the key exists but *our* multipart upload was
    ///     not consumed, so the object was created by someone else — always
    ///     reject, never adopt it.
    ///   - `NoSuchUpload`: the upload id is gone. This is ambiguous — either a
    ///     prior completion consumed it (object is ours) or it was aborted/expired
    ///     (any object at the key is foreign). We disambiguate via the
    ///     [`SESSION_OWNER_META_KEY`] marker stamped at create time: adopt the key
    ///     only when it carries this session's id, otherwise the upload is gone.
    async fn complete_multipart(
        &self,
        object_key: &str,
        upload_id: &str,
        parts: &[S3CompletedPart],
        session: &UploadSessionId,
    ) -> StorageResult<Option<String>> {
        let completed_parts: Vec<CompletedPart> = parts
            .iter()
            .map(|part| {
                CompletedPart::builder()
                    .part_number(part.number)
                    .e_tag(part.etag.clone())
                    .build()
            })
            .collect();
        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();
        match self
            .client
            .complete_multipart_upload()
            .bucket(&self.layout.bucket)
            .key(object_key)
            .upload_id(upload_id)
            .if_none_match("*")
            .multipart_upload(completed)
            .send()
            .await
        {
            Ok(output) => Ok(output.e_tag().map(|etag| normalize_etag(etag.to_string()))),
            Err(err) if is_precondition_failed(&err) => {
                // A foreign object won the race; our upload is still active, so
                // abort it to avoid leaking parts before reporting the collision.
                let _ = self.abort_multipart(object_key, upload_id).await;
                Err(StorageError::policy_rejected(format!(
                    "S3 object key already exists: {object_key}"
                )))
            }
            Err(err) if is_no_such_upload(&err) => {
                match self.head_object(object_key).await? {
                    // Adopt the existing object only if it was stamped by this
                    // session; a foreign object (or none) means this upload was
                    // aborted/expired and can no longer be completed.
                    Some(info) if info.owner_session.as_deref() == Some(session.as_str()) => {
                        Ok(info.etag)
                    }
                    _ => Err(StorageError::UploadClosed {
                        session: session.to_string(),
                    }),
                }
            }
            Err(err) => Err(s3_error("complete multipart upload", err)),
        }
    }

    async fn abort_multipart(&self, object_key: &str, upload_id: &str) -> StorageResult<()> {
        match self
            .client
            .abort_multipart_upload()
            .bucket(&self.layout.bucket)
            .key(object_key)
            .upload_id(upload_id)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(err) if is_no_such_upload(&err) => Ok(()),
            Err(err) => Err(s3_error("abort multipart upload", err)),
        }
    }

    /// Stream one numbered part straight to S3 without buffering it.
    ///
    /// `content_length` is required: S3 `UploadPart` must know the part size, and
    /// the server reads it from the request `Content-Length` rather than draining
    /// the body to measure it. The ETag is recorded by S3 itself, so the
    /// by-number completion path lists parts from the provider rather than
    /// persisting per-part receipts in the session (which would race across
    /// concurrent parts).
    async fn upload_part_streaming(
        &self,
        object_key: &str,
        upload_id: &str,
        part_number: i32,
        body: UploadBody,
        content_length: u64,
    ) -> StorageResult<()> {
        self.client
            .upload_part()
            .bucket(&self.layout.bucket)
            .key(object_key)
            .upload_id(upload_id)
            .part_number(part_number)
            .content_length(content_length as i64)
            .body(upload_body_to_byte_stream(body))
            .customize()
            // The default `WhenSupported` checksum would try to read the
            // non-replayable streaming body to compute a CRC; only checksum when
            // the operation actually requires it.
            .config_override(
                aws_sdk_s3::config::Builder::default().request_checksum_calculation(
                    aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired,
                ),
            )
            .interceptor(UnsignedPayloadInterceptor)
            .send()
            .await
            .map_err(|err| s3_error("upload part", err))?;
        Ok(())
    }

    /// List the parts S3 has recorded for an in-progress multipart upload, in
    /// ascending part-number order. Paginates across S3's 1,000-parts-per-page
    /// limit so the full set (up to [`S3_MAX_PARTS`]) is returned.
    async fn list_parts(
        &self,
        object_key: &str,
        upload_id: &str,
    ) -> StorageResult<Vec<S3CompletedPart>> {
        let mut parts = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_parts()
                .bucket(&self.layout.bucket)
                .key(object_key)
                .upload_id(upload_id);
            if let Some(marker) = &marker {
                request = request.part_number_marker(marker);
            }
            let output = request
                .send()
                .await
                .map_err(|err| s3_error("list parts", err))?;
            for part in output.parts() {
                let number = part.part_number().ok_or_else(|| {
                    StorageError::backend("S3 list parts returned a part without a number")
                })?;
                let etag = part.e_tag().map(str::to_string).ok_or_else(|| {
                    StorageError::backend("S3 list parts returned a part without an etag")
                })?;
                let size = part.size().unwrap_or(0).max(0) as u64;
                parts.push(S3CompletedPart { number, etag, size });
            }
            if output.is_truncated() == Some(true) {
                marker = output.next_part_number_marker().map(str::to_string);
                if marker.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        parts.sort_by_key(|part| part.number);
        Ok(parts)
    }

    /// Resume helper: the multipart parts already committed for a by-number
    /// upload, listed from S3 (the session tracks no per-part receipts).
    ///
    /// This is deliberately a standalone method rather than part of
    /// [`StorageBackend::inspect_upload`]: `inspect_upload` is on the internal
    /// routing/authorization path that every part `PUT` traverses, and listing
    /// there would make an n-part upload O(n²). A client that wants to skip
    /// already-uploaded parts on resume calls this explicitly.
    pub async fn list_committed_parts(
        &self,
        ctx: &StorageContext,
        session: UploadSessionId,
    ) -> StorageResult<Vec<UploadedPartView>> {
        let lock = self.session_locks.lock(&session);
        let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
        let _guard = lock.lock().await;
        let stored = self.read_session(&session).await?;
        ensure_owner(&ctx.actor, &stored.owner)?;
        let Some(upload_id) = stored
            .native
            .multipart()
            .and_then(|state| state.upload_id.clone())
        else {
            return Ok(Vec::new());
        };
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        let parts = self.list_parts(&object_key, &upload_id).await?;
        Ok(parts
            .iter()
            .map(|part| UploadedPartView {
                number: part.number.max(0) as u32,
                size: part.size,
                status: UploadedPartStatus::Uploaded,
                checksum: None,
            })
            .collect())
    }

    /// Verify the assembled object against the checksum policy by streaming it
    /// back (only when a checksum is actually required/supplied — the default
    /// `None` policy never reads the object). On mismatch the live object is
    /// deleted so a bad upload is never observable; if that delete fails it is
    /// surfaced, since a checksum-invalid live object is worse than the error.
    async fn verify_object_checksum(
        &self,
        object_key: &str,
        policy: &ChecksumPolicy,
        requested: Option<ObjectChecksum>,
    ) -> StorageResult<Option<ObjectChecksum>> {
        match checksum_algorithm_to_compute(policy, requested.as_ref()) {
            Some(algorithm) => {
                let computed = self.stream_object_checksum(object_key, algorithm).await?;
                match validate_complete_checksum_precomputed(policy, Some(computed), requested) {
                    Ok(checksum) => Ok(checksum),
                    Err(err) => {
                        self.delete_object(object_key).await.map_err(|delete_err| {
                            tracing::error!(
                                target: "pocopine.log",
                                event_name = "pocopine.storage.s3_invalid_object_cleanup_failed",
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
            None => validate_complete_checksum_precomputed(policy, None, requested),
        }
    }

    /// Stream the stored object through `algorithm` without buffering it.
    async fn stream_object_checksum(
        &self,
        key: &str,
        algorithm: ChecksumAlgorithm,
    ) -> StorageResult<ObjectChecksum> {
        use tokio::io::AsyncReadExt as _;
        let output = self
            .client
            .get_object()
            .bucket(&self.layout.bucket)
            .key(key)
            .send()
            .await
            .map_err(|err| s3_error("get object for checksum", err))?;
        let mut reader = output.body.into_async_read();
        let mut hasher = StreamingChecksum::new(algorithm);
        let mut buffer = vec![0u8; CHECKSUM_READ_CHUNK];
        loop {
            let read = reader
                .read(&mut buffer)
                .await
                .map_err(|err| s3_error("read object body for checksum", err))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hasher.finalize())
    }

    /// Create the provider multipart upload on first use and persist its id.
    async fn ensure_upload_id(
        &self,
        session: &UploadSessionId,
        stored: &mut StoredUploadSession,
        object_key: &str,
    ) -> StorageResult<String> {
        if let Some(state) = stored.native.multipart() {
            if let Some(upload_id) = &state.upload_id {
                return Ok(upload_id.clone());
            }
        }
        let upload_id = self
            .create_multipart(object_key, stored.public.content_type.as_deref(), session)
            .await?;
        if let Some(state) = stored.native.multipart_mut() {
            state.upload_id = Some(upload_id.clone());
        }
        self.write_session(session, stored).await?;
        Ok(upload_id)
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

    // --- native multipart upload flow ----------------------------------------

    async fn append_multipart(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        offset: u64,
        bytes: Bytes,
    ) -> StorageResult<UploadSession> {
        // Copy the buffered tail to prepend to the incoming chunk. The copy
        // (bounded by one part) leaves the tail durable in `stored` so the lazy
        // `ensure_upload_id` write below still persists it; the tail is only
        // replaced by the remainder in the single atomic write at the end.
        let (flushed, tail) = {
            let state = stored
                .native
                .multipart()
                .expect("append_multipart called for a non-multipart session");
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

        let object_key = self.layout.object_key(stored.storage_key.key.as_str());

        // Combine the buffered tail (bounded by one part) with the incoming
        // chunk. When the tail is empty we slice the incoming `Bytes` directly,
        // avoiding any copy.
        let combined: Bytes = if tail.is_empty() {
            bytes
        } else {
            let mut combined = tail;
            combined.extend_from_slice(&bytes);
            Bytes::from(combined)
        };

        // Flush every full-size part; the remainder becomes the new tail. Parts
        // are uploaded to S3 first, then the resulting state (parts + remainder
        // tail) is persisted in a single atomic write at the end. A crash before
        // that write leaves the durable metadata unchanged, so the client simply
        // re-sends from the old offset and we recompute the same parts (uploading
        // a part is idempotent — the same part number overwrites).
        //
        // `part_size` is derived from the (fixed) session max so it is stable
        // across appends and resumes, and large uploads stay under the part cap.
        let part_size = part_size_for(stored.max_bytes) as usize;
        let mut pos = 0usize;
        while combined.len() - pos >= part_size {
            let upload_id = self
                .ensure_upload_id(session, &mut stored, &object_key)
                .await?;
            let number = stored
                .native
                .multipart()
                .expect("multipart state")
                .next_part_number;
            let part = combined.slice(pos..pos + part_size);
            let etag = self
                .upload_part(&object_key, &upload_id, number, part)
                .await?;
            if let Some(state) = stored.native.multipart_mut() {
                state.parts.push(S3CompletedPart {
                    number,
                    etag,
                    size: part_size as u64,
                });
                state.next_part_number = number + 1;
            }
            pos += part_size;
        }

        if let Some(state) = stored.native.multipart_mut() {
            state.pending_tail = combined.slice(pos..).to_vec();
        }
        stored.public.next_offset = Some(new_offset);
        self.write_session(session, &stored).await?;
        Ok(stored.public)
    }

    async fn complete_multipart_upload_flow(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        request: CompleteUpload,
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
        // Reject a missing-required or disallowed-algorithm checksum up front, so
        // a config error never assembles (and then deletes) the object.
        precheck_checksum(&stored.checksum_policy, request.checksum.as_ref())?;

        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        let has_upload_id = stored
            .native
            .multipart()
            .expect("multipart state")
            .upload_id
            .is_some();

        let (etag, checksum) = if !has_upload_id {
            // Single direct-PUT path: the whole object fits in the buffered tail
            // (or is empty). It is atomic — `put_completed_object` refuses to
            // overwrite an existing key — and the checksum is validated before any
            // state change, so a rejected checksum leaves the session open and
            // resumable rather than stranded in `Completing`.
            let tail = stored
                .native
                .multipart()
                .expect("multipart state")
                .pending_tail
                .clone();
            let checksum =
                validate_complete_checksum(&stored.checksum_policy, &tail, request.checksum)?;
            let etag = self
                .put_completed_object(
                    &object_key,
                    Bytes::from(tail),
                    stored.public.content_type.as_deref(),
                )
                .await?;
            (etag, checksum)
        } else {
            // Multipart path.
            // HEAD on every attempt (not just the first). This is the only
            // overwrite guard that works on stores which ignore `If-None-Match`
            // on `CompleteMultipartUpload`, and it distinguishes our own
            // already-finalized object (adopt idempotently) from a foreign object
            // that won the key (reject). The `If-None-Match: *` on completion is
            // an additional atomic guard for the HEAD→complete window on real S3.
            let existing_etag = match self.head_object(&object_key).await? {
                Some(info) if info.owner_session.as_deref() == Some(session.as_str()) => {
                    Some(info.etag)
                }
                Some(_) => {
                    // Foreign object at the destination: abort our orphaned upload
                    // (best effort) and reject rather than overwrite it.
                    let _ = self.abort_multipart_session(&stored).await;
                    return Err(StorageError::policy_rejected(format!(
                        "S3 object key already exists: {object_key}"
                    )));
                }
                None => None,
            };

            let etag = if let Some(etag) = existing_etag {
                // A prior attempt already finalized this object; reuse it.
                etag
            } else {
                if stored.public.status == UploadSessionStatus::Open
                    || stored.public.next_offset != Some(total)
                {
                    stored.public.status = UploadSessionStatus::Completing;
                    stored.public.next_offset = Some(total);
                    self.write_session(session, &stored).await?;
                }

                // Flush the final remainder as the last part (exempt from the
                // minimum-size rule), then assemble the object.
                let tail = stored
                    .native
                    .multipart()
                    .expect("multipart state")
                    .pending_tail
                    .clone();
                if !tail.is_empty() {
                    let tail_len = tail.len() as u64;
                    let upload_id = stored
                        .native
                        .multipart()
                        .and_then(|state| state.upload_id.clone())
                        .expect("multipart upload id");
                    let number = stored
                        .native
                        .multipart()
                        .expect("multipart state")
                        .next_part_number;
                    let etag = self
                        .upload_part(&object_key, &upload_id, number, Bytes::from(tail))
                        .await?;
                    if let Some(state) = stored.native.multipart_mut() {
                        state.parts.push(S3CompletedPart {
                            number,
                            etag,
                            size: tail_len,
                        });
                        state.next_part_number = number + 1;
                        state.pending_tail = Vec::new();
                    }
                    self.write_session(session, &stored).await?;
                }
                let (upload_id, parts) = {
                    let state = stored.native.multipart().expect("multipart state");
                    (
                        state.upload_id.clone().expect("multipart upload id"),
                        state.parts.clone(),
                    )
                };
                self.complete_multipart(&object_key, &upload_id, &parts, session)
                    .await?
            };

            // Validate the checksum by streaming the finished object (only when a
            // checksum was actually supplied; the default path never hashes).
            //
            // NOTE: multipart parts cannot be read before `CompleteMultipartUpload`,
            // so verification necessarily happens after the object is assembled. A
            // mismatch therefore deletes the (already-finalized) object and the
            // session cannot be retried — the client must re-initiate. Phase 0
            // accepts this; a later phase computes the checksum incrementally as
            // parts stream so it can be verified before completion.
            let checksum = self
                .verify_object_checksum(&object_key, &stored.checksum_policy, request.checksum)
                .await?;
            (etag, checksum)
        };

        self.finalize_completed(session, stored, total, etag, checksum)
            .await
    }

    /// Mark a session complete: record the resulting object, clear any buffered
    /// tail, and persist. Native sessions buffer the tail inline in the metadata,
    /// so there is no separate staged object to delete.
    async fn finalize_completed(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        total: u64,
        etag: Option<String>,
        checksum: Option<ObjectChecksum>,
    ) -> StorageResult<ObjectRef> {
        let object = self.object_ref(&stored, total, etag, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(total);
        stored.object = Some(object.clone());
        if let Some(state) = stored.native.multipart_mut() {
            state.pending_tail = Vec::new();
        }
        self.write_session(session, &stored).await?;
        Ok(object)
    }

    /// Assemble a server-mediated multipart upload whose parts arrived by number
    /// (the [`StorageBackend::upload_part`] path). Part receipts live on S3, not
    /// in the session, so they are listed from the provider here rather than read
    /// from local state — which is what makes concurrent, out-of-order part
    /// uploads race-free (no per-part session write to clobber).
    async fn complete_multipart_by_number_flow(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        request: CompleteUpload,
    ) -> StorageResult<ObjectRef> {
        // Reject a missing-required or disallowed-algorithm checksum up front.
        precheck_checksum(&stored.checksum_policy, request.checksum.as_ref())?;
        // A by-number upload must declare its length before completing. Parts
        // arrive concurrently and `upload_part` releases the session lock before
        // streaming, so without a declared total a `complete` racing an in-flight
        // part would treat the currently-listed parts as the whole object and
        // publish a truncated result. With a declared size, the `total != size`
        // check below rejects a premature completion instead.
        let expected_size = stored.public.size.ok_or_else(|| {
            StorageError::policy_rejected(
                "multipart upload requires a declared length before completion",
            )
        })?;
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        let upload_id = stored
            .native
            .multipart()
            .and_then(|state| state.upload_id.clone());

        // No part was ever uploaded: the object is empty. Handle it exactly like
        // the single direct-PUT path so a zero-byte upload still produces an
        // object (CompleteMultipartUpload rejects zero parts, so it is avoided).
        let Some(upload_id) = upload_id else {
            if expected_size != 0 {
                return Err(StorageError::policy_rejected(format!(
                    "upload is incomplete: expected {expected_size} bytes, got 0"
                )));
            }
            let checksum =
                validate_complete_checksum(&stored.checksum_policy, &[], request.checksum)?;
            let etag = self
                .put_empty_owned_object(&object_key, session, stored.public.content_type.as_deref())
                .await?;
            return self
                .finalize_completed(session, stored, 0, etag, checksum)
                .await;
        };

        // Idempotent re-complete: a prior attempt may already have assembled our
        // object. HEAD distinguishes our own finished object (adopt) from a
        // foreign object that won the key (reject), and works even on stores that
        // ignore `If-None-Match` on CompleteMultipartUpload.
        match self.head_object(&object_key).await? {
            Some(info) if info.owner_session.as_deref() == Some(session.as_str()) => {
                // Already assembled by a prior attempt. The multipart upload id is
                // consumed (ListParts would report `NoSuchUpload`), so adopt the
                // object using its own size rather than re-listing parts.
                let total = info.content_length;
                let checksum = self
                    .verify_object_checksum(&object_key, &stored.checksum_policy, request.checksum)
                    .await?;
                return self
                    .finalize_completed(session, stored, total, info.etag, checksum)
                    .await;
            }
            Some(_) => {
                let _ = self.abort_multipart(&object_key, &upload_id).await;
                return Err(StorageError::policy_rejected(format!(
                    "S3 object key already exists: {object_key}"
                )));
            }
            None => {}
        }

        let parts = self.list_parts(&object_key, &upload_id).await?;
        // Reject gaps: `CompleteMultipartUpload` would otherwise concatenate
        // whatever parts exist in number order, silently dropping a missing one
        // (e.g. parts 1 and 3 with no 2) and publishing an object with corrupted
        // offsets. `parts` is sorted ascending, so they must be exactly 1..=len.
        for (index, part) in parts.iter().enumerate() {
            let expected = index as i32 + 1;
            if part.number != expected {
                return Err(StorageError::policy_rejected(format!(
                    "upload is incomplete: missing multipart part {expected}"
                )));
            }
        }
        // S3 requires every part except the last to be at least 5 MiB; otherwise
        // `CompleteMultipartUpload` fails with `EntityTooSmall`. Validate before
        // leaving `Open` (and before the size check, so this specific cause is
        // reported) so a client that ignored `plan.min_part_size` can re-PUT the
        // offending part — a `Completing` session refuses further parts.
        if let Some((_last, leading)) = parts.split_last() {
            for part in leading {
                if part.size < S3_MIN_PART_SIZE {
                    return Err(StorageError::policy_rejected(format!(
                        "multipart part {} is {} bytes, below the {} byte minimum for a non-final part",
                        part.number, part.size, S3_MIN_PART_SIZE
                    )));
                }
            }
        }

        let total: u64 = parts.iter().map(|part| part.size).sum();
        if total != expected_size {
            // Rejects a `complete` that raced an in-flight part (S3 lists only
            // 1..N so far) as well as a genuinely short upload.
            return Err(StorageError::policy_rejected(format!(
                "upload is incomplete: expected {expected_size} bytes, got {total}"
            )));
        }
        ensure_size_limit(stored.max_bytes, stored.public.size, total)?;

        let empty_mpu = parts.is_empty();
        let etag = if empty_mpu {
            // The upload was created but no part committed (every part failed), so
            // the object is empty. Write the empty object (HEAD-guarded: rejects a
            // foreign key, stamps ownership) but DON'T abort the MPU yet — the
            // upload id is the only retry handle until the empty object is durably
            // finalized below, so aborting first would strand the session if the
            // PUT or checksum failed.
            self.put_empty_owned_object(&object_key, session, stored.public.content_type.as_deref())
                .await?
        } else {
            if stored.public.status == UploadSessionStatus::Open
                || stored.public.next_offset != Some(total)
            {
                stored.public.status = UploadSessionStatus::Completing;
                stored.public.next_offset = Some(total);
                self.write_session(session, &stored).await?;
            }
            self.complete_multipart(&object_key, &upload_id, &parts, session)
                .await?
        };

        let checksum = self
            .verify_object_checksum(&object_key, &stored.checksum_policy, request.checksum)
            .await?;
        let object = self
            .finalize_completed(session, stored, total, etag, checksum)
            .await?;
        if empty_mpu {
            // The empty object is durable and the session is finalized; clean up
            // the now-orphaned multipart upload best-effort (a leftover is swept
            // by stale-upload cleanup and never re-adopted, since completion is
            // idempotent via the finalized object).
            let _ = self.abort_multipart(&object_key, &upload_id).await;
        }
        Ok(object)
    }

    async fn abort_multipart_session(&self, stored: &StoredUploadSession) -> StorageResult<()> {
        if let Some(state) = stored.native.multipart() {
            if let Some(upload_id) = &state.upload_id {
                let object_key = self.layout.object_key(stored.storage_key.key.as_str());
                self.abort_multipart(&object_key, upload_id).await?;
            }
        }
        Ok(())
    }

    // --- legacy staged-object rewrite flow (pre-multipart sessions) -----------

    async fn append_legacy(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        offset: u64,
        bytes: Bytes,
    ) -> StorageResult<UploadSession> {
        let mut expected = stored.public.next_offset.unwrap_or(0);
        let mut staged = self.reconcile_staged_bytes(session, expected).await?;
        let staged_len = staged.len() as u64;
        if staged_len > expected {
            expected = staged_len;
            stored.public.next_offset = Some(staged_len);
            self.write_session(session, &stored).await?;
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
        self.put_staged_bytes(session, staged).await?;
        stored.public.next_offset = Some(new_offset);
        self.write_session(session, &stored).await?;
        Ok(stored.public)
    }

    async fn complete_legacy(
        &self,
        session: &UploadSessionId,
        mut stored: StoredUploadSession,
        request: CompleteUpload,
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
            validate_complete_checksum(&stored.checksum_policy, &staged, request.checksum)?;
        let object_key = self.layout.object_key(stored.storage_key.key.as_str());
        if stored.public.status == UploadSessionStatus::Open
            || stored.public.next_offset != Some(actual)
        {
            stored.public.status = UploadSessionStatus::Completing;
            stored.public.next_offset = Some(actual);
            self.write_session(session, &stored).await?;
        }
        let staged = Bytes::from(staged);
        let etag = self
            .put_completed_object(&object_key, staged, stored.public.content_type.as_deref())
            .await?;
        let object = self.object_ref(&stored, actual, etag, checksum);
        stored.public.status = UploadSessionStatus::Complete;
        stored.public.next_offset = Some(actual);
        stored.object = Some(object.clone());
        stored.cleanup_pending = true;
        self.write_session(session, &stored).await?;
        self.cleanup_staged_bytes(session, &mut stored).await?;
        Ok(object)
    }
}

impl StorageBackend for S3StorageBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Sequential proxy (the coalescing-tail path, the default) and
        // server-mediated native multipart (the by-number `upload_part` path) are
        // both backed by S3 multipart uploads.
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
            self.ensure_requested_size_is_supported(request.size)?;
            // By-number multipart requires the total length up front: parts are
            // tracked only on S3 (not in the session), so without a declared size
            // neither `set_upload_length` nor completion can reason about how much
            // has been committed, and a deferred length could be set below
            // already-uploaded parts and strand the session.
            if strategy == UploadStrategy::Multipart && request.size.is_none() {
                return Err(StorageError::policy_rejected(
                    "multipart uploads require a declared length (size) at initiation",
                ));
            }
            let max_bytes = request.policy.max_bytes.min(self.max_proxy_upload_bytes);
            // The by-number multipart path advertises S3-aware part sizing and the
            // policy's concurrency. It pins the part size (rather than allowing up
            // to the 5 GiB provider ceiling): with every part bounded to
            // `part_size` and the count bounded to `ceil(max_bytes / part_size)`,
            // the worst-case stored bytes stay within one part of the budget
            // without tracking a cumulative size across concurrent parts. The
            // sequential path keeps the policy's chunk plan unchanged.
            let (plan, session_part_size) = if strategy == UploadStrategy::Multipart {
                let part_size = part_size_for(max_bytes);
                let plan = TransferPlan {
                    min_part_size: Some(S3_MIN_PART_SIZE.min(part_size)),
                    preferred_part_size: Some(part_size),
                    max_part_size: Some(part_size),
                    max_parts: Some(max_bytes.div_ceil(part_size).max(1) as u32),
                    max_concurrent_parts: request.policy.max_concurrent_parts.max(1),
                    resumable: true,
                };
                // Keep the legacy `part_size` field in sync with the plan so a
                // client reading only that field still sends parts at the S3 floor
                // rather than `preferred_chunk_size` (which may be below 5 MiB).
                (plan, Some(part_size))
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
            let stored = StoredUploadSession {
                public: session.clone(),
                owner: ctx.actor.clone(),
                storage_key: request.storage_key,
                visibility: request.policy.visibility,
                max_bytes,
                checksum_policy: request.policy.checksum,
                request_metadata: request.metadata,
                object: None,
                cleanup_pending: false,
                native: NativeUploadState::Multipart(S3MultipartState {
                    upload_id: None,
                    parts: Vec::new(),
                    next_part_number: 1,
                    pending_tail: Vec::new(),
                }),
            };
            // No staged `bytes.tmp` is created up front; native sessions only
            // write a tail object once there are sub-part bytes to buffer.
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
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
            let _guard = lock.lock().await;
            let mut stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            // Only legacy sessions reconcile here. `inspect_upload` is also on the
            // internal routing/authorization path (`backend_for_session` calls it
            // for every part `PUT`), so it must stay cheap: a by-number multipart
            // session deliberately does NOT `ListParts` here, which would turn an
            // n-part upload into O(n²) provider calls serialized on the session
            // lock. Per-part resume listing is surfaced separately via
            // `list_committed_parts` rather than on this hot path.
            if stored.public.status == UploadSessionStatus::Open
                && stored.native.multipart().is_none()
            {
                // Legacy sessions recover their offset from the staged object.
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
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
            let _guard = lock.lock().await;
            self.ensure_requested_size_is_supported(Some(size))?;
            let mut stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            self.persist_expired_if_needed(&session, &stored).await?;
            let committed_offset = match stored.native.multipart() {
                Some(state) => state.committed_len(),
                None => self
                    .reconcile_staged_bytes(&session, stored.public.next_offset.unwrap_or(0))
                    .await?
                    .len() as u64,
            };
            ensure_upload_length_can_be_set(
                stored.max_bytes,
                &stored.public,
                committed_offset,
                size,
            )?;
            stored.public.size = Some(size);
            stored.public.next_offset = Some(committed_offset);
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
            let lock = self.session_locks.lock(&session);
            let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
            let _guard = lock.lock().await;
            let stored = self.read_session(&session).await?;
            ensure_owner(&ctx.actor, &stored.owner)?;
            self.persist_expired_if_needed(&session, &stored).await?;
            ensure_open(&stored.public)?;
            if stored.public.strategy != UploadStrategy::Sequential {
                return Err(StorageError::unsupported(
                    "S3 backend currently supports sequential proxy uploads only",
                ));
            }
            if stored.native.multipart().is_some() {
                self.append_multipart(&session, stored, offset, bytes).await
            } else {
                self.append_legacy(&session, stored, offset, bytes).await
            }
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
            // trait method is directly callable, so reject zero before it reaches
            // S3 `UploadPart` (which only accepts 1..=10000).
            if number == 0 {
                return Err(StorageError::policy_rejected(
                    "multipart part number must be at least 1",
                ));
            }
            // S3 `UploadPart` requires a known length; the server reads it from
            // the request `Content-Length` rather than draining the stream.
            let content_length = body.content_length().ok_or_else(|| {
                StorageError::client("S3 part upload requires a Content-Length header")
            })?;

            // Resolve the upload id under the session lock (created lazily on the
            // first part), then release the lock before streaming so sibling parts
            // upload concurrently. Per-part receipts are not persisted — the
            // by-number completion path lists them from S3 — so there is no
            // session write to serialize while a part streams.
            let (upload_id, object_key, public) = {
                let lock = self.session_locks.lock(&session);
                let _cleanup = UploadSessionLockCleanup::new(&self.session_locks, &session);
                let _guard = lock.lock().await;
                let mut stored = self.read_session(&session).await?;
                ensure_owner(&ctx.actor, &stored.owner)?;
                self.persist_expired_if_needed(&session, &stored).await?;
                ensure_open(&stored.public)?;
                if stored.public.strategy != UploadStrategy::Multipart {
                    return Err(StorageError::unsupported(
                        "S3 backend part upload requires the multipart strategy",
                    ));
                }
                // Bound aggregate storage without cumulative cross-part state: a
                // valid upload of `limit_bytes` is `ceil(limit_bytes / part_size)`
                // parts of at most `part_size` each, so capping the part number
                // and per-part size keeps the worst case within one part of the
                // budget (completion still enforces the exact total). Prefer the
                // declared size over the scope max so a client cannot upload extra
                // parts beyond the object it declared (which would only fail, with
                // unremovable parts, at completion).
                let part_size = part_size_for(stored.max_bytes);
                let limit_bytes = stored.public.size.unwrap_or(stored.max_bytes);
                let max_parts = limit_bytes.div_ceil(part_size).max(1);
                if u64::from(number) > max_parts {
                    return Err(StorageError::policy_rejected(format!(
                        "part number {number} exceeds the maximum of {max_parts} parts for this upload"
                    )));
                }
                if content_length > part_size {
                    return Err(StorageError::policy_rejected(format!(
                        "part exceeds the maximum part size of {part_size} bytes for this upload"
                    )));
                }
                let object_key = self.layout.object_key(stored.storage_key.key.as_str());
                let upload_id = self
                    .ensure_upload_id(&session, &mut stored, &object_key)
                    .await?;
                (upload_id, object_key, stored.public.clone())
            };

            // Enforce the scope's advertised concurrency server-side: a permit is
            // held for the streaming duration so at most `max_concurrent_parts`
            // parts of this session stream at once, regardless of how many part
            // requests the client fires. Acquired after releasing the session
            // lock so it bounds streams, not the cheap id lookup.
            let max_concurrent = public.plan.max_concurrent_parts.max(1) as usize;
            let _permit = self
                .part_concurrency
                .semaphore(&session, max_concurrent)
                .acquire_owned()
                .await
                .map_err(|_| StorageError::UploadClosed {
                    session: session.to_string(),
                })?;
            // The open check above ran before the permit wait; under saturated
            // concurrency the wait can outlast the session's expiry. Re-check the
            // captured deadline so an expired session can't still accept a part
            // (no extra round-trip — `expires_at` is fixed at initiation). A
            // concurrent abort is handled by S3 itself: the UploadPart then fails
            // with `NoSuchUpload`.
            if OffsetDateTime::now_utc() >= public.expires_at {
                return Err(StorageError::UploadClosed {
                    session: session.to_string(),
                });
            }

            self.upload_part_streaming(
                &object_key,
                &upload_id,
                number as i32,
                body,
                content_length,
            )
            .await?;
            Ok(public)
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
                // Terminal: the object is finalized, so the part-concurrency pool
                // can go (in-flight permit holders keep their own Arc clone).
                self.part_concurrency.forget(&request.session);
                return Ok(object);
            }
            self.persist_expired_if_needed(&request.session, &stored)
                .await?;
            ensure_completable(&stored.public)?;
            let session = request.session.clone();
            let result = if stored.native.multipart().is_some() {
                if stored.public.strategy == UploadStrategy::Multipart {
                    self.complete_multipart_by_number_flow(&session, stored, request)
                        .await
                } else {
                    self.complete_multipart_upload_flow(&session, stored, request)
                        .await
                }
            } else {
                self.complete_legacy(&session, stored, request).await
            };
            // Only drop the concurrency pool once completion actually succeeds. A
            // failed attempt (missing length, part gap, undersized/in-flight part)
            // leaves the session `Open`, so the existing semaphore must survive or
            // a later part would create a fresh one and bypass the limit.
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
            let known_session = match self.read_session(&session).await {
                Ok(stored) => {
                    ensure_owner(&ctx.actor, &stored.owner)?;
                    // Stop concurrent part uploads before aborting. `close()`
                    // immediately fails every queued AND future `upload_part`
                    // permit-acquire, so no part that has not already started
                    // streaming can reach S3 after the abort. (Closing rather than
                    // draining avoids racing Tokio's fair semaphore queue, where
                    // waiters sit ahead of an `acquire_many` and would stream as
                    // active parts release permits.) A part already mid-stream at
                    // this instant is a narrow window whose orphaned provider part
                    // is reclaimed by bucket-lifecycle / stale-upload cleanup.
                    if stored.public.strategy == UploadStrategy::Multipart {
                        let max = stored.public.plan.max_concurrent_parts.max(1);
                        self.part_concurrency
                            .semaphore(&session, max as usize)
                            .close();
                    }
                    // Best-effort abort of the provider multipart upload before
                    // dropping local state.
                    self.abort_multipart_session(&stored).await?;
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
            self.part_concurrency.forget(&session);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{part_size_for, S3_MAX_PARTS, S3_MAX_PART_SIZE, S3_MIN_PART_SIZE};

    #[test]
    fn part_size_stays_at_floor_for_small_caps() {
        // The default 64 MiB cap (and anything that fits in <=10k 5 MiB parts)
        // uses the 5 MiB floor unchanged.
        assert_eq!(part_size_for(64 * 1024 * 1024), S3_MIN_PART_SIZE);
        assert_eq!(
            part_size_for(S3_MIN_PART_SIZE * S3_MAX_PARTS),
            S3_MIN_PART_SIZE
        );
    }

    #[test]
    fn part_size_scales_to_stay_under_part_cap() {
        let max_bytes = 200 * 1024 * 1024 * 1024; // 200 GiB
        let part = part_size_for(max_bytes);
        assert!(part > S3_MIN_PART_SIZE);
        assert!(part.is_multiple_of(1024 * 1024), "rounded to a whole MiB");
        assert!(max_bytes.div_ceil(part) <= S3_MAX_PARTS);
    }

    #[test]
    fn part_size_never_exceeds_provider_max() {
        assert_eq!(part_size_for(u64::MAX), S3_MAX_PART_SIZE);
    }
}
