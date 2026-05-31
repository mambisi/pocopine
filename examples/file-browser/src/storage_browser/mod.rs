use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageConnectionSummary {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub provider_label: String,
    pub icon: String,
    pub favicon_url: String,
    pub endpoint_url: String,
    pub region: String,
    pub bucket: String,
    pub root_prefix: String,
    pub access_key_hint: String,
    pub force_path_style: bool,
    pub project_id: String,
    pub use_emulator: bool,
    pub use_anonymous_auth: bool,
    pub gcs_auth_mode: String,
    pub gcs_service_account_hint: String,
    pub gcs_has_service_account_json: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct S3ConnectionInput {
    pub id: String,
    pub name: String,
    pub endpoint_url: String,
    pub region: String,
    pub bucket: String,
    pub root_prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub force_path_style: bool,
    pub create_bucket_if_missing: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GcsConnectionInput {
    pub id: String,
    pub name: String,
    pub endpoint_url: String,
    pub project_id: String,
    pub bucket: String,
    pub root_prefix: String,
    pub use_emulator: bool,
    pub use_anonymous_auth: bool,
    pub auth_mode: String,
    pub service_account_json: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageBrowserConfigEdit {
    pub config_path: String,
    pub upload_max_bytes: u64,
    pub preferred_chunk_bytes: u64,
    pub upload_max_label: String,
    pub preferred_chunk_label: String,
    pub active_upload_max_bytes: u64,
    pub active_preferred_chunk_bytes: u64,
    pub active_upload_max_label: String,
    pub active_preferred_chunk_label: String,
    pub restart_required: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageBrowserConfigInput {
    pub upload_max_bytes: u64,
    pub preferred_chunk_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageConnectionEdit {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub endpoint_url: String,
    pub region: String,
    pub bucket: String,
    pub root_prefix: String,
    pub access_key_id: String,
    pub force_path_style: bool,
    pub project_id: String,
    pub use_emulator: bool,
    pub use_anonymous_auth: bool,
    pub gcs_auth_mode: String,
    pub gcs_service_account_hint: String,
    pub gcs_has_service_account_json: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageBreadcrumb {
    pub label: String,
    pub prefix: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageEntry {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub key: String,
    pub prefix: String,
    pub size_bytes: u64,
    pub size_label: String,
    pub modified_label: String,
    pub icon: String,
}

/// Full detail for a single object, used by the file detail panel.
/// `created_label` is empty for providers that don't track creation
/// time (S3 exposes only last-modified). `download_url` is a short-
/// lived presigned/signed GET URL used for in-browser preview.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageObjectDetail {
    pub connection_id: String,
    pub key: String,
    pub name: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub size_label: String,
    pub created_label: String,
    pub updated_label: String,
    pub storage_location: String,
    pub download_url: String,
    /// "image" | "pdf" | "video" | "audio" | "text" | "none"
    pub preview_kind: String,
    /// Custom/user metadata; empty renders as "No metadata found".
    pub metadata: Vec<StorageMetadataEntry>,
    pub provider_label: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageMetadataEntry {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageCommandEntry {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub bucket: String,
    pub name: String,
    pub key: String,
    pub prefix: String,
    pub size_label: String,
    pub modified_label: String,
    pub location_label: String,
    pub command_value: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageListing {
    pub connection_id: String,
    pub connection_name: String,
    pub provider_label: String,
    pub connection_favicon_url: String,
    pub bucket: String,
    pub prefix: String,
    pub parent_prefix: String,
    pub path_label: String,
    pub breadcrumbs: Vec<StorageBreadcrumb>,
    pub entries: Vec<StorageEntry>,
    pub truncated: bool,
}

#[pocopine::server(public)]
pub async fn list_storage_connections() -> ServerResult<Vec<StorageConnectionSummary>> {
    server::list_connections()
}

#[pocopine::server(public)]
pub async fn get_storage_connection(connection_id: String) -> ServerResult<StorageConnectionEdit> {
    server::get_connection(&connection_id)
}

#[pocopine::server(public)]
pub async fn save_s3_storage_connection(
    input: S3ConnectionInput,
) -> ServerResult<Vec<StorageConnectionSummary>> {
    server::save_s3_connection(input).await
}

#[pocopine::server(public)]
pub async fn save_gcs_storage_connection(
    input: GcsConnectionInput,
) -> ServerResult<Vec<StorageConnectionSummary>> {
    server::save_gcs_connection(input).await
}

#[pocopine::server(public)]
pub async fn delete_storage_connection(
    connection_id: String,
) -> ServerResult<Vec<StorageConnectionSummary>> {
    server::delete_connection(&connection_id)
}

#[pocopine::server(public)]
pub async fn browse_storage_connection(
    connection_id: String,
    prefix: String,
) -> ServerResult<StorageListing> {
    server::browse_connection(&connection_id, &prefix).await
}

#[pocopine::server(public)]
pub async fn get_storage_object_detail(
    connection_id: String,
    key: String,
) -> ServerResult<StorageObjectDetail> {
    server::object_detail(&connection_id, &key).await
}

#[pocopine::server(public)]
pub async fn list_storage_object_commands(
    connection_id: String,
) -> ServerResult<Vec<StorageCommandEntry>> {
    server::list_object_commands(&connection_id).await
}

#[pocopine::server(public)]
pub async fn create_storage_folder(
    connection_id: String,
    parent_prefix: String,
    folder_name: String,
) -> ServerResult<StorageListing> {
    server::create_folder(&connection_id, &parent_prefix, &folder_name).await
}

#[pocopine::server(public)]
pub async fn get_storage_browser_config() -> ServerResult<StorageBrowserConfigEdit> {
    server::get_app_config()
}

#[pocopine::server(public)]
pub async fn save_storage_browser_config(
    input: StorageBrowserConfigInput,
) -> ServerResult<StorageBrowserConfigEdit> {
    server::save_app_config(input)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn storage_server() -> pocopine_storage::StorageResult<pocopine_storage::StorageServer> {
    server::storage_server()
}

#[cfg(not(target_arch = "wasm32"))]
mod server;
