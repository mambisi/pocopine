//! UI components for the file-browser demo.
//!
//! Each component lives in its own folder with a `mod.rs` for Rust
//! and a `<ComponentName>.poco` template. Shared state lives in
//! [`crate::FileBrowserStore`]; templates read via `$store.files.X`
//! and dispatch through local handlers (pine-expr can't invoke a
//! method off a path so each component carries a thin delegator).

pub mod app_shell;
pub mod file_list;
pub mod sidebar;
pub mod top_bar;
pub mod upload_panel;

pub use app_shell::FileBrowserApp;
pub use file_list::FileBrowserFileList;
pub use sidebar::FileBrowserSidebar;
pub use top_bar::FileBrowserTopBar;
pub use upload_panel::FileBrowserUploadPanel;
