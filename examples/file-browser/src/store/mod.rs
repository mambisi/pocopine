//! `FileBrowserStore` — singleton owning the file-browser demo's
//! durable state.
//!
//! Templates read via `$store.files.<field>`; components dispatch
//! through `pocopine::store::<FileBrowserStore>().update(|s| …)`.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::FileEntry;

const QUOTA_BYTES: u64 = 100 * 1024 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[store(name = "files")]
pub struct FileBrowserStore {
    pub files: Vec<FileEntry>,
    pub loading: bool,
    pub files_empty: bool,
    pub error: String,
    pub deleting_id: String,
    pub file_count_label: String,
    pub total_size_label: String,
    /// 0..=100 — visual progress against a 100 GB quota.
    pub storage_percent: f64,
    pub storage_quota_label: String,
}

impl Default for FileBrowserStore {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            loading: false,
            files_empty: true,
            error: String::new(),
            deleting_id: String::new(),
            file_count_label: "0 files".to_string(),
            total_size_label: "0 B".to_string(),
            storage_percent: 0.0,
            storage_quota_label: "100 GB".to_string(),
        }
    }
}

#[handlers]
impl FileBrowserStore {
    pub fn refresh(&mut self) {
        self.loading = true;
        self.error.clear();
        dispatch!(crate::list_files().await, |s, result| {
            s.loading = false;
            match result {
                Ok(files) => s.apply_files(files),
                Err(err) => s.error = err.to_string(),
            }
        },);
    }

    pub fn remove_file(&mut self, file_id: String) {
        if file_id.is_empty() || self.deleting_id == file_id {
            return;
        }
        self.deleting_id = file_id.clone();
        self.error.clear();
        dispatch!(crate::delete_stored_file(file_id).await, |s, result| {
            s.deleting_id.clear();
            match result {
                Ok(files) => s.apply_files(files),
                Err(err) => s.error = err.to_string(),
            }
        },);
    }
}

// `apply_files` takes a `Vec<FileEntry>` — not `FromHandlerArg`, so
// `#[handlers]` can't synthesize a dispatch arm for it. Keep the
// internal helper in a plain impl block instead.
impl FileBrowserStore {
    fn apply_files(&mut self, files: Vec<FileEntry>) {
        let total_size: u64 = files.iter().map(|file| file.size_bytes).sum();
        self.file_count_label = match files.len() {
            0 => "0 files".to_string(),
            1 => "1 file".to_string(),
            len => format!("{len} files"),
        };
        self.total_size_label = crate::format_size(total_size);
        self.storage_percent = ((total_size as f64 / QUOTA_BYTES as f64) * 100.0).clamp(0.0, 100.0);
        self.files_empty = files.is_empty();
        self.files = files;
        self.error.clear();
    }
}
