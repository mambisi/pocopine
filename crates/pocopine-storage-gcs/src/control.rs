use std::collections::BTreeMap;
use std::time::Duration;

use google_cloud_gax::options::RequestOptionsBuilder;
use google_cloud_storage::client::StorageControl;
use google_cloud_storage::model::Object;
use google_cloud_storage::model::compose_object_request::SourceObject;
use pocopine_codec::{AsciiSet, CONTROLS, percent_encode_set};
use pocopine_storage::{StorageError, StorageResult};

use crate::layout::GcsKeyLayout;
use crate::state::{GcsObjectAttrs, GcsObjectWrite};
use crate::util::{gcs_error, is_gcs_not_found, is_gcs_precondition_failed, non_empty};

/// One source component for a compose request.
pub(crate) struct ComposeSource {
    pub(crate) key: String,
    pub(crate) generation: Option<i64>,
}

/// A request to assemble `sources` into the destination object `dest_key`.
pub(crate) struct ComposeRequest<'a> {
    pub(crate) dest_key: &'a str,
    pub(crate) content_type: Option<&'a str>,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) sources: Vec<ComposeSource>,
    /// `Some(0)` refuses to overwrite an existing destination object.
    pub(crate) if_generation_match: Option<i64>,
}

const JSON_CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const GCS_JSON_PATH_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b'!')
    .add(b'#')
    .add(b'$')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'=')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b']');

#[derive(Clone)]
pub(crate) enum GcsControl {
    Google(StorageControl),
    Json(GcsJsonControl),
}

impl GcsControl {
    pub(crate) async fn delete_object(
        &self,
        layout: &GcsKeyLayout,
        key: &str,
    ) -> StorageResult<()> {
        match self {
            Self::Google(control) => delete_object_with_google_control(control, layout, key).await,
            Self::Json(control) => control.delete_object(layout, key).await,
        }
    }

    pub(crate) async fn compose_object(
        &self,
        layout: &GcsKeyLayout,
        request: ComposeRequest<'_>,
    ) -> StorageResult<GcsObjectWrite> {
        match self {
            Self::Google(control) => compose_with_google_control(control, layout, request).await,
            Self::Json(control) => control.compose_object(layout, request).await,
        }
    }

    /// Fetch an object's etag/generation plus the value of its `marker_key`
    /// custom-metadata entry. `None` when the object does not exist.
    pub(crate) async fn get_object_attrs(
        &self,
        layout: &GcsKeyLayout,
        key: &str,
        marker_key: &str,
    ) -> StorageResult<Option<GcsObjectAttrs>> {
        match self {
            Self::Google(control) => {
                get_object_attrs_with_google_control(control, layout, key, marker_key).await
            }
            Self::Json(control) => control.get_object_attrs(layout, key, marker_key).await,
        }
    }
}

async fn get_object_attrs_with_google_control(
    control: &StorageControl,
    layout: &GcsKeyLayout,
    key: &str,
    marker_key: &str,
) -> StorageResult<Option<GcsObjectAttrs>> {
    match control
        .get_object()
        .set_bucket(layout.bucket_resource())
        .set_object(key)
        .send()
        .await
    {
        Ok(object) => Ok(Some(GcsObjectAttrs {
            etag: non_empty(object.etag),
            generation: if object.generation > 0 {
                Some(object.generation)
            } else {
                None
            },
            owner_session: object.metadata.get(marker_key).cloned(),
        })),
        Err(err) if is_gcs_not_found(&err) => Ok(None),
        Err(err) => Err(gcs_error("get object", err)),
    }
}

async fn compose_with_google_control(
    control: &StorageControl,
    layout: &GcsKeyLayout,
    request: ComposeRequest<'_>,
) -> StorageResult<GcsObjectWrite> {
    let mut destination = Object::new()
        .set_bucket(layout.bucket_resource())
        .set_name(request.dest_key)
        .set_metadata(request.metadata);
    if let Some(content_type) = request.content_type {
        destination = destination.set_content_type(content_type);
    }
    let sources: Vec<SourceObject> = request
        .sources
        .iter()
        .map(|source| {
            let builder = SourceObject::new().set_name(source.key.clone());
            match source.generation {
                Some(generation) => builder.set_generation(generation),
                None => builder,
            }
        })
        .collect();
    let mut builder = control
        .compose_object()
        .set_destination(destination)
        .set_source_objects(sources);
    if let Some(generation) = request.if_generation_match {
        builder = builder.set_if_generation_match(generation);
    }
    let object = builder.send().await.map_err(|err| {
        if is_gcs_precondition_failed(&err) {
            StorageError::policy_rejected("GCS compose precondition failed")
        } else {
            gcs_error("compose object", err)
        }
    })?;
    Ok(GcsObjectWrite {
        etag: non_empty(object.etag),
        generation: if object.generation > 0 {
            Some(object.generation.to_string())
        } else {
            None
        },
        generation_match: if object.generation > 0 {
            Some(object.generation)
        } else {
            None
        },
    })
}

#[derive(Clone)]
pub(crate) struct GcsJsonControl {
    endpoint: String,
    http: reqwest::Client,
}

impl GcsJsonControl {
    pub(crate) fn new(endpoint: String) -> StorageResult<Self> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        if endpoint.is_empty() {
            return Err(StorageError::policy_rejected(
                "GCS emulator endpoint must not be empty",
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(JSON_CONTROL_TIMEOUT)
            .build()
            .map_err(|err| {
                StorageError::backend(format!("build GCS JSON control client: {err}"))
            })?;
        Ok(Self { endpoint, http })
    }

    async fn delete_object(&self, layout: &GcsKeyLayout, key: &str) -> StorageResult<()> {
        let url = format!(
            "{}/storage/v1/b/{}/o/{}",
            self.endpoint,
            encode_uri_component(layout.bucket()),
            encode_uri_component(key)
        );
        let response = self
            .http
            .delete(url)
            .send()
            .await
            .map_err(|err| gcs_error("delete object", err))?;
        if response.status().as_u16() == 404 {
            return Err(StorageError::unknown_upload_session(key.to_string()));
        }
        if !response.status().is_success() {
            return Err(StorageError::backend(format!(
                "GCS delete object: HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    async fn compose_object(
        &self,
        layout: &GcsKeyLayout,
        request: ComposeRequest<'_>,
    ) -> StorageResult<GcsObjectWrite> {
        let url = format!(
            "{}/storage/v1/b/{}/o/{}/compose",
            self.endpoint,
            encode_uri_component(layout.bucket()),
            encode_uri_component(request.dest_key)
        );
        let mut destination = serde_json::Map::new();
        if let Some(content_type) = request.content_type {
            destination.insert("contentType".to_string(), content_type.into());
        }
        if !request.metadata.is_empty() {
            destination.insert(
                "metadata".to_string(),
                serde_json::to_value(&request.metadata)
                    .map_err(|err| gcs_error("encode compose metadata", err))?,
            );
        }
        let source_objects: Vec<serde_json::Value> = request
            .sources
            .iter()
            .map(|source| {
                let mut entry = serde_json::Map::new();
                entry.insert("name".to_string(), source.key.clone().into());
                if let Some(generation) = source.generation {
                    entry.insert("generation".to_string(), generation.into());
                }
                serde_json::Value::Object(entry)
            })
            .collect();
        let body = serde_json::json!({
            "sourceObjects": source_objects,
            "destination": serde_json::Value::Object(destination),
        });

        let mut builder = self.http.post(url).json(&body);
        if let Some(generation) = request.if_generation_match {
            builder = builder.query(&[("ifGenerationMatch", generation.to_string())]);
        }
        let response = builder
            .send()
            .await
            .map_err(|err| gcs_error("compose object", err))?;
        let status = response.status();
        if status.as_u16() == 412 {
            return Err(StorageError::policy_rejected(
                "GCS compose precondition failed",
            ));
        }
        if !status.is_success() {
            return Err(StorageError::backend(format!(
                "GCS compose object: HTTP {status}"
            )));
        }
        let object: ComposedObject = response
            .json()
            .await
            .map_err(|err| gcs_error("decode composed object", err))?;
        let generation = object
            .generation
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|generation| *generation > 0);
        Ok(GcsObjectWrite {
            etag: object.etag.and_then(non_empty),
            generation: generation.map(|value| value.to_string()),
            generation_match: generation,
        })
    }
}

impl GcsJsonControl {
    async fn get_object_attrs(
        &self,
        layout: &GcsKeyLayout,
        key: &str,
        marker_key: &str,
    ) -> StorageResult<Option<GcsObjectAttrs>> {
        let url = format!(
            "{}/storage/v1/b/{}/o/{}",
            self.endpoint,
            encode_uri_component(layout.bucket()),
            encode_uri_component(key)
        );
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|err| gcs_error("get object", err))?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(StorageError::backend(format!(
                "GCS get object: HTTP {}",
                response.status()
            )));
        }
        let object: FetchedObject = response
            .json()
            .await
            .map_err(|err| gcs_error("decode object metadata", err))?;
        let generation = object
            .generation
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|generation| *generation > 0);
        Ok(Some(GcsObjectAttrs {
            etag: object.etag.and_then(non_empty),
            generation,
            owner_session: object
                .metadata
                .and_then(|metadata| metadata.get(marker_key).cloned()),
        }))
    }
}

#[derive(serde::Deserialize)]
struct ComposedObject {
    #[serde(default)]
    generation: Option<String>,
    #[serde(default)]
    etag: Option<String>,
}

#[derive(serde::Deserialize)]
struct FetchedObject {
    #[serde(default)]
    generation: Option<String>,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    metadata: Option<BTreeMap<String, String>>,
}

async fn delete_object_with_google_control(
    control: &StorageControl,
    layout: &GcsKeyLayout,
    key: &str,
) -> StorageResult<()> {
    control
        .delete_object()
        .set_bucket(layout.bucket_resource())
        .set_object(key)
        .with_idempotency(true)
        .send()
        .await
        .map_err(|err| {
            if is_gcs_not_found(&err) {
                StorageError::unknown_upload_session(key.to_string())
            } else {
                gcs_error("delete object", err)
            }
        })?;
    Ok(())
}

fn encode_uri_component(value: &str) -> String {
    percent_encode_set(value, GCS_JSON_PATH_ENCODE_SET)
}

#[cfg(test)]
mod tests {
    use super::encode_uri_component;

    #[test]
    fn json_control_uri_encoding_preserves_unreserved_chars() {
        assert_eq!(
            encode_uri_component("tenant-a/file.name_~"),
            "tenant-a%2Ffile.name_~"
        );
    }
}
