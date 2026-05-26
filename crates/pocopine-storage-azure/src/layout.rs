use azure_core::http::Url;
use pocopine_storage::{SafeObjectKey, StorageError, StorageResult, UploadSessionId};

pub(crate) const DEFAULT_INTERNAL_PREFIX: &str = "__pocopine/storage/sessions";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AzureKeyLayout {
    pub(crate) container_name: String,
    pub(crate) container_url: String,
    pub(crate) object_prefix: Option<String>,
    internal_prefix_base: String,
    pub(crate) internal_prefix: String,
}

impl AzureKeyLayout {
    pub(crate) fn new(
        container_url: &Url,
        object_prefix: Option<String>,
        internal_prefix: String,
    ) -> StorageResult<Self> {
        let container_name = container_name_from_url(container_url)?;
        let object_prefix = match object_prefix {
            Some(prefix) => normalize_object_prefix(prefix)?,
            None => None,
        };
        let internal_prefix_base = normalize_internal_prefix(internal_prefix).ok_or_else(|| {
            StorageError::policy_rejected("Azure internal prefix must not be empty")
        })?;
        let internal_prefix =
            join_optional_prefix(object_prefix.as_deref(), internal_prefix_base.as_str());
        Ok(Self {
            container_name,
            container_url: container_url.to_string(),
            object_prefix,
            internal_prefix_base,
            internal_prefix,
        })
    }

    pub(crate) fn with_prefix(&self, prefix: String) -> StorageResult<Self> {
        Self::new(
            &Url::parse(&self.container_url).map_err(|err| {
                StorageError::backend(format!("parse Azure container URL: {err}"))
            })?,
            Some(prefix),
            self.internal_prefix_base.clone(),
        )
    }

    pub(crate) fn with_internal_prefix(&self, prefix: String) -> StorageResult<Self> {
        Self::new(
            &Url::parse(&self.container_url).map_err(|err| {
                StorageError::backend(format!("parse Azure container URL: {err}"))
            })?,
            self.object_prefix.clone(),
            prefix,
        )
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

fn container_name_from_url(url: &Url) -> StorageResult<String> {
    let segments: Vec<_> = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let container = match segments.as_slice() {
        [container] => (*container).to_string(),
        [_account, container] if is_local_azurite_url(url) => (*container).to_string(),
        [] => {
            return Err(StorageError::policy_rejected(
                "Azure container URL must include a container name",
            ));
        }
        _ => {
            return Err(StorageError::policy_rejected(
                "Azure container URL must point directly to one container",
            ));
        }
    };
    if container.trim().is_empty() {
        return Err(StorageError::policy_rejected(
            "Azure container name must not be empty",
        ));
    }
    Ok(container)
}

fn is_local_azurite_url(url: &Url) -> bool {
    url.host_str()
        .map(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"))
        .unwrap_or(false)
}

fn normalize_object_prefix(prefix: String) -> StorageResult<Option<String>> {
    let prefix = prefix.trim().trim_matches('/');
    if prefix.is_empty() {
        return Ok(None);
    }
    SafeObjectKey::parse(prefix)?;
    Ok(Some(prefix.to_string()))
}

fn normalize_internal_prefix(prefix: String) -> Option<String> {
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
    use azure_core::http::Url;

    use super::*;

    fn container_url() -> Url {
        Url::parse("http://127.0.0.1:10000/devstoreaccount1/pocopine").unwrap()
    }

    #[test]
    fn prefix_layout_keeps_internal_objects_out_of_app_keyspace() {
        let layout = AzureKeyLayout::new(
            &container_url(),
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
        assert_eq!(layout.container_name, "pocopine");
    }

    #[test]
    fn prefix_and_internal_prefix_are_order_independent() {
        let first =
            AzureKeyLayout::new(&container_url(), None, DEFAULT_INTERNAL_PREFIX.to_string())
                .unwrap()
                .with_internal_prefix("custom/sessions".to_string())
                .unwrap()
                .with_prefix("tenant-a".to_string())
                .unwrap();
        let second =
            AzureKeyLayout::new(&container_url(), None, DEFAULT_INTERNAL_PREFIX.to_string())
                .unwrap()
                .with_prefix("tenant-a".to_string())
                .unwrap()
                .with_internal_prefix("custom/sessions".to_string())
                .unwrap();

        assert_eq!(first.internal_prefix, "tenant-a/custom/sessions");
        assert_eq!(second.internal_prefix, first.internal_prefix);
    }

    #[test]
    fn empty_container_is_rejected() {
        let url = Url::parse("https://account.blob.core.windows.net/").unwrap();
        assert!(AzureKeyLayout::new(&url, None, DEFAULT_INTERNAL_PREFIX.to_string()).is_err());
    }

    #[test]
    fn multi_segment_non_azurite_container_url_is_rejected() {
        let url = Url::parse("https://account.blob.core.windows.net/pocopine/audit/").unwrap();
        assert!(AzureKeyLayout::new(&url, None, DEFAULT_INTERNAL_PREFIX.to_string()).is_err());
    }

    #[test]
    fn reserved_object_prefix_is_rejected() {
        let rejected = AzureKeyLayout::new(
            &container_url(),
            Some("__pocopine".to_string()),
            DEFAULT_INTERNAL_PREFIX.to_string(),
        );
        assert!(matches!(
            rejected,
            Err(StorageError::InvalidValue { field, .. }) if field == "object key"
        ));
    }

    #[test]
    fn path_traversal_object_prefix_is_rejected() {
        let rejected = AzureKeyLayout::new(
            &container_url(),
            Some("tenant/../files".to_string()),
            DEFAULT_INTERNAL_PREFIX.to_string(),
        );
        assert!(matches!(
            rejected,
            Err(StorageError::InvalidValue { field, .. }) if field == "object key"
        ));
    }

    #[test]
    fn empty_internal_prefix_is_rejected() {
        assert!(AzureKeyLayout::new(&container_url(), None, " / ".to_string()).is_err());
    }
}
