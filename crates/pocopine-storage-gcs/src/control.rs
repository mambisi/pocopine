use std::time::Duration;

use google_cloud_gax::options::RequestOptionsBuilder;
use google_cloud_storage::client::StorageControl;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use pocopine_storage::{StorageError, StorageResult};

use crate::layout::GcsKeyLayout;
use crate::util::{gcs_error, is_gcs_not_found};

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
    utf8_percent_encode(value, GCS_JSON_PATH_ENCODE_SET).to_string()
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
