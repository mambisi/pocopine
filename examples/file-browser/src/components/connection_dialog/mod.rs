//! `<file-browser-connection-dialog>` — provider connection form and save flow.

use pine::{
    PineDialogClose, PineDialogContent, PineDialogDescription, PineDialogOverlay, PineDialogPortal,
    PineDialogRoot, PineDialogTitle,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

use crate::{
    GcsConnectionInput, S3ConnectionInput, StorageBrowserStore, StorageConnectionEdit,
    StorageConnectionSummary,
};

#[derive(Serialize, Deserialize)]
#[component(
    template = "FileBrowserConnectionDialog.poco",
    role = "panel",
    display = "contents",
    uses = [
        PineIcon,
        PineDialogRoot,
        PineDialogPortal,
        PineDialogOverlay,
        PineDialogContent,
        PineDialogTitle,
        PineDialogDescription,
        PineDialogClose,
    ]
)]
pub struct FileBrowserConnectionDialog {
    #[model]
    pub open: bool,
    #[prop]
    pub edit_connection_id: String,
    pub loading_connection: bool,
    pub saving_connection: bool,
    pub modal_error: String,
    pub connection_dialog_title: String,
    pub connection_dialog_description: String,
    pub connection_provider: String,
    pub connection_id: String,
    pub connection_name: String,
    pub connection_endpoint_url: String,
    pub connection_project_id: String,
    pub connection_region: String,
    pub connection_bucket: String,
    pub connection_root_prefix: String,
    pub connection_access_key_id: String,
    pub connection_secret_access_key: String,
    pub connection_force_path_style: bool,
    pub connection_create_bucket: bool,
    pub connection_use_emulator: bool,
    pub connection_use_anonymous_auth: bool,
    pub connection_gcs_auth_mode: String,
    pub connection_gcs_service_account_json: String,
    pub connection_gcs_service_account_hint: String,
    pub connection_gcs_has_service_account_json: bool,
}

impl Default for FileBrowserConnectionDialog {
    fn default() -> Self {
        let mut dialog = Self {
            open: false,
            edit_connection_id: String::new(),
            loading_connection: false,
            saving_connection: false,
            modal_error: String::new(),
            connection_dialog_title: String::new(),
            connection_dialog_description: String::new(),
            connection_provider: String::new(),
            connection_id: String::new(),
            connection_name: String::new(),
            connection_endpoint_url: String::new(),
            connection_project_id: String::new(),
            connection_region: String::new(),
            connection_bucket: String::new(),
            connection_root_prefix: String::new(),
            connection_access_key_id: String::new(),
            connection_secret_access_key: String::new(),
            connection_force_path_style: true,
            connection_create_bucket: true,
            connection_use_emulator: false,
            connection_use_anonymous_auth: false,
            connection_gcs_auth_mode: String::new(),
            connection_gcs_service_account_json: String::new(),
            connection_gcs_service_account_hint: String::new(),
            connection_gcs_has_service_account_json: false,
        };
        dialog.reset_to_minio_defaults();
        dialog
    }
}

impl FileBrowserConnectionDialog {
    fn prepare_open(&mut self) {
        self.modal_error.clear();
        self.saving_connection = false;
        let edit_connection_id = self.edit_connection_id.trim().to_string();
        if edit_connection_id.is_empty() {
            self.loading_connection = false;
            self.reset_to_minio_defaults();
            return;
        }
        if self.loading_connection && self.connection_id == edit_connection_id {
            return;
        }
        self.load_connection(edit_connection_id);
    }

    fn reset_to_minio_defaults(&mut self) {
        self.connection_provider = "s3".to_string();
        self.connection_dialog_title = "S3 connection".to_string();
        self.connection_dialog_description =
            "Connect MinIO or an S3-compatible bucket.".to_string();
        self.connection_id.clear();
        self.connection_name = "Local MinIO".to_string();
        self.connection_endpoint_url = "http://127.0.0.1:9000".to_string();
        self.connection_project_id.clear();
        self.connection_region = "us-east-1".to_string();
        self.connection_bucket = "pocopine-demo".to_string();
        self.connection_root_prefix.clear();
        self.connection_access_key_id = "minioadmin".to_string();
        self.connection_secret_access_key = "minioadmin".to_string();
        self.connection_force_path_style = true;
        self.connection_create_bucket = true;
        self.connection_use_emulator = false;
        self.connection_use_anonymous_auth = false;
        self.connection_gcs_auth_mode = "application_default".to_string();
        self.connection_gcs_service_account_json.clear();
        self.connection_gcs_service_account_hint.clear();
        self.connection_gcs_has_service_account_json = false;
    }

    fn reset_to_gcs_defaults(&mut self) {
        self.connection_provider = "gcs".to_string();
        self.connection_dialog_title = "GCS connection".to_string();
        self.connection_dialog_description =
            "Connect Google Cloud Storage or a GCS JSON API emulator.".to_string();
        self.connection_id.clear();
        self.connection_name = "Local GCS".to_string();
        self.connection_endpoint_url = "http://127.0.0.1:4443".to_string();
        self.connection_project_id = "local-dev".to_string();
        self.connection_region.clear();
        self.connection_bucket = "pocopine-demo".to_string();
        self.connection_root_prefix.clear();
        self.connection_access_key_id.clear();
        self.connection_secret_access_key.clear();
        self.connection_force_path_style = false;
        self.connection_create_bucket = false;
        self.connection_use_emulator = true;
        self.connection_use_anonymous_auth = true;
        self.connection_gcs_auth_mode = "anonymous".to_string();
        self.connection_gcs_service_account_json.clear();
        self.connection_gcs_service_account_hint.clear();
        self.connection_gcs_has_service_account_json = false;
    }

    fn populate_connection_form(&mut self, connection: StorageConnectionEdit) {
        self.connection_provider = connection.provider;
        if self.connection_provider == "gcs" {
            self.connection_dialog_title = "GCS connection".to_string();
            self.connection_dialog_description =
                "Connect Google Cloud Storage or a GCS JSON API emulator.".to_string();
        } else {
            self.connection_dialog_title = "S3 connection".to_string();
            self.connection_dialog_description =
                "Connect MinIO or an S3-compatible bucket.".to_string();
        }
        self.connection_id = connection.id;
        self.connection_name = connection.name;
        self.connection_endpoint_url = connection.endpoint_url;
        self.connection_project_id = connection.project_id;
        self.connection_region = connection.region;
        self.connection_bucket = connection.bucket;
        self.connection_root_prefix = connection.root_prefix;
        self.connection_access_key_id = connection.access_key_id;
        self.connection_secret_access_key.clear();
        self.connection_force_path_style = connection.force_path_style;
        self.connection_create_bucket = false;
        self.connection_use_emulator = connection.use_emulator;
        self.connection_use_anonymous_auth = connection.use_anonymous_auth;
        self.connection_gcs_auth_mode = if connection.gcs_auth_mode.is_empty() {
            if connection.use_anonymous_auth {
                "anonymous".to_string()
            } else {
                "application_default".to_string()
            }
        } else {
            connection.gcs_auth_mode
        };
        self.connection_gcs_service_account_json.clear();
        self.connection_gcs_service_account_hint = connection.gcs_service_account_hint;
        self.connection_gcs_has_service_account_json = connection.gcs_has_service_account_json;
        self.modal_error.clear();
    }

    fn finish_save(
        &mut self,
        connections: Vec<StorageConnectionSummary>,
        requested_id: String,
        selected_name: String,
        selected_bucket: String,
    ) {
        self.open = false;
        self.connection_secret_access_key.clear();
        self.connection_gcs_service_account_json.clear();
        pocopine::store::<StorageBrowserStore>().update(move |store| {
            store.apply_saved_connection_result(
                connections,
                requested_id,
                selected_name,
                selected_bucket,
            );
        });
    }
}

#[handlers]
impl FileBrowserConnectionDialog {
    pub fn on_mount(&mut self) {
        if self.connection_provider.is_empty() {
            self.reset_to_minio_defaults();
        }
    }

    #[watch(open)]
    fn on_open_change(&mut self, open: bool, _prev: Option<bool>) {
        if open {
            self.prepare_open();
        }
    }

    #[watch(edit_connection_id)]
    fn on_edit_connection_change(&mut self, _next: String, _prev: Option<String>) {
        if self.open {
            self.prepare_open();
        }
    }

    pub fn load_connection(&mut self, connection_id: String) {
        let connection_id = connection_id.trim().to_string();
        if connection_id.is_empty() {
            self.reset_to_minio_defaults();
            return;
        }
        self.connection_id = connection_id.clone();
        self.loading_connection = true;
        self.modal_error.clear();
        dispatch!(
            crate::get_storage_connection(connection_id.clone()).await,
            |s, result| {
                if !s.open || s.edit_connection_id != connection_id {
                    return;
                }
                s.loading_connection = false;
                match result {
                    Ok(connection) => s.populate_connection_form(connection),
                    Err(err) => s.modal_error = err.to_string(),
                }
            },
        );
    }

    pub fn use_minio_defaults(&mut self) {
        self.modal_error.clear();
        self.loading_connection = false;
        self.reset_to_minio_defaults();
    }

    pub fn use_gcs_defaults(&mut self) {
        self.modal_error.clear();
        self.loading_connection = false;
        self.reset_to_gcs_defaults();
    }

    pub fn set_gcs_auth_mode(&mut self, mode: String) {
        match mode.as_str() {
            "application_default" | "service_account_json" | "anonymous" => {
                self.connection_gcs_auth_mode = mode;
                self.connection_use_anonymous_auth = self.connection_gcs_auth_mode == "anonymous";
            }
            _ => {
                self.modal_error = "Unsupported GCS auth mode".to_string();
            }
        }
    }

    pub fn toggle_gcs_emulator(&mut self) {
        let enabled = !self.connection_use_emulator;
        self.connection_use_emulator = enabled;
        if enabled {
            if self.connection_endpoint_url.trim().is_empty()
                || self.connection_endpoint_url.trim() == "https://storage.googleapis.com"
            {
                self.connection_endpoint_url = "http://127.0.0.1:4443".to_string();
            }
            if self.connection_project_id.trim().is_empty() {
                self.connection_project_id = "local-dev".to_string();
            }
            self.connection_gcs_auth_mode = "anonymous".to_string();
        } else {
            if self.connection_gcs_auth_mode == "anonymous" {
                self.connection_gcs_auth_mode = "application_default".to_string();
            }
            if self.connection_endpoint_url.trim() == "http://127.0.0.1:4443" {
                self.connection_endpoint_url.clear();
            }
            if self.connection_project_id.trim() == "local-dev" {
                self.connection_project_id.clear();
            }
        }
        self.connection_use_anonymous_auth = self.connection_gcs_auth_mode == "anonymous";
    }

    pub fn set_gcs_service_account_json(&mut self, value: String) {
        self.connection_gcs_service_account_json = value;
        if let Some(project_id) =
            service_account_json_field(&self.connection_gcs_service_account_json, "project_id")
        {
            self.connection_project_id = project_id;
        }
    }

    pub fn sync_gcs_service_account_json(&mut self, event: web_sys::InputEvent) {
        let Some(value) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
            .map(|textarea| textarea.value())
        else {
            return;
        };
        self.set_gcs_service_account_json(value);
    }

    pub fn save_connection(&mut self) {
        if self.saving_connection || self.loading_connection {
            return;
        }
        self.saving_connection = true;
        self.modal_error.clear();
        if self.connection_provider == "gcs" {
            let input = GcsConnectionInput {
                id: self.connection_id.clone(),
                name: self.connection_name.clone(),
                endpoint_url: self.connection_endpoint_url.clone(),
                project_id: self.connection_project_id.clone(),
                bucket: self.connection_bucket.clone(),
                root_prefix: self.connection_root_prefix.clone(),
                use_emulator: self.connection_use_emulator,
                use_anonymous_auth: self.connection_gcs_auth_mode == "anonymous",
                auth_mode: self.connection_gcs_auth_mode.clone(),
                service_account_json: self.connection_gcs_service_account_json.clone(),
            };
            let requested_id = input.id.clone();
            let selected_name = input.name.clone();
            let selected_bucket = input.bucket.clone();
            dispatch!(
                crate::save_gcs_storage_connection(input).await,
                |s, result| {
                    s.saving_connection = false;
                    match result {
                        Ok(connections) => {
                            s.finish_save(connections, requested_id, selected_name, selected_bucket)
                        }
                        Err(err) => s.modal_error = err.to_string(),
                    }
                },
            );
            return;
        }

        let input = S3ConnectionInput {
            id: self.connection_id.clone(),
            name: self.connection_name.clone(),
            endpoint_url: self.connection_endpoint_url.clone(),
            region: self.connection_region.clone(),
            bucket: self.connection_bucket.clone(),
            root_prefix: self.connection_root_prefix.clone(),
            access_key_id: self.connection_access_key_id.clone(),
            secret_access_key: self.connection_secret_access_key.clone(),
            force_path_style: self.connection_force_path_style,
            create_bucket_if_missing: self.connection_create_bucket,
        };
        let requested_id = input.id.clone();
        let selected_name = input.name.clone();
        let selected_bucket = input.bucket.clone();
        dispatch!(
            crate::save_s3_storage_connection(input).await,
            |s, result| {
                s.saving_connection = false;
                match result {
                    Ok(connections) => {
                        s.finish_save(connections, requested_id, selected_name, selected_bucket)
                    }
                    Err(err) => s.modal_error = err.to_string(),
                }
            },
        );
    }
}

fn service_account_json_field(raw: &str, field: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(raw).ok()?;
    let kind = json.get("type").and_then(serde_json::Value::as_str)?;
    if kind != "service_account" {
        return None;
    }
    json.get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_account_json(project_id: &str) -> String {
        serde_json::json!({
            "type": "service_account",
            "project_id": project_id,
            "private_key_id": "test-private-key-id",
            "private_key": "-----BEGIN PRIVATE KEY-----\nredacted\n-----END PRIVATE KEY-----\n",
            "client_email": "browser@example.iam.gserviceaccount.com"
        })
        .to_string()
    }

    #[test]
    fn minio_defaults_are_ready_for_local_demo() {
        let dialog = FileBrowserConnectionDialog::default();
        assert_eq!(dialog.connection_provider, "s3");
        assert_eq!(dialog.connection_endpoint_url, "http://127.0.0.1:9000");
        assert_eq!(dialog.connection_bucket, "pocopine-demo");
        assert!(dialog.connection_force_path_style);
        assert!(dialog.connection_create_bucket);
    }

    #[test]
    fn gcs_defaults_are_ready_for_local_demo() {
        let mut dialog = FileBrowserConnectionDialog::default();
        dialog.use_gcs_defaults();

        assert_eq!(dialog.connection_provider, "gcs");
        assert_eq!(dialog.connection_endpoint_url, "http://127.0.0.1:4443");
        assert_eq!(dialog.connection_project_id, "local-dev");
        assert_eq!(dialog.connection_bucket, "pocopine-demo");
        assert!(dialog.connection_use_emulator);
        assert!(dialog.connection_use_anonymous_auth);
        assert_eq!(dialog.connection_gcs_auth_mode, "anonymous");
    }

    #[test]
    fn gcs_auth_mode_controls_legacy_anonymous_flag() {
        let mut dialog = FileBrowserConnectionDialog::default();
        dialog.use_gcs_defaults();

        dialog.set_gcs_auth_mode("service_account_json".to_string());

        assert_eq!(dialog.connection_gcs_auth_mode, "service_account_json");
        assert!(!dialog.connection_use_anonymous_auth);
    }

    #[test]
    fn gcs_emulator_toggle_switches_local_auth_defaults() {
        let mut dialog = FileBrowserConnectionDialog::default();
        dialog.use_minio_defaults();
        dialog.connection_provider = "gcs".to_string();
        dialog.connection_endpoint_url = "https://storage.googleapis.com".to_string();
        dialog.connection_project_id.clear();
        dialog.connection_gcs_auth_mode = "application_default".to_string();

        dialog.toggle_gcs_emulator();

        assert!(dialog.connection_use_emulator);
        assert_eq!(dialog.connection_endpoint_url, "http://127.0.0.1:4443");
        assert_eq!(dialog.connection_project_id, "local-dev");
        assert_eq!(dialog.connection_gcs_auth_mode, "anonymous");
        assert!(dialog.connection_use_anonymous_auth);

        dialog.toggle_gcs_emulator();

        assert!(!dialog.connection_use_emulator);
        assert!(dialog.connection_endpoint_url.is_empty());
        assert!(dialog.connection_project_id.is_empty());
        assert_eq!(dialog.connection_gcs_auth_mode, "application_default");
        assert!(!dialog.connection_use_anonymous_auth);
    }

    #[test]
    fn gcs_service_account_json_extracts_project_id() {
        let mut dialog = FileBrowserConnectionDialog::default();
        dialog.connection_project_id = "manual-project".to_string();

        dialog.set_gcs_service_account_json(service_account_json("json-project"));

        assert_eq!(dialog.connection_project_id, "json-project");
    }

    #[test]
    fn invalid_gcs_service_account_json_keeps_project_id() {
        let mut dialog = FileBrowserConnectionDialog::default();
        dialog.connection_project_id = "manual-project".to_string();

        dialog.set_gcs_service_account_json("{ not valid".to_string());

        assert_eq!(dialog.connection_project_id, "manual-project");
    }
}
