//! HN components. One `#[component]` per file so the `.poco` template
//! sits next to the struct it belongs to (the proc-macro resolves
//! `include_str!("<Ident>.poco")` relative to the calling `.rs`).

pub mod app_shell;
pub mod not_found;
pub mod story_detail;
pub mod story_list;

pub use app_shell::AppShell;
pub use not_found::NotFound;
pub use story_detail::StoryDetail;
pub use story_list::StoryList;
