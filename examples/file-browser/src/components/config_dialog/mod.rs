//! `<file-browser-config-dialog>` — local CFE settings form.

use pine::{
    PineDialogClose, PineDialogContent, PineDialogDescription, PineDialogOverlay, PineDialogPortal,
    PineDialogRoot, PineDialogTitle,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    FileBrowserSizeControl, StorageBrowserConfigEdit, StorageBrowserConfigInput,
    StorageBrowserStore,
};

const MIB: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[component(
    template = "FileBrowserConfigDialog.poco",
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
        FileBrowserSizeControl,
    ]
)]
pub struct FileBrowserConfigDialog {
    #[model]
    pub open: bool,
    pub loading_config: bool,
    pub saving_config: bool,
    pub config_error: String,
    pub config_status: String,
    pub config_path: String,
    pub upload_max_mib: f64,
    pub preferred_chunk_mib: f64,
    pub preferred_chunk_max_mib: f64,
}

impl Default for FileBrowserConfigDialog {
    fn default() -> Self {
        Self {
            open: false,
            loading_config: false,
            saving_config: false,
            config_error: String::new(),
            config_status: String::new(),
            config_path: String::new(),
            upload_max_mib: 25.0,
            preferred_chunk_mib: 1.0,
            preferred_chunk_max_mib: 25.0,
        }
    }
}

impl FileBrowserConfigDialog {
    fn populate_config(&mut self, config: StorageBrowserConfigEdit) {
        let store_config = config.clone();
        self.config_path = config.config_path;
        self.upload_max_mib = bytes_to_mib(config.upload_max_bytes);
        self.preferred_chunk_mib = bytes_to_mib(config.preferred_chunk_bytes);
        self.sync_size_values();
        pocopine::store::<StorageBrowserStore>().update(move |store| {
            store.apply_config_result(store_config);
        });
    }

    fn config_input(&self) -> StorageBrowserConfigInput {
        let upload_max_mib = mib_value(self.upload_max_mib);
        let preferred_chunk_mib = mib_value(self.preferred_chunk_mib).min(upload_max_mib);
        StorageBrowserConfigInput {
            upload_max_bytes: upload_max_mib.saturating_mul(MIB),
            preferred_chunk_bytes: preferred_chunk_mib.saturating_mul(MIB),
        }
    }

    fn sync_size_values(&mut self) {
        self.upload_max_mib = mib_value(self.upload_max_mib) as f64;
        self.preferred_chunk_max_mib = self.upload_max_mib.min(64.0).max(1.0);
        self.preferred_chunk_mib = mib_value(self.preferred_chunk_mib) as f64;
        if self.preferred_chunk_mib > self.preferred_chunk_max_mib {
            self.preferred_chunk_mib = self.preferred_chunk_max_mib;
        }
    }
}

#[handlers]
impl FileBrowserConfigDialog {
    #[watch(open)]
    fn on_open_change(&mut self, open: bool, _prev: Option<bool>) {
        if open {
            self.load_config();
        }
    }

    #[watch(upload_max_mib)]
    fn on_upload_max_mib_change(&mut self, _next: f64, _prev: Option<f64>) {
        self.sync_size_values();
    }

    #[watch(preferred_chunk_mib)]
    fn on_preferred_chunk_mib_change(&mut self, _next: f64, _prev: Option<f64>) {
        self.sync_size_values();
    }

    pub fn load_config(&mut self) {
        self.loading_config = true;
        self.config_error.clear();
        self.config_status.clear();
        dispatch!(crate::get_storage_browser_config().await, |s, result| {
            s.loading_config = false;
            match result {
                Ok(config) => s.populate_config(config),
                Err(err) => s.config_error = err.to_string(),
            }
        },);
    }

    pub fn save_config(&mut self) {
        if self.loading_config || self.saving_config {
            return;
        }
        let input = self.config_input();
        self.saving_config = true;
        self.config_error.clear();
        self.config_status.clear();
        dispatch!(
            crate::save_storage_browser_config(input).await,
            |s, result| {
                s.saving_config = false;
                match result {
                    Ok(config) => {
                        let restart_required = config.restart_required;
                        s.populate_config(config);
                        s.config_status = if restart_required {
                            "Saved for next server start".to_string()
                        } else {
                            "Config saved".to_string()
                        };
                    }
                    Err(err) => s.config_error = err.to_string(),
                }
            },
        );
    }
}

fn bytes_to_mib(bytes: u64) -> f64 {
    (bytes / MIB).max(1) as f64
}

fn mib_value(value: f64) -> u64 {
    value.round().clamp(1.0, 1024.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_input_converts_mib_to_bytes() {
        let mut dialog = FileBrowserConfigDialog::default();
        dialog.upload_max_mib = 64.0;
        dialog.preferred_chunk_mib = 4.0;

        let input = dialog.config_input();

        assert_eq!(input.upload_max_bytes, 64 * MIB);
        assert_eq!(input.preferred_chunk_bytes, 4 * MIB);
    }

    #[test]
    fn size_values_clamp_chunk_to_upload_cap() {
        let mut dialog = FileBrowserConfigDialog::default();
        dialog.upload_max_mib = 2.0;
        dialog.preferred_chunk_mib = 4.0;

        dialog.sync_size_values();

        assert_eq!(dialog.preferred_chunk_mib, 2.0);
        assert_eq!(dialog.preferred_chunk_max_mib, 2.0);
    }
}
