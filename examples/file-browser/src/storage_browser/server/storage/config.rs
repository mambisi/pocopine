use std::fs;
use std::io::Write;
use std::path::PathBuf;

use pocopine::{ServerError, ServerResult};
use pocopine_storage::{StorageError, StorageResult, UploadPolicy};
use serde::{Deserialize, Serialize};

use crate::storage_browser::server::storage::*;
use crate::storage_browser::{StorageBrowserConfigEdit, StorageBrowserConfigInput};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StorageBrowserConfig {
    #[serde(default)]
    pub(crate) settings: StorageBrowserSettings,
    #[serde(default)]
    pub(crate) connections: Vec<SavedConnection>,
}

impl Default for StorageBrowserConfig {
    fn default() -> Self {
        Self {
            settings: StorageBrowserSettings::default(),
            connections: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StorageBrowserSettings {
    #[serde(default = "default_upload_max_bytes")]
    pub(crate) upload_max_bytes: u64,
    #[serde(default = "default_preferred_chunk_bytes")]
    pub(crate) preferred_chunk_bytes: u64,
}

impl Default for StorageBrowserSettings {
    fn default() -> Self {
        Self {
            upload_max_bytes: DIRECT_UPLOAD_LIMIT_BYTES,
            preferred_chunk_bytes: DEFAULT_CHUNK_BYTES,
        }
    }
}

impl StorageBrowserSettings {
    pub(crate) fn from_input(input: StorageBrowserConfigInput) -> ServerResult<Self> {
        Self {
            upload_max_bytes: input.upload_max_bytes,
            preferred_chunk_bytes: input.preferred_chunk_bytes,
        }
        .validate()
    }

    pub(crate) fn validate(self) -> ServerResult<Self> {
        if !(MIN_UPLOAD_LIMIT_BYTES..=MAX_UPLOAD_LIMIT_BYTES).contains(&self.upload_max_bytes) {
            return Err(ServerError::App(format!(
                "upload cap must be between {} and {}",
                crate::format_size(MIN_UPLOAD_LIMIT_BYTES),
                crate::format_size(MAX_UPLOAD_LIMIT_BYTES)
            )));
        }
        if !(MIN_CHUNK_BYTES..=MAX_CHUNK_BYTES).contains(&self.preferred_chunk_bytes) {
            return Err(ServerError::App(format!(
                "chunk size must be between {} and {}",
                crate::format_size(MIN_CHUNK_BYTES),
                crate::format_size(MAX_CHUNK_BYTES)
            )));
        }
        if self.preferred_chunk_bytes > self.upload_max_bytes {
            return Err(ServerError::App(
                "chunk size cannot exceed upload cap".to_string(),
            ));
        }
        Ok(self)
    }

    pub(crate) fn edit(&self, active: &StorageBrowserSettings) -> StorageBrowserConfigEdit {
        StorageBrowserConfigEdit {
            config_path: config_path().display().to_string(),
            upload_max_bytes: self.upload_max_bytes,
            preferred_chunk_bytes: self.preferred_chunk_bytes,
            upload_max_label: crate::format_size(self.upload_max_bytes),
            preferred_chunk_label: crate::format_size(self.preferred_chunk_bytes),
            active_upload_max_bytes: active.upload_max_bytes,
            active_preferred_chunk_bytes: active.preferred_chunk_bytes,
            active_upload_max_label: crate::format_size(active.upload_max_bytes),
            active_preferred_chunk_label: crate::format_size(active.preferred_chunk_bytes),
            restart_required: self.upload_max_bytes != active.upload_max_bytes
                || self.preferred_chunk_bytes != active.preferred_chunk_bytes,
        }
    }
}

fn default_upload_max_bytes() -> u64 {
    DIRECT_UPLOAD_LIMIT_BYTES
}

fn default_preferred_chunk_bytes() -> u64 {
    DEFAULT_CHUNK_BYTES
}

pub(crate) fn active_settings(fallback: &StorageBrowserSettings) -> StorageBrowserSettings {
    fallback.clone()
}

pub(crate) fn load_upload_settings() -> StorageResult<StorageBrowserSettings> {
    load_config_storage()?
        .settings
        .validate()
        .map_err(|err| StorageError::backend(format!("load storage browser settings: {err}")))
}

pub(crate) fn upload_policy_for_settings(
    settings: &StorageBrowserSettings,
) -> StorageResult<UploadPolicy> {
    Ok(UploadPolicy::new(UPLOAD_BACKEND)?
        .max_bytes(settings.upload_max_bytes)
        .preferred_chunk_size(settings.preferred_chunk_bytes))
}

pub(crate) fn load_config_storage() -> StorageResult<StorageBrowserConfig> {
    load_config().map_err(|err| StorageError::backend(format!("load storage config: {err}")))
}

pub(crate) fn load_config() -> ServerResult<StorageBrowserConfig> {
    let path = config_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StorageBrowserConfig::default());
        }
        Err(err) => return Err(io_error("read storage browser config", err)),
    };
    serde_json::from_slice(&bytes)
        .map_err(|err| ServerError::App(format!("parse storage browser config: {err}")))
}

pub(crate) fn save_config(config: &StorageBrowserConfig) -> ServerResult<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| io_error("create config directory", err))?;
    }
    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|err| ServerError::App(format!("encode storage browser config: {err}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| io_error("open storage browser config", err))?;
        file.write_all(&bytes)
            .map_err(|err| io_error("write storage browser config", err))?;
        file.sync_all()
            .map_err(|err| io_error("sync storage browser config", err))?;
    }

    #[cfg(not(unix))]
    {
        fs::write(&path, bytes).map_err(|err| io_error("write storage browser config", err))?;
    }

    Ok(())
}

pub(crate) fn config_path() -> PathBuf {
    std::env::var(CONFIG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".data")
                .join("storage-browser")
                .join("connections.json")
        })
}
