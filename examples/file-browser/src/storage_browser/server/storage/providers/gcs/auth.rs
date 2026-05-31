use percent_encoding::{AsciiSet, CONTROLS};
use pocopine::{ServerError, ServerResult};
use serde::{Deserialize, Serialize};

use crate::storage_browser::GcsConnectionInput;

pub(crate) const GCS_JSON_PATH_ENCODE_SET: &AsciiSet = &CONTROLS
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum SavedGcsAuth {
    ApplicationDefault,
    Anonymous,
    ServiceAccountJson {
        json: serde_json::Value,
        client_email: String,
        project_id_hint: String,
    },
}

impl Default for SavedGcsAuth {
    fn default() -> Self {
        Self::ApplicationDefault
    }
}

impl std::fmt::Debug for SavedGcsAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApplicationDefault => f.write_str("ApplicationDefault"),
            Self::Anonymous => f.write_str("Anonymous"),
            Self::ServiceAccountJson {
                client_email,
                project_id_hint,
                ..
            } => f
                .debug_struct("ServiceAccountJson")
                .field("client_email", client_email)
                .field("project_id_hint", project_id_hint)
                .field("json", &"<redacted>")
                .finish(),
        }
    }
}

impl SavedGcsAuth {
    pub(crate) fn mode(&self) -> &'static str {
        match self {
            Self::ApplicationDefault => "application_default",
            Self::Anonymous => "anonymous",
            Self::ServiceAccountJson { .. } => "service_account_json",
        }
    }

    pub(crate) fn hint(&self) -> String {
        match self {
            Self::ApplicationDefault => "ADC".to_string(),
            Self::Anonymous => "anonymous".to_string(),
            Self::ServiceAccountJson { client_email, .. } => client_email.clone(),
        }
    }

    pub(crate) fn has_service_account_json(&self) -> bool {
        matches!(self, Self::ServiceAccountJson { .. })
    }
}

pub(crate) fn gcs_auth_from_input(
    input: &GcsConnectionInput,
    existing_auth: Option<SavedGcsAuth>,
) -> ServerResult<SavedGcsAuth> {
    let mode = normalize_gcs_auth_mode(&input.auth_mode, input.use_anonymous_auth)?;
    match mode {
        "anonymous" => Ok(SavedGcsAuth::Anonymous),
        "application_default" => Ok(SavedGcsAuth::ApplicationDefault),
        "service_account_json" => {
            let json = input.service_account_json.trim();
            if json.is_empty() {
                match existing_auth {
                    Some(auth @ SavedGcsAuth::ServiceAccountJson { .. }) => Ok(auth),
                    _ => Err(ServerError::App(
                        "service account JSON is required".to_string(),
                    )),
                }
            } else {
                parse_service_account_json(json)
            }
        }
        _ => Err(ServerError::App("unsupported GCS auth mode".to_string())),
    }
}

pub(crate) fn normalize_gcs_auth_mode(
    mode: &str,
    legacy_anonymous: bool,
) -> ServerResult<&'static str> {
    match mode.trim() {
        "" if legacy_anonymous => Ok("anonymous"),
        "" => Ok("application_default"),
        "anonymous" => Ok("anonymous"),
        "application_default" | "adc" => Ok("application_default"),
        "service_account_json" => Ok("service_account_json"),
        _ => Err(ServerError::App("unsupported GCS auth mode".to_string())),
    }
}

pub(crate) fn parse_service_account_json(raw: &str) -> ServerResult<SavedGcsAuth> {
    let json: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| ServerError::App(format!("invalid service account JSON: {err}")))?;
    let kind = service_account_field(&json, "type")?;
    if kind != "service_account" {
        return Err(ServerError::App(
            "service account JSON must have type \"service_account\"".to_string(),
        ));
    }
    let client_email = service_account_field(&json, "client_email")?.to_string();
    let private_key = service_account_field(&json, "private_key")?;
    if !private_key.contains("BEGIN PRIVATE KEY") {
        return Err(ServerError::App(
            "service account JSON private_key must be a PEM private key".to_string(),
        ));
    }
    service_account_field(&json, "private_key_id")?;
    let project_id_hint = service_account_field(&json, "project_id")?.to_string();

    Ok(SavedGcsAuth::ServiceAccountJson {
        json,
        client_email,
        project_id_hint,
    })
}

pub(crate) fn service_account_field<'a>(
    json: &'a serde_json::Value,
    field: &str,
) -> ServerResult<&'a str> {
    json.get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ServerError::App(format!("service account JSON missing {field}")))
}
