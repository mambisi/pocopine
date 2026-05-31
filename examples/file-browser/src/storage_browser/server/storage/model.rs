use pocopine::{ServerError, ServerResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage_browser::server::storage::*;
use crate::storage_browser::{
    GcsConnectionInput, S3ConnectionInput, StorageConnectionEdit, StorageConnectionSummary,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub(crate) enum SavedConnection {
    S3(SavedS3Connection),
    Gcs(SavedGcsConnection),
}

impl SavedConnection {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::S3(connection) => &connection.id,
            Self::Gcs(connection) => &connection.id,
        }
    }

    pub(crate) fn summary(&self) -> StorageConnectionSummary {
        match self {
            Self::S3(connection) => connection.summary(),
            Self::Gcs(connection) => connection.summary(),
        }
    }

    pub(crate) fn edit(&self) -> StorageConnectionEdit {
        match self {
            Self::S3(connection) => connection.edit(),
            Self::Gcs(connection) => connection.edit(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedS3Connection {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) endpoint_url: String,
    pub(crate) region: String,
    pub(crate) bucket: String,
    pub(crate) root_prefix: String,
    pub(crate) access_key_id: String,
    pub(crate) secret_access_key: String,
    pub(crate) force_path_style: bool,
    #[serde(skip)]
    create_bucket_if_missing: bool,
}

impl SavedS3Connection {
    pub(crate) fn from_input(
        input: S3ConnectionInput,
        existing_secret: Option<&str>,
    ) -> ServerResult<Self> {
        let bucket = input.bucket.trim().to_string();
        if bucket.is_empty() {
            return Err(ServerError::App("bucket is required".to_string()));
        }
        let access_key_id = input.access_key_id.trim().to_string();
        if access_key_id.is_empty() {
            return Err(ServerError::App("access key is required".to_string()));
        }
        let secret_access_key = input.secret_access_key.trim().to_string();
        let secret_access_key = if secret_access_key.is_empty() {
            existing_secret.unwrap_or_default().to_string()
        } else {
            secret_access_key
        };
        if secret_access_key.is_empty() {
            return Err(ServerError::App("secret key is required".to_string()));
        }

        let endpoint_url = input.endpoint_url.trim().to_string();
        let region = input.region.trim();
        let region = if region.is_empty() {
            DEFAULT_REGION.to_string()
        } else {
            region.to_string()
        };
        let name = input.name.trim();
        let name = if name.is_empty() {
            format!("{} / {}", provider_label("s3"), bucket)
        } else {
            name.to_string()
        };
        let id = if input.id.trim().is_empty() {
            Uuid::new_v4().to_string()
        } else {
            input.id.trim().to_string()
        };

        Ok(Self {
            id,
            name,
            endpoint_url,
            region,
            bucket,
            root_prefix: normalize_prefix(&input.root_prefix),
            access_key_id,
            secret_access_key,
            force_path_style: input.force_path_style,
            create_bucket_if_missing: input.create_bucket_if_missing,
        })
    }

    pub(crate) fn create_bucket_if_requested(&self) -> bool {
        // The field is consumed before persistence so it is not part of the
        // saved connection shape. MinIO demo users can create once and then
        // browse normally.
        self.create_bucket_if_missing
    }

    pub(crate) fn summary(&self) -> StorageConnectionSummary {
        StorageConnectionSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            provider: "s3".to_string(),
            provider_label: "S3".to_string(),
            icon: s3_connection_icon(&self.endpoint_url).to_string(),
            favicon_url: connection_favicon_url("s3", &self.endpoint_url),
            endpoint_url: self.endpoint_url.clone(),
            region: self.region.clone(),
            bucket: self.bucket.clone(),
            root_prefix: self.root_prefix.clone(),
            access_key_hint: access_key_hint(&self.access_key_id),
            force_path_style: self.force_path_style,
            project_id: String::new(),
            use_emulator: false,
            use_anonymous_auth: false,
            gcs_auth_mode: String::new(),
            gcs_service_account_hint: String::new(),
            gcs_has_service_account_json: false,
        }
    }

    pub(crate) fn edit(&self) -> StorageConnectionEdit {
        StorageConnectionEdit {
            id: self.id.clone(),
            name: self.name.clone(),
            provider: "s3".to_string(),
            endpoint_url: self.endpoint_url.clone(),
            region: self.region.clone(),
            bucket: self.bucket.clone(),
            root_prefix: self.root_prefix.clone(),
            access_key_id: self.access_key_id.clone(),
            force_path_style: self.force_path_style,
            project_id: String::new(),
            use_emulator: false,
            use_anonymous_auth: false,
            gcs_auth_mode: String::new(),
            gcs_service_account_hint: String::new(),
            gcs_has_service_account_json: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedGcsConnection {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) endpoint_url: String,
    pub(crate) project_id: String,
    pub(crate) bucket: String,
    pub(crate) root_prefix: String,
    pub(crate) use_emulator: bool,
    #[serde(default)]
    auth: SavedGcsAuth,
    #[serde(default)]
    use_anonymous_auth: bool,
}

impl SavedGcsConnection {
    pub(crate) fn effective_auth(&self) -> SavedGcsAuth {
        if matches!(self.auth, SavedGcsAuth::ApplicationDefault) && self.use_anonymous_auth {
            SavedGcsAuth::Anonymous
        } else {
            self.auth.clone()
        }
    }

    pub(crate) fn from_input(
        input: GcsConnectionInput,
        existing_auth: Option<SavedGcsAuth>,
    ) -> ServerResult<Self> {
        let bucket = input.bucket.trim().to_string();
        if bucket.is_empty() {
            return Err(ServerError::App("bucket is required".to_string()));
        }
        let endpoint_url = input.endpoint_url.trim().trim_end_matches('/').to_string();
        if input.use_emulator && endpoint_url.is_empty() {
            return Err(ServerError::App(
                "GCS emulator endpoint is required".to_string(),
            ));
        }
        let name = input.name.trim();
        let name = if name.is_empty() {
            format!("{} / {}", provider_label("gcs"), bucket)
        } else {
            name.to_string()
        };
        let id = if input.id.trim().is_empty() {
            Uuid::new_v4().to_string()
        } else {
            input.id.trim().to_string()
        };
        let auth = gcs_auth_from_input(&input, existing_auth)?;
        let use_anonymous_auth = matches!(&auth, SavedGcsAuth::Anonymous);
        let project_id = match (&auth, input.project_id.trim()) {
            (
                SavedGcsAuth::ServiceAccountJson {
                    project_id_hint, ..
                },
                "",
            ) => project_id_hint.clone(),
            (_, project_id) => project_id.to_string(),
        };

        Ok(Self {
            id,
            name,
            endpoint_url,
            project_id,
            bucket,
            root_prefix: normalize_prefix(&input.root_prefix),
            use_emulator: input.use_emulator,
            auth,
            use_anonymous_auth,
        })
    }

    pub(crate) fn summary(&self) -> StorageConnectionSummary {
        let auth = self.effective_auth();
        StorageConnectionSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            provider: "gcs".to_string(),
            provider_label: provider_label("gcs").to_string(),
            icon: "cloud".to_string(),
            favicon_url: connection_favicon_url("gcs", &self.endpoint_url),
            endpoint_url: self.endpoint_url.clone(),
            region: self.project_id.clone(),
            bucket: self.bucket.clone(),
            root_prefix: self.root_prefix.clone(),
            access_key_hint: auth.hint(),
            force_path_style: false,
            project_id: self.project_id.clone(),
            use_emulator: self.use_emulator,
            use_anonymous_auth: matches!(&auth, SavedGcsAuth::Anonymous),
            gcs_auth_mode: auth.mode().to_string(),
            gcs_service_account_hint: auth.hint(),
            gcs_has_service_account_json: auth.has_service_account_json(),
        }
    }

    pub(crate) fn edit(&self) -> StorageConnectionEdit {
        let auth = self.effective_auth();
        StorageConnectionEdit {
            id: self.id.clone(),
            name: self.name.clone(),
            provider: "gcs".to_string(),
            endpoint_url: self.endpoint_url.clone(),
            region: String::new(),
            bucket: self.bucket.clone(),
            root_prefix: self.root_prefix.clone(),
            access_key_id: String::new(),
            force_path_style: false,
            project_id: self.project_id.clone(),
            use_emulator: self.use_emulator,
            use_anonymous_auth: matches!(&auth, SavedGcsAuth::Anonymous),
            gcs_auth_mode: auth.mode().to_string(),
            gcs_service_account_hint: auth.hint(),
            gcs_has_service_account_json: auth.has_service_account_json(),
        }
    }
}
