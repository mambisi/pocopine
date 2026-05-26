use pocopine_storage::{StorageError, StorageResult, UploadSessionId};

pub(crate) const DEFAULT_INTERNAL_PREFIX: &str = "__pocopine/storage/sessions";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GcsKeyLayout {
    pub(crate) bucket: String,
    pub(crate) bucket_resource: String,
    pub(crate) object_prefix: Option<String>,
    internal_prefix_base: String,
    pub(crate) internal_prefix: String,
}

impl GcsKeyLayout {
    pub(crate) fn new(
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

    pub(crate) fn with_prefix(&self, prefix: String) -> StorageResult<Self> {
        let object_prefix = normalize_prefix(prefix);
        Self::new(
            self.bucket.clone(),
            object_prefix,
            self.internal_prefix_base.clone(),
        )
    }

    pub(crate) fn with_internal_prefix(&self, prefix: String) -> StorageResult<Self> {
        Self::new(
            self.bucket.clone(),
            self.object_prefix.clone(),
            prefix.trim_matches('/').to_string(),
        )
    }

    pub(crate) fn bucket_resource(&self) -> &str {
        &self.bucket_resource
    }

    pub(crate) fn bucket(&self) -> &str {
        &self.bucket
    }

    pub(crate) fn object_key(&self, key: &str) -> String {
        join_optional_prefix(self.object_prefix.as_deref(), key)
    }

    pub(crate) fn session_meta_key(&self, session: &UploadSessionId) -> String {
        format!("{}/{}/session.json", self.internal_prefix, session.as_str())
    }

    pub(crate) fn session_bytes_key(&self, session: &UploadSessionId) -> String {
        format!("{}/{}/bytes.tmp", self.internal_prefix, session.as_str())
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
