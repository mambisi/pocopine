use pocopine::{ServerError, ServerResult};

use crate::storage_browser::server::storage::*;
use crate::storage_browser::{
    GcsConnectionInput, S3ConnectionInput, StorageConnectionEdit, StorageConnectionSummary,
};

pub(crate) fn list_connections() -> ServerResult<Vec<StorageConnectionSummary>> {
    Ok(load_config()?
        .connections
        .iter()
        .map(SavedConnection::summary)
        .collect())
}

pub(crate) fn get_connection(connection_id: &str) -> ServerResult<StorageConnectionEdit> {
    load_config()?
        .connections
        .iter()
        .find(|connection| connection.id() == connection_id)
        .map(SavedConnection::edit)
        .ok_or_else(|| ServerError::App("storage connection not found".to_string()))
}

pub(crate) async fn save_s3_connection(
    input: S3ConnectionInput,
) -> ServerResult<Vec<StorageConnectionSummary>> {
    let mut config = load_config()?;
    let existing_secret = config.connections.iter().find_map(|saved| match saved {
        SavedConnection::S3(existing) if existing.id == input.id => {
            Some(existing.secret_access_key.clone())
        }
        _ => None,
    });
    let connection = SavedS3Connection::from_input(input, existing_secret.as_deref())?;
    let client = s3_client(&connection);
    ensure_bucket(
        &client,
        &connection.bucket,
        connection.create_bucket_if_requested(),
    )
    .await?;

    config
        .connections
        .retain(|saved| saved.id() != connection.id.as_str());
    config.connections.push(SavedConnection::S3(connection));
    config
        .connections
        .sort_by(|left, right| left.summary().name.cmp(&right.summary().name));
    save_config(&config)?;
    list_connections()
}

pub(crate) async fn save_gcs_connection(
    input: GcsConnectionInput,
) -> ServerResult<Vec<StorageConnectionSummary>> {
    let mut config = load_config()?;
    let existing_auth = config.connections.iter().find_map(|saved| match saved {
        SavedConnection::Gcs(existing) if existing.id == input.id => {
            Some(existing.effective_auth())
        }
        _ => None,
    });
    let connection = SavedGcsConnection::from_input(input, existing_auth)?;
    ensure_gcs_bucket(&connection).await?;

    config
        .connections
        .retain(|saved| saved.id() != connection.id.as_str());
    config.connections.push(SavedConnection::Gcs(connection));
    config
        .connections
        .sort_by(|left, right| left.summary().name.cmp(&right.summary().name));
    save_config(&config)?;
    list_connections()
}

pub(crate) fn delete_connection(
    connection_id: &str,
) -> ServerResult<Vec<StorageConnectionSummary>> {
    let mut config = load_config()?;
    let before = config.connections.len();
    config
        .connections
        .retain(|connection| connection.id() != connection_id);
    if config.connections.len() != before {
        save_config(&config)?;
    }
    list_connections()
}
