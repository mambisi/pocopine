use pine::PineUpload;
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: String,
    pub name: String,
    pub extension: String,
    pub size_bytes: u64,
    pub size_label: String,
    pub modified_label: String,
    pub download_url: String,
    pub icon: String,
}

#[pocopine::server(public)]
pub async fn list_files() -> ServerResult<Vec<FileEntry>> {
    server_side::list_files()
}

#[pocopine::server(public)]
pub async fn delete_stored_file(file_id: String) -> ServerResult<Vec<FileEntry>> {
    server_side::delete_file(&file_id)
}

#[derive(Serialize, Deserialize)]
#[component(
    template = "FileBrowserApp.poco",
    role = "panel",
    uses = [PineIcon, PineUpload]
)]
pub struct FileBrowserApp {
    pub files: Vec<FileEntry>,
    pub loading: bool,
    pub files_empty: bool,
    pub error: String,
    pub deleting_id: String,
    pub file_count_label: String,
    pub total_size_label: String,
}

impl Default for FileBrowserApp {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            loading: false,
            files_empty: true,
            error: String::new(),
            deleting_id: String::new(),
            file_count_label: "0 files".to_string(),
            total_size_label: "0 B".to_string(),
        }
    }
}

#[handlers]
impl FileBrowserApp {
    pub fn on_mount(&mut self) {
        self.refresh();
    }

    pub fn refresh(&mut self) {
        self.loading = true;
        self.error.clear();
        dispatch!(list_files().await, |s, result| {
            s.loading = false;
            match result {
                Ok(files) => s.apply_files(files),
                Err(err) => s.error = err.to_string(),
            }
        },);
    }

    pub fn upload_complete(&mut self) {
        self.refresh();
    }

    pub fn upload_failed(&mut self) {
        self.refresh();
    }

    pub fn remove_file(&mut self, file_id: String) {
        if file_id.is_empty() || self.deleting_id == file_id {
            return;
        }
        self.deleting_id = file_id.clone();
        self.error.clear();
        dispatch!(delete_stored_file(file_id).await, |s, result| {
            s.deleting_id.clear();
            match result {
                Ok(files) => s.apply_files(files),
                Err(err) => s.error = err.to_string(),
            }
        },);
    }
}

impl FileBrowserApp {
    fn apply_files(&mut self, files: Vec<FileEntry>) {
        let total_size = files.iter().map(|file| file.size_bytes).sum();
        self.file_count_label = match files.len() {
            0 => "0 files".to_string(),
            1 => "1 file".to_string(),
            len => format!("{len} files"),
        };
        self.total_size_label = format_size(total_size);
        self.files_empty = files.is_empty();
        self.files = files;
        self.error.clear();
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use server_side::{
    content_disposition_for_id, content_type_for_id, download_file_path, storage_server,
};

#[cfg(not(target_arch = "wasm32"))]
mod server_side {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use pocopine::{ServerError, ServerResult};
    use pocopine_storage::{
        LocalFsStorageBackend, ObjectMetadata, SafeObjectKey, StorageContext, StorageKey,
        StorageKeyFuture, StorageKeyResolver, StorageResult, StorageScope, StorageServer,
        UploadIntent, UploadPolicy,
    };

    use super::{format_size, FileEntry};

    const STORAGE_BACKEND: &str = "local_fs";
    const STORAGE_SCOPE: &str = "files";
    const DEFAULT_MAX_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;
    const DEFAULT_CHUNK_BYTES: u64 = 1024 * 1024;

    struct FileBrowserKeyResolver;

    impl StorageKeyResolver for FileBrowserKeyResolver {
        fn resolve_key<'a>(
            &'a self,
            _ctx: &'a StorageContext,
            intent: &'a UploadIntent,
        ) -> StorageKeyFuture<'a> {
            Box::pin(async move {
                let file_name = sanitize_upload_name(intent.file_name());
                let key = SafeObjectKey::parse(format!(
                    "{STORAGE_SCOPE}/{}-{file_name}",
                    intent.generated_object_id()
                ))?;
                let mut metadata = ObjectMetadata::default();
                metadata.insert("original_name", intent.file_name());
                Ok(StorageKey::new(key).metadata(metadata))
            })
        }
    }

    pub fn storage_server() -> StorageResult<StorageServer> {
        let root = storage_root_path();
        let backend = LocalFsStorageBackend::new(&root);
        let policy = UploadPolicy::new(STORAGE_BACKEND)?
            .max_bytes(max_upload_bytes())
            .preferred_chunk_size(DEFAULT_CHUNK_BYTES);
        let scope = StorageScope::builder(policy)
            .key_resolver(FileBrowserKeyResolver)
            .build();

        let builder = StorageServer::builder()
            .backend(STORAGE_BACKEND, backend)?
            .secure_anonymous_cookies(false)
            .scope(STORAGE_SCOPE, scope)?;
        Ok(builder.build())
    }

    pub fn list_files() -> ServerResult<Vec<FileEntry>> {
        let dir = storage_root_path().join(STORAGE_SCOPE);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(io_error("read storage directory", err)),
        };

        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| io_error("read storage entry", err))?;
            let file_type = entry
                .file_type()
                .map_err(|err| io_error("read storage entry type", err))?;
            if !file_type.is_file() {
                continue;
            }

            let Some(id) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if validate_file_id(&id).is_err() {
                continue;
            }

            let metadata = entry
                .metadata()
                .map_err(|err| io_error("read file metadata", err))?;
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            let modified_sort = modified
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let name = display_name_for_id(&id);
            let extension = extension_for(&name);
            let size_bytes = metadata.len();

            files.push((
                modified_sort,
                FileEntry {
                    id: id.clone(),
                    name,
                    extension: extension.clone(),
                    size_bytes,
                    size_label: format_size(size_bytes),
                    modified_label: format_modified(modified),
                    download_url: format!("/api/files/download/{id}"),
                    icon: icon_for_extension(&extension).to_string(),
                },
            ));
        }

        files.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.name.cmp(&right.1.name))
        });
        Ok(files.into_iter().map(|(_, file)| file).collect())
    }

    pub fn delete_file(file_id: &str) -> ServerResult<Vec<FileEntry>> {
        let path = download_file_path(file_id)?;
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_error("delete file", err)),
        }
        list_files()
    }

    pub fn download_file_path(file_id: &str) -> ServerResult<PathBuf> {
        let file_id = validate_file_id(file_id)?;
        Ok(storage_root_path().join(STORAGE_SCOPE).join(file_id))
    }

    pub fn content_type_for_id(file_id: &str) -> &'static str {
        match extension_for(file_id).as_str() {
            "avif" => "image/avif",
            "css" => "text/css; charset=utf-8",
            "csv" => "text/csv; charset=utf-8",
            "gif" => "image/gif",
            "html" | "htm" => "text/html; charset=utf-8",
            "jpg" | "jpeg" => "image/jpeg",
            "js" | "mjs" => "text/javascript; charset=utf-8",
            "json" => "application/json",
            "md" | "txt" => "text/plain; charset=utf-8",
            "pdf" => "application/pdf",
            "png" => "image/png",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "zip" => "application/zip",
            _ => "application/octet-stream",
        }
    }

    pub fn content_disposition_for_id(file_id: &str) -> String {
        let name = display_name_for_id(file_id);
        let quoted = name
            .chars()
            .map(|ch| match ch {
                '"' | '\\' | '\r' | '\n' => '_',
                ch if ch.is_control() => '_',
                ch => ch,
            })
            .collect::<String>();
        format!("attachment; filename=\"{quoted}\"")
    }

    fn storage_root_path() -> PathBuf {
        std::env::var("POCOPINE_FILE_BROWSER_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join(".data")
                    .join("storage")
            })
    }

    fn max_upload_bytes() -> u64 {
        std::env::var("POCOPINE_FILE_BROWSER_MAX_BYTES")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES)
    }

    pub(crate) fn sanitize_upload_name(raw: &str) -> String {
        let raw = raw
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(raw)
            .trim()
            .trim_matches('\0');
        let mut name = String::with_capacity(raw.len().max("upload.bin".len()));
        for ch in raw.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                name.push(ch);
            } else {
                name.push('_');
            }
        }

        while name.ends_with('.') || name.ends_with(' ') {
            name.pop();
        }
        while name.starts_with('.') {
            name.remove(0);
        }
        if name.is_empty() {
            name.push_str("upload.bin");
        }
        if is_windows_reserved_leaf(&name) {
            name.insert(0, '_');
        }
        truncate_file_name(name, 180)
    }

    fn truncate_file_name(name: String, max_len: usize) -> String {
        if name.len() <= max_len {
            return name;
        }
        let Some((base, ext)) = name.rsplit_once('.') else {
            return name.chars().take(max_len).collect();
        };
        let ext_len = ext.len() + 1;
        if ext_len >= max_len {
            return name.chars().take(max_len).collect();
        }
        let base_len = max_len - ext_len;
        let base = base.chars().take(base_len).collect::<String>();
        format!("{base}.{ext}")
    }

    fn validate_file_id(file_id: &str) -> ServerResult<String> {
        if file_id.is_empty()
            || file_id.len() > 240
            || file_id.trim() != file_id
            || file_id.contains('/')
            || file_id.contains('\\')
            || file_id.chars().any(char::is_control)
        {
            return Err(ServerError::App("invalid file id".to_string()));
        }
        SafeObjectKey::parse(format!("{STORAGE_SCOPE}/{file_id}"))
            .map_err(|_| ServerError::App("invalid file id".to_string()))?;
        Ok(file_id.to_string())
    }

    fn display_name_for_id(file_id: &str) -> String {
        let uuid_prefix_len = 37;
        if file_id.len() > uuid_prefix_len
            && file_id.as_bytes().get(36) == Some(&b'-')
            && file_id
                .chars()
                .take(36)
                .all(|ch| ch.is_ascii_hexdigit() || ch == '-')
        {
            file_id[uuid_prefix_len..].to_string()
        } else {
            file_id.to_string()
        }
    }

    fn extension_for(name: &str) -> String {
        name.rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default()
    }

    fn icon_for_extension(extension: &str) -> &'static str {
        match extension {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "svg" => "photo",
            "zip" | "gz" | "tar" | "7z" => "archive",
            "pdf" => "file-type-pdf",
            _ => "file",
        }
    }

    fn format_modified(modified: SystemTime) -> String {
        let Ok(age) = SystemTime::now().duration_since(modified) else {
            return "just now".to_string();
        };
        let seconds = age.as_secs();
        if seconds < 60 {
            "just now".to_string()
        } else if seconds < 60 * 60 {
            let minutes = seconds / 60;
            format!("{minutes}m ago")
        } else if seconds < 24 * 60 * 60 {
            let hours = seconds / (60 * 60);
            format!("{hours}h ago")
        } else if seconds < 30 * 24 * 60 * 60 {
            let days = seconds / (24 * 60 * 60);
            format!("{days}d ago")
        } else {
            "older".to_string()
        }
    }

    fn is_windows_reserved_leaf(name: &str) -> bool {
        let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
        matches!(
            stem.to_ascii_uppercase().as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
    }

    fn io_error(action: &str, err: std::io::Error) -> ServerError {
        ServerError::App(format!("{action}: {err}"))
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    pine_icons::register_icons![
        "archive",
        "check",
        "cloud",
        "database",
        "download",
        "file",
        "file-type-pdf",
        "file-upload",
        "folder",
        "photo",
        "refresh",
        "search",
        "trash",
        "upload",
        "x",
    ];
    pine::register_all();
    App::new()
        .plugin(pocopine_storage::upload_plugin())
        .register::<PineIcon>()
        .register::<FileBrowserApp>()
        .run();
}

#[cfg(test)]
mod tests {
    use pocopine_storage::SafeObjectKey;

    use super::server_side;

    #[test]
    fn upload_name_sanitization_produces_safe_object_segments() {
        let name = server_side::sanitize_upload_name("../CON.txt");
        assert_eq!(name, "_CON.txt");
        assert!(SafeObjectKey::parse(format!("files/{name}")).is_ok());

        let name = server_side::sanitize_upload_name("team report (final).pdf");
        assert_eq!(name, "team_report__final_.pdf");
        assert!(SafeObjectKey::parse(format!("files/{name}")).is_ok());
    }

    #[test]
    fn file_id_validation_rejects_path_escape() {
        assert!(server_side::download_file_path("../secret").is_err());
        assert!(server_side::download_file_path("nested/secret").is_err());
        assert!(server_side::download_file_path("good-file.txt").is_ok());
    }
}
