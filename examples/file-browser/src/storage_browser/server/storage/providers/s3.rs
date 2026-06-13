use aws_sdk_s3::Client;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use pocopine::{ServerError, ServerResult};
use pocopine_storage::StorageResult;
use pocopine_storage_s3::S3StorageBackend;

use crate::storage_browser::server::storage::*;
use crate::storage_browser::{
    StorageCommandEntry, StorageEntry, StorageListing, StorageMetadataEntry, StorageObjectDetail,
};

pub(crate) fn s3_client(connection: &SavedS3Connection) -> Client {
    let mut config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(Region::new(connection.region.clone()))
        .credentials_provider(Credentials::new(
            connection.access_key_id.clone(),
            connection.secret_access_key.clone(),
            None,
            None,
            "pocopine-storage-browser",
        ))
        .force_path_style(connection.force_path_style);
    if !connection.endpoint_url.is_empty() {
        config = config.endpoint_url(connection.endpoint_url.clone());
    }
    Client::from_conf(config.build())
}

pub(crate) async fn ensure_bucket(
    client: &Client,
    bucket: &str,
    create_if_missing: bool,
) -> ServerResult<()> {
    match client.head_bucket().bucket(bucket).send().await {
        Ok(_) => Ok(()),
        Err(head_err) if create_if_missing => client
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|err| s3_error("create bucket", err)),
        Err(err) => {
            let context = head_err_message(&err);
            Err(s3_error("check bucket", err).or_else_with_context(context))
        }
    }
}

pub(crate) async fn browse_s3(
    connection: &SavedS3Connection,
    prefix: &str,
) -> ServerResult<StorageListing> {
    let mut relative_prefix = normalize_prefix(prefix);
    if is_internal_storage_key(&relative_prefix) {
        relative_prefix.clear();
    }
    let effective_prefix = join_prefixes(&connection.root_prefix, &relative_prefix);
    let output = s3_client(connection)
        .list_objects_v2()
        .bucket(&connection.bucket)
        .prefix(effective_prefix.clone())
        .delimiter("/")
        .max_keys(1000)
        .send()
        .await
        .map_err(|err| s3_error("list objects", err))?;

    let mut entries = Vec::new();
    for common in output.common_prefixes() {
        let Some(full_prefix) = common.prefix() else {
            continue;
        };
        let relative = strip_root_prefix(full_prefix, &connection.root_prefix);
        if is_internal_storage_key(relative) {
            continue;
        }
        let name = prefix_leaf(relative);
        entries.push(StorageEntry {
            id: format!("folder:{relative}"),
            kind: "folder".to_string(),
            name,
            key: String::new(),
            prefix: relative.to_string(),
            size_bytes: 0,
            size_label: String::new(),
            modified_label: String::new(),
            icon: "folder-open".to_string(),
        });
    }

    for object in output.contents() {
        let Some(full_key) = object.key() else {
            continue;
        };
        if full_key == effective_prefix {
            continue;
        }
        let relative_key = strip_root_prefix(full_key, &connection.root_prefix);
        if relative_key.is_empty() || is_internal_storage_key(relative_key) {
            continue;
        }
        let size = object.size().unwrap_or_default().max(0) as u64;
        entries.push(StorageEntry {
            id: format!("object:{relative_key}"),
            kind: "object".to_string(),
            name: object_leaf(relative_key),
            key: relative_key.to_string(),
            prefix: relative_prefix.clone(),
            size_bytes: size,
            size_label: crate::format_size(size),
            modified_label: object
                .last_modified()
                .map(|modified| modified.to_string())
                .unwrap_or_default(),
            icon: "file".to_string(),
        });
    }

    entries.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });

    Ok(StorageListing {
        connection_id: connection.id.clone(),
        connection_name: connection.name.clone(),
        provider_label: provider_label("s3").to_string(),
        connection_favicon_url: connection_favicon_url("s3", &connection.endpoint_url),
        bucket: connection.bucket.clone(),
        prefix: relative_prefix.clone(),
        parent_prefix: parent_prefix(&relative_prefix),
        path_label: path_label(&relative_prefix),
        breadcrumbs: breadcrumbs(&relative_prefix),
        entries,
        truncated: output.is_truncated().unwrap_or(false),
    })
}

pub(crate) async fn object_detail_s3(
    connection: &SavedS3Connection,
    relative_key: &str,
) -> ServerResult<StorageObjectDetail> {
    let client = s3_client(connection);
    let full_key = join_object_key(&connection.root_prefix, relative_key);

    let head = client
        .head_object()
        .bucket(&connection.bucket)
        .key(&full_key)
        .send()
        .await
        .map_err(|err| s3_error("head object", err))?;

    let size = head.content_length().unwrap_or_default().max(0) as u64;
    let content_type = head.content_type().unwrap_or_default().to_string();
    let updated_label = head
        .last_modified()
        .map(|modified| modified.to_string())
        .unwrap_or_default();
    let metadata = head
        .metadata()
        .map(|map| {
            map.iter()
                .map(|(key, value)| StorageMetadataEntry {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    // Short-lived presigned GET URL for in-browser preview / download.
    let presign = aws_sdk_s3::presigning::PresigningConfig::expires_in(
        std::time::Duration::from_secs(15 * 60),
    )
    .map_err(|err| s3_error("build presign config", err))?;
    let presigned = client
        .get_object()
        .bucket(&connection.bucket)
        .key(&full_key)
        .presigned(presign)
        .await
        .map_err(|err| s3_error("presign get object", err))?;
    let download_url = presigned.uri().to_string();

    Ok(StorageObjectDetail {
        connection_id: connection.id.clone(),
        key: relative_key.to_string(),
        name: object_leaf(relative_key),
        preview_kind: preview_kind_for(&content_type, relative_key),
        content_type,
        size_bytes: size,
        size_label: crate::format_size(size),
        created_label: String::new(),
        updated_label,
        storage_location: format!("s3://{}/{full_key}", connection.bucket),
        download_url,
        metadata,
        provider_label: provider_label("s3").to_string(),
    })
}

pub(crate) async fn append_s3_object_commands(
    connection: &SavedS3Connection,
    entries: &mut Vec<StorageCommandEntry>,
) -> ServerResult<()> {
    let client = s3_client(connection);
    let effective_prefix = normalize_prefix(&connection.root_prefix);
    let mut continuation_token = None::<String>;

    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(&connection.bucket)
            .prefix(effective_prefix.clone())
            .max_keys(1000);
        if let Some(token) = continuation_token.as_deref() {
            request = request.continuation_token(token);
        }

        let output = request
            .send()
            .await
            .map_err(|err| s3_error("list command objects", err))?;

        for object in output.contents() {
            let Some(full_key) = object.key() else {
                continue;
            };
            if full_key == effective_prefix || full_key.ends_with('/') {
                continue;
            }
            let relative_key = strip_root_prefix(full_key, &connection.root_prefix);
            if relative_key.is_empty() || is_internal_storage_key(relative_key) {
                continue;
            }
            let size = object.size().unwrap_or_default().max(0) as u64;
            let name = object_leaf(relative_key);
            let prefix = parent_prefix(relative_key);
            let location_label = if prefix.is_empty() {
                format!("{} · {}", connection.name, connection.bucket)
            } else {
                format!("{} · {}/{}", connection.name, connection.bucket, prefix)
            };
            entries.push(StorageCommandEntry {
                id: format!("{}:{relative_key}", connection.id),
                connection_id: connection.id.clone(),
                connection_name: connection.name.clone(),
                bucket: connection.bucket.clone(),
                name: name.clone(),
                key: relative_key.to_string(),
                prefix,
                size_label: crate::format_size(size),
                modified_label: object
                    .last_modified()
                    .map(|modified| modified.to_string())
                    .unwrap_or_default(),
                location_label,
                command_value: format!(
                    "{} {} {} {}",
                    name, relative_key, connection.bucket, connection.name
                ),
            });
        }

        continuation_token = output.next_continuation_token().map(ToString::to_string);
        if !output.is_truncated().unwrap_or(false)
            || continuation_token.is_none()
            || entries.len() >= 3000
        {
            break;
        }
    }

    Ok(())
}

pub(crate) async fn create_s3_folder(
    connection: &SavedS3Connection,
    parent_prefix: &str,
    folder_name: &str,
) -> ServerResult<()> {
    let parent_prefix = normalize_prefix(parent_prefix);
    if is_internal_storage_key(&parent_prefix) {
        return Err(ServerError::App(
            "cannot create folders under Pocopine internal storage".to_string(),
        ));
    }
    let folder_name = sanitize_folder_name(folder_name)?;
    let relative_key = format!("{parent_prefix}{folder_name}/");
    if is_internal_storage_key(&relative_key) {
        return Err(ServerError::App(
            "folder name is reserved for Pocopine internals".to_string(),
        ));
    }
    let full_key = join_prefixes(&connection.root_prefix, &relative_key);
    s3_client(connection)
        .put_object()
        .bucket(&connection.bucket)
        .key(full_key)
        .content_type("application/x-directory")
        .body(ByteStream::from(Vec::new()))
        .send()
        .await
        .map(|_| ())
        .map_err(|err| s3_error("create folder", err))
}

impl SavedS3Connection {
    pub(crate) fn upload_backend(
        &self,
        max_proxy_upload_bytes: u64,
    ) -> StorageResult<S3StorageBackend> {
        S3StorageBackend::named(UPLOAD_BACKEND, s3_client(self), self.bucket.clone())?
            .with_prefix(self.root_prefix.clone())?
            .with_max_proxy_upload_bytes(max_proxy_upload_bytes)
    }
}
