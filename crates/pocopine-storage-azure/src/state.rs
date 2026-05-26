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
    pub(crate) meta_etag: Option<String>,
}

pub(crate) struct AzureObjectBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) etag: Option<String>,
    pub(crate) version_id: Option<String>,
    pub(crate) truncated: bool,
}

impl AzureObjectBytes {
    pub(crate) fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            etag: None,
            version_id: None,
            truncated: false,
        }
    }
}

pub(crate) struct AzureObjectMetadata {
    pub(crate) size: Option<u64>,
    pub(crate) etag: Option<String>,
    pub(crate) version_id: Option<String>,
}

pub(crate) struct AzureObjectWrite {
    pub(crate) etag: Option<String>,
    pub(crate) version_id: Option<String>,
}

pub(crate) enum AbortSessionRead {
    Known(Box<StoredUploadSession>),
    Missing,
    Corrupt,
}

pub(crate) fn decode_session_object(
    object: AzureObjectBytes,
) -> Result<StoredUploadSession, serde_json::Error> {
    let meta_etag = object.etag;
    let mut stored: StoredUploadSession = serde_json::from_slice(&object.bytes)?;
    stored.meta_etag = meta_etag;
    refresh_expired(&mut stored.public);
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pocopine_storage::{
        ChecksumPolicy, ObjectVisibility, SafeObjectKey, StorageActor, StorageKey, TransferPlan,
        UploadSession, UploadSessionId, UploadSessionStatus, UploadStrategy,
    };
    use time::OffsetDateTime;

    use super::*;

    #[test]
    fn metadata_decode_refreshes_expiration_and_carries_etag_state() {
        let session = UploadSession {
            id: UploadSessionId::new("expired-session").unwrap(),
            scope: "files".to_string(),
            file_name: "hello.txt".to_string(),
            size: Some(5),
            content_type: Some("text/plain".to_string()),
            metadata: BTreeMap::new(),
            strategy: UploadStrategy::Sequential,
            status: UploadSessionStatus::Open,
            next_offset: Some(0),
            part_size: Some(5),
            plan: TransferPlan {
                min_part_size: None,
                preferred_part_size: Some(5),
                max_part_size: None,
                max_parts: None,
                max_concurrent_parts: 1,
                resumable: true,
            },
            uploaded_parts: Vec::new(),
            expires_at: OffsetDateTime::UNIX_EPOCH,
        };
        let stored = StoredUploadSession {
            public: session,
            owner: StorageActor::System("owner".to_string()),
            storage_key: StorageKey::new(SafeObjectKey::parse("files/hello.txt").unwrap()),
            visibility: ObjectVisibility::Private,
            max_bytes: 1024,
            checksum_policy: ChecksumPolicy::None,
            request_metadata: BTreeMap::new(),
            object: None,
            completion_object_key: None,
            cleanup_pending: false,
            meta_etag: None,
        };
        let bytes = serde_json::to_vec(&stored).unwrap();

        let decoded = decode_session_object(AzureObjectBytes {
            bytes,
            etag: Some("\"etag-1\"".to_string()),
            version_id: Some("version-1".to_string()),
            truncated: false,
        })
        .unwrap();

        assert_eq!(decoded.public.status, UploadSessionStatus::Expired);
        assert_eq!(decoded.meta_etag.as_deref(), Some("\"etag-1\""));
    }
}
