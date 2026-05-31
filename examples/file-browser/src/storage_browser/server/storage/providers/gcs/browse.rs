use bytes::Bytes;
use google_cloud_auth::credentials::{
    service_account::Builder as ServiceAccountCredentials, Builder as GoogleCredentialsBuilder,
};
use google_cloud_storage::builder::storage::SignedUrlBuilder;
use google_cloud_storage::http::Method;
use pocopine::{ServerError, ServerResult};
use serde::Deserialize;

use crate::storage_browser::server::storage::*;
use crate::storage_browser::{
    StorageCommandEntry, StorageEntry, StorageListing, StorageMetadataEntry, StorageObjectDetail,
};

pub(crate) async fn browse_gcs(
    connection: &SavedGcsConnection,
    prefix: &str,
) -> ServerResult<StorageListing> {
    let mut relative_prefix = normalize_prefix(prefix);
    if is_internal_storage_key(&relative_prefix) {
        relative_prefix.clear();
    }
    let effective_prefix = join_prefixes(&connection.root_prefix, &relative_prefix);
    let output = list_gcs_page(connection, &effective_prefix, Some("/"), None, 1000).await?;

    let mut entries = Vec::new();
    for full_prefix in output.prefixes.iter() {
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

    for object in output.objects.iter() {
        let full_key = object.name.as_str();
        if full_key == effective_prefix {
            continue;
        }
        let relative_key = strip_root_prefix(full_key, &connection.root_prefix);
        if relative_key.is_empty()
            || relative_key.ends_with('/')
            || is_internal_storage_key(relative_key)
        {
            continue;
        }
        let size = object.size.max(0) as u64;
        entries.push(StorageEntry {
            id: format!("object:{relative_key}"),
            kind: "object".to_string(),
            name: object_leaf(relative_key),
            key: relative_key.to_string(),
            prefix: relative_prefix.clone(),
            size_bytes: size,
            size_label: crate::format_size(size),
            modified_label: gcs_modified_label(object.update_time.as_deref()),
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
        provider_label: provider_label("gcs").to_string(),
        connection_favicon_url: connection_favicon_url("gcs", &connection.endpoint_url),
        bucket: connection.bucket.clone(),
        prefix: relative_prefix.clone(),
        parent_prefix: parent_prefix(&relative_prefix),
        path_label: path_label(&relative_prefix),
        breadcrumbs: breadcrumbs(&relative_prefix),
        entries,
        truncated: !output.next_page_token.is_empty(),
    })
}

pub(crate) async fn object_detail_gcs(
    connection: &SavedGcsConnection,
    relative_key: &str,
) -> ServerResult<StorageObjectDetail> {
    let full_key = join_object_key(&connection.root_prefix, relative_key);
    let (content_type, size, created_label, updated_label, metadata, download_url) =
        if connection.use_emulator {
            let object_path = gcs_json_object_url(connection, &full_key)?;
            let response = reqwest::Client::new()
                .get(&object_path)
                .send()
                .await
                .map_err(|err| gcs_error("get object metadata", err))?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(ServerError::App(format!(
                    "get object metadata: HTTP {status}: {body}"
                )));
            }
            let object = response
                .json::<GcsJsonObject>()
                .await
                .map_err(|err| gcs_error("decode object metadata", err))?;
            let size = gcs_json_size(&object.size).max(0) as u64;
            let metadata = object
                .metadata
                .into_iter()
                .map(|(key, value)| StorageMetadataEntry { key, value })
                .collect::<Vec<_>>();
            (
                object.content_type,
                size,
                object.time_created,
                object.updated,
                metadata,
                format!("{object_path}?alt=media"),
            )
        } else {
            let object = gcs_control_client(connection)
                .await?
                .get_object()
                .set_bucket(gcs_bucket_resource(&connection.bucket))
                .set_object(full_key.clone())
                .send()
                .await
                .map_err(|err| gcs_error("get object metadata", err))?;
            let size = object.size.max(0) as u64;
            let metadata = object
                .metadata
                .into_iter()
                .map(|(key, value)| StorageMetadataEntry { key, value })
                .collect::<Vec<_>>();
            (
                object.content_type,
                size,
                object.create_time.map(String::from).unwrap_or_default(),
                object.update_time.map(String::from).unwrap_or_default(),
                metadata,
                gcs_download_url(connection, &full_key).await?,
            )
        };

    Ok(StorageObjectDetail {
        connection_id: connection.id.clone(),
        key: relative_key.to_string(),
        name: object_leaf(relative_key),
        preview_kind: preview_kind_for(&content_type, relative_key),
        content_type,
        size_bytes: size,
        size_label: if size > 0 {
            crate::format_size(size)
        } else {
            String::new()
        },
        created_label,
        updated_label,
        storage_location: format!("gs://{}/{full_key}", connection.bucket),
        download_url,
        metadata,
        provider_label: provider_label("gcs").to_string(),
    })
}

pub(crate) async fn append_gcs_object_commands(
    connection: &SavedGcsConnection,
    entries: &mut Vec<StorageCommandEntry>,
) -> ServerResult<()> {
    let effective_prefix = normalize_prefix(&connection.root_prefix);
    let mut page_token = None::<String>;

    loop {
        let output = list_gcs_page(
            connection,
            &effective_prefix,
            None,
            page_token.as_deref(),
            1000,
        )
        .await?;

        for object in output.objects.iter() {
            let full_key = object.name.as_str();
            if full_key == effective_prefix || full_key.ends_with('/') {
                continue;
            }
            let relative_key = strip_root_prefix(full_key, &connection.root_prefix);
            if relative_key.is_empty() || is_internal_storage_key(relative_key) {
                continue;
            }
            let size = object.size.max(0) as u64;
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
                modified_label: gcs_modified_label(object.update_time.as_deref()),
                location_label,
                command_value: format!(
                    "{} {} {} {}",
                    name, relative_key, connection.bucket, connection.name
                ),
            });
        }

        page_token = if output.next_page_token.is_empty() {
            None
        } else {
            Some(output.next_page_token)
        };
        if page_token.is_none() || entries.len() >= 3000 {
            break;
        }
    }

    Ok(())
}

pub(crate) async fn create_gcs_folder(
    connection: &SavedGcsConnection,
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
    gcs_storage_client(connection)
        .await?
        .write_object(
            gcs_bucket_resource(&connection.bucket),
            full_key,
            Bytes::new(),
        )
        .set_content_type("application/x-directory")
        .send_unbuffered()
        .await
        .map(|_| ())
        .map_err(|err| gcs_error("create folder", err))
}

#[derive(Clone, Debug, Default)]
pub(crate) struct GcsListPage {
    pub(crate) objects: Vec<GcsListedObject>,
    pub(crate) prefixes: Vec<String>,
    pub(crate) next_page_token: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct GcsListedObject {
    pub(crate) name: String,
    pub(crate) size: i64,
    pub(crate) update_time: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct GcsJsonListResponse {
    #[serde(default)]
    items: Vec<GcsJsonObject>,
    #[serde(default)]
    prefixes: Vec<String>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: String,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct GcsJsonObject {
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: serde_json::Value,
    #[serde(default)]
    updated: String,
    #[serde(default, rename = "timeCreated")]
    time_created: String,
    #[serde(default, rename = "contentType")]
    content_type: String,
    #[serde(default)]
    metadata: std::collections::BTreeMap<String, String>,
}

pub(crate) async fn list_gcs_page(
    connection: &SavedGcsConnection,
    prefix: &str,
    delimiter: Option<&str>,
    page_token: Option<&str>,
    page_size: i32,
) -> ServerResult<GcsListPage> {
    if connection.use_emulator {
        return list_gcs_json_page(connection, prefix, delimiter, page_token, page_size).await;
    }

    let mut request = gcs_control_client(connection)
        .await?
        .list_objects()
        .set_parent(gcs_bucket_resource(&connection.bucket))
        .set_prefix(prefix.to_string())
        .set_page_size(page_size);
    if let Some(delimiter) = delimiter {
        request = request.set_delimiter(delimiter.to_string());
    }
    if let Some(page_token) = page_token {
        request = request.set_page_token(page_token.to_string());
    }
    let output = request
        .send()
        .await
        .map_err(|err| gcs_error("list objects", err))?;

    Ok(GcsListPage {
        objects: output
            .objects
            .into_iter()
            .map(|object| GcsListedObject {
                name: object.name,
                size: object.size,
                update_time: object.update_time.map(String::from),
            })
            .collect(),
        prefixes: output.prefixes,
        next_page_token: output.next_page_token,
    })
}

pub(crate) async fn list_gcs_json_page(
    connection: &SavedGcsConnection,
    prefix: &str,
    delimiter: Option<&str>,
    page_token: Option<&str>,
    page_size: i32,
) -> ServerResult<GcsListPage> {
    let endpoint = connection.endpoint_url.trim().trim_end_matches('/');
    if endpoint.is_empty() {
        return Err(ServerError::App(
            "GCS emulator endpoint is required".to_string(),
        ));
    }

    let url = format!(
        "{endpoint}/storage/v1/b/{}/o",
        encode_uri_component(&connection.bucket)
    );
    let mut query = vec![
        ("prefix", prefix.to_string()),
        ("maxResults", page_size.to_string()),
    ];
    if let Some(delimiter) = delimiter {
        query.push(("delimiter", delimiter.to_string()));
    }
    if let Some(page_token) = page_token {
        query.push(("pageToken", page_token.to_string()));
    }

    let response = reqwest::Client::new()
        .get(url)
        .query(&query)
        .send()
        .await
        .map_err(|err| gcs_error("list objects", err))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ServerError::App(format!(
            "list objects: HTTP {status}: {body}"
        )));
    }
    let output = response
        .json::<GcsJsonListResponse>()
        .await
        .map_err(|err| gcs_error("decode object list", err))?;

    Ok(GcsListPage {
        objects: output
            .items
            .into_iter()
            .map(|object| {
                let update_time = if object.updated.is_empty() {
                    object.time_created
                } else {
                    object.updated
                };
                GcsListedObject {
                    name: object.name,
                    size: gcs_json_size(&object.size),
                    update_time: if update_time.is_empty() {
                        None
                    } else {
                        Some(update_time)
                    },
                }
            })
            .collect(),
        prefixes: output.prefixes,
        next_page_token: output.next_page_token,
    })
}

pub(crate) fn gcs_json_object_url(
    connection: &SavedGcsConnection,
    object_key: &str,
) -> ServerResult<String> {
    let base = if connection.endpoint_url.trim().is_empty() {
        "https://storage.googleapis.com".to_string()
    } else {
        connection
            .endpoint_url
            .trim()
            .trim_end_matches('/')
            .to_string()
    };
    Ok(format!(
        "{base}/storage/v1/b/{}/o/{}",
        encode_uri_component(&connection.bucket),
        encode_uri_component(object_key)
    ))
}

pub(crate) async fn gcs_download_url(
    connection: &SavedGcsConnection,
    object_key: &str,
) -> ServerResult<String> {
    match connection.effective_auth() {
        SavedGcsAuth::Anonymous => Ok(format!(
            "{}?alt=media",
            gcs_json_object_url(connection, object_key)?
        )),
        SavedGcsAuth::ApplicationDefault => {
            let signer = GoogleCredentialsBuilder::default()
                .build_signer()
                .map_err(|err| gcs_error("build GCS signer", err))?;
            gcs_signed_download_url(connection, object_key, &signer).await
        }
        SavedGcsAuth::ServiceAccountJson { ref json, .. } => {
            let signer = ServiceAccountCredentials::new(json.clone())
                .build_signer()
                .map_err(|err| gcs_error("build GCS service account signer", err))?;
            gcs_signed_download_url(connection, object_key, &signer).await
        }
    }
}

pub(crate) async fn gcs_signed_download_url(
    connection: &SavedGcsConnection,
    object_key: &str,
    signer: &google_cloud_auth::signer::Signer,
) -> ServerResult<String> {
    let mut builder =
        SignedUrlBuilder::for_object(gcs_bucket_resource(&connection.bucket), object_key)
            .with_method(Method::GET)
            .with_expiration(std::time::Duration::from_secs(15 * 60));
    if !connection.endpoint_url.trim().is_empty() {
        builder = builder.with_endpoint(connection.endpoint_url.trim().trim_end_matches('/'));
    }
    builder
        .sign_with(signer)
        .await
        .map_err(|err| gcs_error("sign GCS download URL", err))
}
