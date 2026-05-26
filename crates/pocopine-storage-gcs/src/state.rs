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
    #[serde(skip)]
    pub(crate) meta_generation: Option<i64>,
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
