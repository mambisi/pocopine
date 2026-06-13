//! UI components for the Cloud File Explorer demo.
//!
//! Each component lives in its own folder with a `mod.rs` for Rust
//! and a `<ComponentName>.poco` template. Shared state lives in
//! [`crate::StorageBrowserStore`]; templates read via `$store.storage.X`
//! and dispatch through local handlers (pine-expr can't invoke a
//! method off a path so each component carries a thin delegator).

pub mod app_shell;
pub mod config_dialog;
pub mod connection_dialog;
pub mod file_detail;
pub mod file_list;
pub mod new_folder_dialog;
pub mod route_sync;
pub mod sidebar;
pub mod size_control;
pub mod storage_command;
pub mod top_bar;
pub mod upload_dock;
pub mod upload_panel;

pub use app_shell::FileBrowserApp;
pub use config_dialog::FileBrowserConfigDialog;
pub use connection_dialog::FileBrowserConnectionDialog;
pub use file_detail::FileBrowserFileDetail;
pub use file_list::FileBrowserFileList;
pub use new_folder_dialog::FileBrowserNewFolderDialog;
pub use route_sync::FileBrowserRoute;
pub use sidebar::FileBrowserSidebar;
pub use size_control::FileBrowserSizeControl;
pub use storage_command::FileBrowserStorageCommand;
pub use top_bar::{FileBrowserTopBar, apply_saved_theme};
pub use upload_dock::FileBrowserUploadDock;
pub use upload_panel::FileBrowserUploadPanel;
