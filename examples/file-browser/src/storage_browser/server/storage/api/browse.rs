use pocopine::{ServerError, ServerResult};

use crate::storage_browser::server::storage::*;
use crate::storage_browser::{StorageCommandEntry, StorageListing, StorageObjectDetail};

pub(crate) async fn browse_connection(
    connection_id: &str,
    prefix: &str,
) -> ServerResult<StorageListing> {
    let config = load_config()?;
    let connection = config
        .connections
        .iter()
        .find(|connection| connection.id() == connection_id)
        .ok_or_else(|| ServerError::App("storage connection not found".to_string()))?;

    match connection {
        SavedConnection::S3(connection) => browse_s3(connection, prefix).await,
        SavedConnection::Gcs(connection) => browse_gcs(connection, prefix).await,
    }
}

pub(crate) async fn object_detail(
    connection_id: &str,
    key: &str,
) -> ServerResult<StorageObjectDetail> {
    let config = load_config()?;
    let connection = config
        .connections
        .iter()
        .find(|connection| connection.id() == connection_id)
        .ok_or_else(|| ServerError::App("storage connection not found".to_string()))?;

    match connection {
        SavedConnection::S3(connection) => object_detail_s3(connection, key).await,
        SavedConnection::Gcs(connection) => object_detail_gcs(connection, key).await,
    }
}

pub(crate) async fn list_object_commands(
    connection_id: &str,
) -> ServerResult<Vec<StorageCommandEntry>> {
    let config = load_config()?;
    let mut entries = Vec::new();

    for connection in config.connections.iter() {
        if !connection_id.trim().is_empty() && connection.id() != connection_id {
            continue;
        }
        match connection {
            SavedConnection::S3(connection) => {
                append_s3_object_commands(connection, &mut entries).await?;
            }
            SavedConnection::Gcs(connection) => {
                append_gcs_object_commands(connection, &mut entries).await?;
            }
        }
    }

    entries.sort_by(|left, right| {
        left.connection_name
            .to_lowercase()
            .cmp(&right.connection_name.to_lowercase())
            .then_with(|| left.bucket.to_lowercase().cmp(&right.bucket.to_lowercase()))
            .then_with(|| left.key.to_lowercase().cmp(&right.key.to_lowercase()))
    });
    Ok(entries)
}

pub(crate) async fn create_folder(
    connection_id: &str,
    parent_prefix: &str,
    folder_name: &str,
) -> ServerResult<StorageListing> {
    let config = load_config()?;
    let connection = config
        .connections
        .iter()
        .find(|connection| connection.id() == connection_id)
        .ok_or_else(|| ServerError::App("storage connection not found".to_string()))?;

    match connection {
        SavedConnection::S3(connection) => {
            create_s3_folder(connection, parent_prefix, folder_name).await?;
            browse_s3(connection, parent_prefix).await
        }
        SavedConnection::Gcs(connection) => {
            create_gcs_folder(connection, parent_prefix, folder_name).await?;
            browse_gcs(connection, parent_prefix).await
        }
    }
}
