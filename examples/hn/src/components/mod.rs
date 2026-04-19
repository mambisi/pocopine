//! HN components. One `#[component]` per directory so the `.poco`
//! template and per-component styles sit next to the struct (the
//! proc-macro resolves `include_str!("<Ident>.poco")` relative to
//! the calling `.rs`).

pub mod app_shell;
pub mod comment;
pub mod not_found;
pub mod story_detail;
pub mod story_list;

pub use app_shell::AppShell;
pub use comment::{Comment, HnComment};
pub use not_found::NotFound;
pub use story_detail::StoryDetail;
pub use story_list::StoryList;
