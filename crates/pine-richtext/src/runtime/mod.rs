//! Per-`<pine-rich-text-root>` editor configuration.
//!
//! An [`EditorRuntime`] is an immutable bundle of everything an editor
//! mount needs: a [`Schema`], the chain of [`RichTextExtension`]s that
//! produced it, typed semantic/view registries, a
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

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::extension::{KeyBindingFactory, NamedCommand, RichTextExtension};
use crate::inputrules::InputRule;
use crate::markdown::{
    MarkEmitter as MarkdownMarkEmitter, MarkdownParseRule, NodeEmitter as MarkdownNodeEmitter,
    ParseMatch as MarkdownParseMatch,
};
use crate::model::Schema;
use crate::render::NodeDomSpec;
use crate::serialization::NodeSerializationSpec;
use crate::state::Plugin;
use crate::typed_nodes::{RichTextNodeType, TypedNodeSpec};
#[cfg(feature = "view")]
use crate::view::typed_node_views::{NodeViewSpec as TypedNodeViewSpec, TypedNodeViewRegistry};

pub mod builder;
pub mod registry;

#[cfg(test)]
mod tests;

pub use builder::{RuntimeBuildError, RuntimeBuilder};

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
    pub(crate) commands: HashMap<String, NamedCommand>,
    pub(crate) key_bindings: Vec<(String, KeyBindingFactory)>,
    pub(crate) plugins: Vec<Plugin>,
    pub(crate) list_item_types: HashSet<String>,
    pub(crate) input_rules: Vec<InputRule>,
    pub(crate) markdown_node_emitters: HashMap<String, MarkdownNodeEmitter>,
    pub(crate) markdown_mark_emitters: HashMap<String, MarkdownMarkEmitter>,
    pub(crate) markdown_parse_rules: HashMap<MarkdownParseMatch, Arc<MarkdownParseRule>>,
    pub(crate) typed_nodes: HashMap<String, TypedNodeSpec>,
    pub(crate) node_serialization: HashMap<TypeId, NodeSerializationSpec>,
    pub(crate) dom_views: HashMap<String, NodeDomSpec>,
    pub(crate) wire_descriptor: serde_json::Value,
    pub(crate) wire_fingerprint: String,
    #[cfg(feature = "view")]
    pub(crate) typed_node_views: TypedNodeViewRegistry,
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

    /// Canonical semantic schema/encoding descriptor used for collaboration
    /// compatibility negotiation.
    pub fn wire_descriptor(&self) -> &serde_json::Value {
        &self.wire_descriptor
    }

    /// SHA-256 of [`Self::wire_descriptor`]. Presentation-only changes are
    /// excluded, so component/CSS/DOM revisions do not split compatible rooms.
    pub fn wire_fingerprint(&self) -> &str {
        &self.wire_fingerprint
    }

    /// Snapshot of the extension chain in fold order. Cheap; intended
    /// for diagnostics.
    pub fn extensions(&self) -> &[Arc<dyn RichTextExtension>] {
        &self.extensions
    }

    /// Resolve a typed semantic descriptor by its persisted node name.
    pub fn lookup_typed_node(&self, node_type: &str) -> Option<&TypedNodeSpec> {
        self.typed_nodes.get(node_type)
    }

    /// Resolve a typed semantic descriptor and prove it is the exact Rust
    /// marker `N`, not merely another descriptor sharing `N::NAME`.
    pub fn typed_node<N: RichTextNodeType>(&self) -> Option<&TypedNodeSpec> {
        self.typed_nodes
            .get(N::NAME)
            .filter(|spec| spec.semantic_type_id() == std::any::TypeId::of::<N>())
    }

    /// Resolve the complete output policy for an exact typed semantic marker.
    pub fn node_serialization<N: RichTextNodeType>(&self) -> Option<&NodeSerializationSpec> {
        self.node_serialization.get(&TypeId::of::<N>())
    }

    /// Resolve output policy from a persisted name only after that name has
    /// been resolved to the runtime's exact semantic `TypeId`.
    pub fn lookup_node_serialization(&self, node_type: &str) -> Option<&NodeSerializationSpec> {
        let typed = self.typed_nodes.get(node_type)?;
        self.node_serialization.get(&typed.semantic_type_id())
    }

    /// Resolve the validated native/fallback DOM view for a semantic node.
    pub fn lookup_dom_view(&self, node_type: &str) -> Option<&NodeDomSpec> {
        self.dom_views.get(node_type)
    }

    /// Resolve the typed component view paired with a semantic node in this
    /// runtime.
    #[cfg(feature = "view")]
    pub fn lookup_typed_node_view(&self, node_type: &str) -> Option<&TypedNodeViewSpec> {
        self.typed_node_views.get(node_type)
    }

    #[cfg(feature = "view")]
    pub fn typed_node_view_count(&self) -> usize {
        self.typed_node_views.len()
    }

    /// Look up a named command. Used by the
    /// `pine:richtext:command`'s `Custom { name, args }` dispatch path.
    pub fn named_command(&self, name: &str) -> Option<NamedCommand> {
        self.commands.get(name).cloned()
    }

    /// Snapshot of every key-binding factory contributed by this
    /// runtime's extensions. The view's `default_keymap()` reads from
    /// this list and merges on top of the base 4 keymap entries
    /// (`Backspace`, `Delete`, `Enter`, `ArrowLeft`, `ArrowRight`, `Mod-a`).
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

    /// Merged input rules contributed by this runtime's extensions
    /// in registration order. The view-side `beforeinput` hook
    /// consults this list (via [`crate::inputrules::run_rules`])
    /// before defaulting to a plain text insert.
    pub fn input_rules(&self) -> &[InputRule] {
        &self.input_rules
    }

    /// Build a [`crate::markdown::MarkdownSerializer`] pre-populated
    /// with this runtime's per-node-type markdown emitters.
    ///
    /// The emitter table is pre-folded at build time with **user-
    /// first** precedence (same semantics as commands / keymaps):
    /// a user extension whose `markdown_node_emitters()` returns
    /// an entry for `task_item` shadows the default
    /// `TaskListExtension`'s entry for the same node type. This
    /// call just clones the pre-built table into a fresh serializer
    /// — cheap because every emitter is held by `Arc`.
    pub fn markdown_serializer(&self) -> crate::markdown::MarkdownSerializer {
        let mut serializer = crate::markdown::MarkdownSerializer::new();
        for (type_name, emitter) in &self.markdown_node_emitters {
            serializer = serializer.register_node(type_name.clone(), emitter.clone());
        }
        for (type_name, emitter) in &self.markdown_mark_emitters {
            serializer = serializer.register_mark(type_name.clone(), emitter.clone());
        }
        serializer
    }

    /// Build a [`crate::markdown::MarkdownParser`] for this
    /// runtime, pre-populated with parse rules contributed by
    /// every extension's
    /// [`crate::extension::RichTextExtension::markdown_parse_rules`].
    /// Extension-contributed rules SHADOW the parser's built-in
    /// handling for the same event, so apps can override default
    /// behavior (in addition to recognizing novel shapes like
    /// tables or callouts).
    ///
    /// The schema accompanying the parser is `self.schema()`, so
    /// task-list parsing automatically adapts: schemas that
    /// declare `task_list`/`task_item` get proper task nodes;
    /// schemas that don't fall back to `bullet_list` (logged as
    /// a warning).
    pub fn markdown_parser(&self) -> crate::markdown::MarkdownParser {
        use crate::markdown::{ParseMatch, TagKind};
        let mut parser = crate::markdown::MarkdownParser::new();
        parser.rules = self.markdown_parse_rules.clone();
        // Enable `ENABLE_TABLES` only when a Table parse rule is
        // registered — otherwise pipe-table markdown would be
        // tokenized into Tag::Table* events the walker can't
        // handle, dropping cells into loose paragraphs.
        if parser.rules.contains_key(&ParseMatch::Tag(TagKind::Table)) {
            parser
                .options
                .insert(pulldown_cmark::Options::ENABLE_TABLES);
        }
        parser
    }
}

impl std::fmt::Debug for EditorRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("EditorRuntime");
        debug
            .field("name", &self.name)
            .field(
                "extensions",
                &self.extensions.iter().map(|e| e.name()).collect::<Vec<_>>(),
            )
            .field("typed_node_count", &self.typed_nodes.len());
        debug.field("wire_fingerprint", &self.wire_fingerprint);
        debug.field("dom_view_count", &self.dom_views.len());
        #[cfg(feature = "view")]
        debug.field("typed_node_view_count", &self.typed_node_view_count());
        debug
            .field("command_count", &self.commands.len())
            .field("key_binding_count", &self.key_bindings.len())
            .field("plugin_count", &self.plugins.len())
            .field("list_item_types", &self.list_item_types)
            .finish()
    }
}
