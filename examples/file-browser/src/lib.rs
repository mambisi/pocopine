//! Cloud File Explorer (CFE) demo — S3/MinIO-first object-store explorer.
//!
//! The example keeps the original `examples/file-browser` path while
//! evolving the app into a storage client. The first shipped provider
//! is S3-compatible storage, with MinIO as the local development target.

pub mod components;
pub mod storage_browser;
pub mod store;

pub use components::{
    FileBrowserApp, FileBrowserConfigDialog, FileBrowserConnectionDialog, FileBrowserFileDetail,
    FileBrowserFileList, FileBrowserNewFolderDialog, FileBrowserRoute, FileBrowserSidebar,
    FileBrowserSizeControl, FileBrowserStorageCommand, FileBrowserTopBar, FileBrowserUploadDock,
    FileBrowserUploadPanel,
};
#[cfg(not(target_arch = "wasm32"))]
pub use storage_browser::storage_server;
pub use storage_browser::{
    GcsConnectionInput, S3ConnectionInput, StorageBreadcrumb, StorageBrowserConfigEdit,
    StorageBrowserConfigInput, StorageCommandEntry, StorageConnectionEdit,
    StorageConnectionSummary, StorageEntry, StorageListing, StorageMetadataEntry,
    StorageObjectDetail, browse_storage_connection, create_storage_folder,
    delete_storage_connection, get_storage_browser_config, get_storage_connection,
    get_storage_object_detail, list_storage_connections, list_storage_object_commands,
    save_gcs_storage_connection, save_s3_storage_connection, save_storage_browser_config,
};
pub use store::StorageBrowserStore;

use pine_icons::PineIcon;
use pocopine::prelude::*;

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn format_size(bytes: u64) -> String {
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

#[wasm_bindgen(start)]
pub fn main() {
    pine_icons::register_icons![
        "alert-circle",
        "arrow-up",
        "brand-amazon",
        "brand-aws",
        "bucket",
        "check",
        "chevron-left",
        "chevron-right",
        "clock",
        "cloud",
        "copy",
        "database",
        "dots-vertical",
        "download",
        "external-link",
        "file",
        "folder",
        "folder-plus",
        "folder-open",
        "grip-vertical",
        "key",
        "link",
        "lock",
        "maximize",
        "minimize",
        "pencil",
        "plug",
        "plus",
        "refresh",
        "rotate",
        "route",
        "moon",
        "search",
        "server",
        "settings",
        "sun",
        "trash",
        "upload",
        "world",
        "x",
    ];
    pine::register_all();
    components::apply_saved_theme();
    App::new()
        .plugin(pocopine_storage::storage_plugin())
        .store::<StorageBrowserStore>()
        .register::<PineIcon>()
        .register::<FileBrowserApp>()
        .register::<FileBrowserTopBar>()
        .register::<FileBrowserStorageCommand>()
        .register::<FileBrowserSidebar>()
        .register::<FileBrowserSizeControl>()
        .register::<FileBrowserUploadPanel>()
        .register::<FileBrowserConfigDialog>()
        .register::<FileBrowserConnectionDialog>()
        .register::<FileBrowserUploadDock>()
        .register::<FileBrowserNewFolderDialog>()
        .register::<FileBrowserFileList>()
        .register::<FileBrowserFileDetail>()
        .route::<FileBrowserRoute>("/")
        .route::<FileBrowserRoute>("/connection/:connection_id")
        .route::<FileBrowserRoute>("/connection/:connection_id/:prefix")
        .route::<FileBrowserRoute>("/connection/:connection_id/*prefix")
        .run();
}
