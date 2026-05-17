//! Per-`<pine-rich-text-root>` editor configuration.
//!
//! An [`EditorRuntime`] is an immutable bundle of everything an editor
//! mount needs: a [`Schema`], the chain of [`RichTextExtension`]s that
//! produced it, a [`NodeViewRegistry`] for custom-element tags, a
//! command table, a key-binding factory list, a plugin set, and a
//! list-item-type table. It's wrapped in `Arc` so multiple mounts of
//! the same configuration share one fold without re-building.
//!
//! ## Why a runtime instead of process-globals
//!
//! Phase 4 stored everything an editor needed in five process-global
//! `OnceLock`s (one schema, one extension list, one command table, one
//! key-bindings list, one node-view tag map). That sealed the door on
//! the day's most common cross-cutting request: two editors on one
//! page with different configurations (a comment box with no
//! headings/lists alongside a doc editor with the full kit).
//!
//! Phase 4b reshapes those globals into a per-runtime bundle. The
//! "default runtime" — built lazily by [`registry::default`] — folds
//! the same `default_extensions()` set as `schema_basic::schema()`, so
//! editors that don't specify a runtime get byte-identical behavior.
//! Named runtimes registered via [`registry::register`] participate
//! alongside the default; each `<pine-rich-text-root runtime="…">`
//! mount picks its runtime by name.
//!
//! See `docs/extensions.md` for the migration story and a worked
//! two-editor example.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::extension::{KeyBindingFactory, NamedCommand, RichTextExtension};
use crate::model::Schema;
use crate::state::Plugin;

pub mod builder;
pub mod node_views;
pub mod registry;

#[cfg(test)]
mod tests;

pub use builder::RuntimeBuilder;
pub use node_views::{NodeViewRegistry, NodeViewSpec};

/// Immutable per-instance editor configuration.
///
/// Built by [`RuntimeBuilder::build`] or [`registry::default`]; consumed
/// (cloned cheaply via `Arc`) by every `<pine-rich-text-root>` mount.
/// Once built, fields are read-only — swapping configuration on a live
/// editor is undefined behavior in Phase 4b.
pub struct EditorRuntime {
    pub(crate) name: Option<String>,
    pub(crate) schema: Schema,
    pub(crate) extensions: Vec<Arc<dyn RichTextExtension>>,
    pub(crate) node_views: NodeViewRegistry,
    pub(crate) commands: HashMap<String, NamedCommand>,
    pub(crate) key_bindings: Vec<(String, KeyBindingFactory)>,
    pub(crate) plugins: Vec<Plugin>,
    pub(crate) list_item_types: HashSet<String>,
}

impl EditorRuntime {
    /// Start a runtime builder. Equivalent to [`RuntimeBuilder::new`].
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// Optional diagnostic label set via [`RuntimeBuilder::name`].
    /// `None` for the default runtime; `Some("comment")` for a runtime
    /// registered via `registry::register("comment", …)`.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Folded schema. Same shape as `schema_basic::schema()` for the
    /// default runtime, custom for named runtimes.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Snapshot of the extension chain in fold order. Cheap; intended
    /// for diagnostics.
    pub fn extensions(&self) -> &[Arc<dyn RichTextExtension>] {
        &self.extensions
    }

    /// Resolve the node-view binding for `node_type` in this runtime.
    /// `None` means "render with the default tag." See
    /// [`NodeViewRegistry`] for the data shape.
    pub fn lookup_node_view(&self, node_type: &str) -> Option<&NodeViewSpec> {
        self.node_views.lookup(node_type)
    }

    /// Process-globally-registered custom-element tags this runtime
    /// uses. The browser view mounts these after every `innerHTML`
    /// patch so dynamically-rendered node-view hosts behave like
    /// app-authored components.
    pub fn registered_tags(&self) -> Vec<String> {
        self.node_views.registered_tags()
    }

    /// Look up a named command. Used by the
    /// `pine:richtext:command`'s `Custom { name, args }` dispatch path.
    pub fn named_command(&self, name: &str) -> Option<NamedCommand> {
        self.commands.get(name).cloned()
    }

    /// Snapshot of every key-binding factory contributed by this
    /// runtime's extensions. The view's `default_keymap()` reads from
    /// this list and merges on top of the base 4 keymap entries
    /// (`Backspace`, `Delete`, `Enter`, `Mod-a`).
    pub fn merged_keymap_factories(&self) -> &[(String, KeyBindingFactory)] {
        &self.key_bindings
    }

    /// Plugins this runtime contributes to `EditorState::create`.
    /// Includes the base `HistoryExtension`'s `history_plugin` unless
    /// a builder explicitly opted out via `without_defaults`.
    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    /// Is `name` a list-item-shaped node type contributed by any
    /// extension in this runtime? Consulted by the list-conversion
    /// fast path in `commands::wrap_in_list`.
    pub fn is_list_item_type(&self, name: &str) -> bool {
        self.list_item_types.contains(name)
    }

    /// Snapshot of every list-item-shaped node type. Used by the
    /// `enclosing_list_ancestor` fast path that drives bullet ↔ task ↔
    /// ordered conversions.
    pub fn list_item_type_names(&self) -> &HashSet<String> {
        &self.list_item_types
    }
}

impl std::fmt::Debug for EditorRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorRuntime")
            .field("name", &self.name)
            .field(
                "extensions",
                &self.extensions.iter().map(|e| e.name()).collect::<Vec<_>>(),
            )
            .field("node_view_count", &self.node_views.len())
            .field("command_count", &self.commands.len())
            .field("key_binding_count", &self.key_bindings.len())
            .field("plugin_count", &self.plugins.len())
            .field("list_item_types", &self.list_item_types)
            .finish()
    }
}
