use std::collections::BTreeMap;

use pocopine_storage::backend_common::refresh_expired;
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
    pub(crate) completion_object_key: Option<String>,
    #[serde(default)]
    pub(crate) cleanup_pending: bool,
    /// Transfer mechanism. Sessions written before native compose support omit
    /// this field and deserialize as [`NativeUploadState::LegacyProxy`], so
    /// in-flight uploads from an older build keep the staged-object rewrite path.
    #[serde(default)]
    pub(crate) native: NativeUploadState,
    #[serde(skip)]
    pub(crate) meta_generation: Option<i64>,
}

/// Backend transfer mechanism for an upload session.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeUploadState {
    /// Legacy staged-object rewrite (download → extend → re-upload per append).
    #[default]
    LegacyProxy,
    /// Native compose: each flushed block is written once as a component object
    /// and the final object is assembled with a single `ComposeObject`. Only an
    /// unflushed tail (smaller than one component) is buffered, inline in this
    /// metadata so the tail and bookkeeping persist in one atomic write.
    Compose(GcsComposeState),
}

/// Compose bookkeeping for one session.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct GcsComposeState {
    /// Component objects already written, in order.
    #[serde(default)]
    pub(crate) components: Vec<GcsComponent>,
    /// Next component index to assign.
    #[serde(default)]
    pub(crate) next_index: u32,
    /// Bytes received but not yet flushed to a component (always < one component,
    /// except the final remainder at completion). Base64 inline in the metadata.
    #[serde(
        default,
        with = "pocopine_codec::base64_bytes",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub(crate) pending_tail: Vec<u8>,
}

/// One written component object.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct GcsComponent {
    pub(crate) key: String,
    pub(crate) size: u64,
    #[serde(default)]
    pub(crate) generation: Option<i64>,
}

impl NativeUploadState {
    pub(crate) fn compose(&self) -> Option<&GcsComposeState> {
        match self {
            NativeUploadState::Compose(state) => Some(state),
            NativeUploadState::LegacyProxy => None,
        }
    }

    pub(crate) fn compose_mut(&mut self) -> Option<&mut GcsComposeState> {
        match self {
            NativeUploadState::Compose(state) => Some(state),
            NativeUploadState::LegacyProxy => None,
        }
    }
}

impl GcsComposeState {
    pub(crate) fn flushed_len(&self) -> u64 {
        self.components.iter().map(|component| component.size).sum()
    }

    pub(crate) fn committed_len(&self) -> u64 {
        self.flushed_len() + self.pending_tail.len() as u64
    }
}

pub(crate) struct GcsObjectBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) etag: Option<String>,
    pub(crate) generation: Option<String>,
    pub(crate) generation_match: Option<i64>,
    pub(crate) truncated: bool,
}

impl GcsObjectBytes {
    pub(crate) fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            etag: None,
            generation: None,
            generation_match: None,
            truncated: false,
        }
    }
}

pub(crate) struct GcsObjectMetadata {
    pub(crate) size: Option<u64>,
    pub(crate) etag: Option<String>,
    pub(crate) generation: Option<String>,
    pub(crate) generation_match: Option<i64>,
}

/// Full object attributes including custom metadata (fetched via the control
/// layer, since the streaming read only exposes lightweight highlights).
pub(crate) struct GcsObjectAttrs {
    pub(crate) etag: Option<String>,
    pub(crate) generation: Option<i64>,
    pub(crate) owner_session: Option<String>,
}

pub(crate) struct GcsObjectWrite {
    pub(crate) etag: Option<String>,
    pub(crate) generation: Option<String>,
    pub(crate) generation_match: Option<i64>,
}

pub(crate) enum AbortSessionRead {
    Known(Box<StoredUploadSession>),
    Missing,
    Corrupt,
}

pub(crate) fn decode_session_object(
    object: GcsObjectBytes,
) -> Result<StoredUploadSession, serde_json::Error> {
    let generation_match = object.generation_match;
    let mut stored: StoredUploadSession = serde_json::from_slice(&object.bytes)?;
    stored.meta_generation = generation_match;
    refresh_expired(&mut stored.public);
    Ok(stored)
}
