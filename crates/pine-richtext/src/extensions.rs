//! Built-in extensions composing the default rich text schema.
//!
//! Each submodule contributes a slice of the schema + commands +
//! key bindings + plugins + list-item metadata to the
//! [`crate::extension::RichTextExtension`] contract.
//! The fold order in [`default_extensions`] determines the resulting
//! node insertion rank in `schema_basic::schema()` — kept byte-identical
//! to the pre-extension hand-rolled order so the snapshot test passes
//! and existing content-match behaviour is preserved.
//!
//! `schema_basic::schema()` folds [`default_extensions`] on its first
//! call (and applies any user `extension::register(...)` shadowing
//! by name); `view::input::default_keymap` and
//! `view::root::default_plugins` read merged commands / key bindings /
//! plugins from `extension::registry`. The demo in `examples/richtext`
//! registers a `TaskListExtension::with_node_view::<PineTaskItem>()`
//! to thread its custom-element node-view through the same contract.

pub mod core_inline;
pub mod core_marks;
pub mod core_nodes;
pub mod history;
pub mod lists;
pub mod task_list;

#[cfg(test)]
mod tests;

pub use core_inline::CoreInlineExtension;
pub use core_marks::CoreMarksExtension;
pub use core_nodes::CoreNodesExtension;
pub use history::HistoryExtension;
pub use lists::ListsExtension;
pub use task_list::TaskListExtension;

use crate::extension::RichTextExtension;

/// The default set of extensions composing `schema_basic::schema()`.
/// Ordering matters: it determines node insertion rank, which the
/// schema's content-match resolution observes.
///
/// Each call returns a fresh `Vec` of boxed trait objects so the
/// schema fold (C3) and tests can consume them by value.
pub fn default_extensions() -> Vec<Box<dyn RichTextExtension>> {
    vec![
        Box::new(CoreNodesExtension),
        Box::new(ListsExtension),
        Box::new(TaskListExtension::new()),
        Box::new(CoreInlineExtension),
        Box::new(CoreMarksExtension),
        Box::new(HistoryExtension),
    ]
}
