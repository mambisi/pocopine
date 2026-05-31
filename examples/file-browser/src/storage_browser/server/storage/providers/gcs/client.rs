use google_cloud_auth::credentials::{
    anonymous::Builder as AnonymousCredentials,
    service_account::Builder as ServiceAccountCredentials,
};
use google_cloud_storage::client::{Storage as GcsStorageClient, StorageControl};
use pocopine::{ServerError, ServerResult};
use pocopine_storage::{StorageError, StorageResult};
use pocopine_storage_gcs::GcsStorageBackend;

use crate::storage_browser::server::storage::*;

impl SavedGcsConnection {
    pub(crate) async fn upload_backend(
        &self,
        max_proxy_upload_bytes: u64,
    ) -> StorageResult<GcsStorageBackend> {
        let storage = gcs_storage_client_storage(self).await?;
        let backend = if self.use_emulator {
            GcsStorageBackend::named_for_emulator(
                UPLOAD_BACKEND,
                storage,
                self.endpoint_url.clone(),
                self.bucket.clone(),
            )?
        } else {
            let control = gcs_control_client_storage(self).await?;
            GcsStorageBackend::named(UPLOAD_BACKEND, storage, control, self.bucket.clone())?
        };
        backend
            .with_prefix(self.root_prefix.clone())?
            .with_max_proxy_upload_bytes(max_proxy_upload_bytes)
    }
}

pub(crate) async fn ensure_gcs_bucket(connection: &SavedGcsConnection) -> ServerResult<()> {
    list_gcs_page(connection, "", None, None, 1)
        .await
        .map(|_| ())
}

pub(crate) async fn gcs_storage_client(
    connection: &SavedGcsConnection,
) -> ServerResult<GcsStorageClient> {
    gcs_storage_client_storage(connection)
        .await
        .map_err(|err| ServerError::App(format!("build GCS storage client: {err}")))
}

pub(crate) async fn gcs_control_client(
    connection: &SavedGcsConnection,
) -> ServerResult<StorageControl> {
    gcs_control_client_storage(connection)
        .await
        .map_err(|err| ServerError::App(format!("build GCS control client: {err}")))
}

pub(crate) async fn gcs_storage_client_storage(
    connection: &SavedGcsConnection,
) -> StorageResult<GcsStorageClient> {
    let mut builder = GcsStorageClient::builder();
    if !connection.endpoint_url.is_empty() {
        builder = builder.with_endpoint(connection.endpoint_url.clone());
    }
    if let Some(credentials) = gcs_credentials(connection)? {
        builder = builder.with_credentials(credentials);
    }
    builder
        .build()
        .await
        .map_err(|err| StorageError::backend(format!("build GCS storage client: {err}")))
}

pub(crate) async fn gcs_control_client_storage(
    connection: &SavedGcsConnection,
) -> StorageResult<StorageControl> {
    let mut builder = StorageControl::builder();
    if !connection.endpoint_url.is_empty() {
        builder = builder.with_endpoint(connection.endpoint_url.clone());
    }
    if let Some(credentials) = gcs_credentials(connection)? {
        builder = builder.with_credentials(credentials);
    }
    builder
        .build()
        .await
        .map_err(|err| StorageError::backend(format!("build GCS control client: {err}")))
}

pub(crate) fn gcs_credentials(
    connection: &SavedGcsConnection,
) -> StorageResult<Option<google_cloud_auth::credentials::Credentials>> {
    match connection.effective_auth() {
        SavedGcsAuth::ApplicationDefault => Ok(None),
        SavedGcsAuth::Anonymous => Ok(Some(AnonymousCredentials::new().build())),
        SavedGcsAuth::ServiceAccountJson { json, .. } => ServiceAccountCredentials::new(json)
            .build()
            .map(Some)
            .map_err(|err| StorageError::backend(format!("build GCS service account auth: {err}"))),
    }
}

pub(crate) fn gcs_bucket_resource(bucket: &str) -> String {
    if bucket.starts_with("projects/") {
        bucket.to_string()
    } else {
        format!("projects/_/buckets/{bucket}")
    }
}
