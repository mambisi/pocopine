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
}
