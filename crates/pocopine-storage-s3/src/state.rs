use std::collections::BTreeMap;

use pocopine_storage::{
    ChecksumPolicy, ObjectRef, ObjectVisibility, StorageActor, StorageKey, UploadSession,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredUploadSession {
    pub(crate) public: UploadSession,
    pub(crate) owner: StorageActor,
    pub(crate) storage_key: StorageKey,
    pub(crate) visibility: ObjectVisibility,
    pub(crate) max_bytes: u64,
    pub(crate) checksum_policy: ChecksumPolicy,
    pub(crate) request_metadata: BTreeMap<String, String>,
    pub(crate) object: Option<ObjectRef>,
    #[serde(default)]
    pub(crate) cleanup_pending: bool,
    /// Transfer mechanism backing this session.
    ///
    /// Sessions written before native multipart support omit this field and
    /// deserialize as [`NativeUploadState::LegacyProxy`], so in-flight uploads
    /// created by an older build keep working on the staged-object rewrite path.
    #[serde(default)]
    pub(crate) native: NativeUploadState,
}

/// Backend transfer mechanism for an upload session.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeUploadState {
    /// Legacy staged-object rewrite (download → extend → re-upload per append).
    ///
    /// Only used for sessions created before native multipart was introduced.
    #[default]
    LegacyProxy,
    /// S3 native multipart upload: each flushed block is an `UploadPart`, the
    /// final object is assembled by `CompleteMultipartUpload`, and only an
    /// unflushed tail (smaller than one part) is ever staged in memory/`bytes.tmp`.
    Multipart(S3MultipartState),
}

/// Provider-side multipart bookkeeping for one session.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct S3MultipartState {
    /// `UploadId` returned by `CreateMultipartUpload`.
    ///
    /// Created lazily on the first part flush so an initiated-but-abandoned
    /// session leaves no provider-side multipart upload behind.
    #[serde(default)]
    pub(crate) upload_id: Option<String>,
    /// Parts already uploaded to S3, in ascending part-number order.
    #[serde(default)]
    pub(crate) parts: Vec<S3CompletedPart>,
    /// Next part number to assign (S3 part numbers are 1-based).
    #[serde(default)]
    pub(crate) next_part_number: i32,
    /// Bytes currently buffered in `bytes.tmp` that have not yet been flushed to
    /// a part. Always smaller than one part (the S3 5 MiB minimum), except just
    /// before completion when the final remainder becomes the last part.
    #[serde(default)]
    pub(crate) pending_tail_len: u64,
}

/// One uploaded multipart part.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct S3CompletedPart {
    pub(crate) number: i32,
    pub(crate) etag: String,
    pub(crate) size: u64,
}

impl NativeUploadState {
    pub(crate) fn multipart_mut(&mut self) -> Option<&mut S3MultipartState> {
        match self {
            NativeUploadState::Multipart(state) => Some(state),
            NativeUploadState::LegacyProxy => None,
        }
    }

    pub(crate) fn multipart(&self) -> Option<&S3MultipartState> {
        match self {
            NativeUploadState::Multipart(state) => Some(state),
            NativeUploadState::LegacyProxy => None,
        }
    }
}

impl S3MultipartState {
    /// Total bytes already committed to provider parts (excludes the pending tail).
    pub(crate) fn flushed_len(&self) -> u64 {
        self.parts.iter().map(|part| part.size).sum()
    }
}
