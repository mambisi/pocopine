//! Extension contract for pine-richtext.
//!
//! A [`RichTextExtension`] bundles the target-independent parts of an editor
//! extension: semantic node/mark specs, native DOM and serialization policies,
//! named commands, key bindings, input/Markdown rules, and state plugins.
//!
//! The primary composition boundary is an immutable
//! [`crate::runtime::EditorRuntime`]. Build one with
//! [`crate::runtime::RuntimeBuilder::with`], or use `with_view` for an
//! extension that also implements the browser-only `RichTextViewExtension`.
//! Typed component views are validated and stored inside that runtime; there is
//! no process-global node-view registry. Different editors on one page may use
//! different runtime values and therefore different extension/view sets.
//!
//! [`registry::register`] remains as a deprecated process-global bootstrap for
//! adding model extensions to the lazily built **default** runtime. It must run
//! before any runtime is resolved, cannot carry typed component-view
//! contributions through its erased model trait, and does not affect explicit
//! runtimes. See `docs/extensions.md` for current composition examples.

use std::sync::Arc;

use serde_json::Value;

use crate::TypedNodeSpec;
use crate::commands::BoxedCommand;
use crate::model::{MarkSpec, NodeSpec};
use crate::render::NodeDomSpec;
use crate::serialization::NodeSerializationSpec;
use crate::state::Plugin;

pub mod registry;

/// Legacy convenience re-export so apps can write
/// `pine_richtext::extension::register(...)` without naming the
/// registry submodule. Identical to the deprecated [`registry::register`]; new
/// code should compose an explicit runtime.
#[allow(deprecated)]
pub use registry::register;

#[cfg(test)]
mod tests;

/// Builds a fresh [`BoxedCommand`] for a key binding. Called once per
/// `default_keymap()` build; the returned command owns its closure for
/// the keymap's lifetime.
pub type KeyBindingFactory = Arc<dyn Fn() -> BoxedCommand + Send + Sync>;

/// One extension-contributed key binding. `combo` follows the format
/// produced by [`crate::view::input::key_combo`] (e.g. `"Mod-z"`,
/// `"Mod-Shift-z"`, `"Backspace"`).
pub type KeyBinding = (String, KeyBindingFactory);
/// Vec alias used by [`RichTextExtension::key_bindings`].
pub type KeyBindings = Vec<KeyBinding>;

/// Builds a [`BoxedCommand`] from JSON args supplied over the wire by
/// `pine:richtext:command`'s `Custom { name, args }` variant. Returns
/// `None` when the args don't fit the command's shape; the dispatcher
/// treats that the same as a non-applicable command (silent no-op).
pub type NamedCommand = Arc<dyn Fn(Value) -> Option<BoxedCommand> + Send + Sync>;

/// Single point of contribution for a pine-richtext extension. Every
/// method defaults to "contributes nothing" so an extension only
/// overrides what it needs.
///
/// The runtime's extension fold order determines:
///   * Node insertion rank in that runtime's schema (matters for
///     content-match resolution).
///   * Keymap first-wins for overlapping combos. The base keymap is
///     installed first, so extensions cannot shadow `Backspace`,
///     `Delete`, `Enter`, `ArrowLeft`, `ArrowRight`, or `Mod-a`.
///   * Command-name first-wins (a later extension cannot overwrite an
///     earlier extension's `wrap_in_bullet_list`, e.g.).
pub trait RichTextExtension: 'static + Send + Sync {
    /// Stable identifier used for runtime overlay. In a
    /// [`crate::runtime::RuntimeBuilder`], a later extension with the same name
    /// shadows the earlier one. The deprecated process-global registry instead
    /// preserves its historical first-wins behavior.
    fn name(&self) -> &str;

    /// Node specs this extension contributes. Folded into each
    /// [`crate::runtime::EditorRuntime`] in extension order.
    fn nodes(&self) -> Vec<NodeSpec> {
        Vec::new()
    }

    /// Typed semantic node descriptors contributed by this extension.
    ///
    /// Unlike [`Self::nodes`], each entry retains the exact Rust semantic
    /// marker identity, closed serde attr-key set, wire version, and migration
    /// metadata. Component-backed external nodes should use this lane so a
    /// later node-view registration can be checked against the semantic type
    /// rather than matching only a user-authored string.
    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        Vec::new()
    }

    /// Deterministic native/fallback DOM views for typed semantic nodes.
    fn dom_views(&self) -> Vec<NodeDomSpec> {
        Vec::new()
    }

    /// Complete persistence/output policies for exact typed semantic nodes.
    ///
    /// Every entry returned by [`Self::typed_nodes`] must have exactly one
    /// matching declaration. Each output lane is explicit: supported with a
    /// closed policy, or [`Unsupported`](crate::serialization::MarkdownPolicy::Unsupported)
    /// where the format cannot preserve the node. Runtime construction rejects
    /// missing, duplicate, type-mismatched, or incomplete declarations.
    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        Vec::new()
    }

    /// Mark specs this extension contributes.
    fn marks(&self) -> Vec<MarkSpec> {
        Vec::new()
    }

    /// Keystroke bindings contributed to the runtime's merged keymap.
    fn key_bindings(&self) -> KeyBindings {
        Vec::new()
    }

    /// Named commands reachable via `pine:richtext:command`'s
    /// `Custom { name, args }` variant.
    fn commands(&self) -> Vec<(String, NamedCommand)> {
        Vec::new()
    }

    /// State plugins this extension contributes (e.g. history).
    fn plugins(&self) -> Vec<Plugin> {
        Vec::new()
    }

    /// Node-type names this extension contributes as **list item shape**
    /// — block-level children of a list wrapper (e.g. `list_item`,
    /// `task_item`). Consulted by the list-conversion fast path in
    /// `commands::wrap_in_list` to detect whether the selection is
    /// already inside a list of any item type, so cross-item-type
    /// conversions (bullet ↔ task) take the in-place swap path
    /// instead of the slow `find_wrapping` BFS.
    ///
    /// Empty by default; built-in `ListsExtension` returns
    /// `&["list_item"]` and `TaskListExtension` returns `&["task_item"]`.
    /// A custom "callout list" extension would return
    /// `&["callout_item"]` and participate in the contract without
    /// touching pine core.
    ///
    /// Returning a `&'static [&'static str]` lets the
    /// `is_list_item_type` slow path iterate without allocating per
    /// extension — the table is static data, not built per call.
    fn list_item_types(&self) -> &'static [&'static str] {
        &[]
    }

    /// Input rules this extension contributes to the runtime's
    /// typing-time rule list. Empty by default. The view-side
    /// `beforeinput` hook consults the merged rule list (via the
    /// runtime accessor) before defaulting to a plain text insert.
    ///
    /// Smart-typography rules (em-dash, ellipsis, smart quotes) ship
    /// in `crate::extensions::SmartTypographyExtension`; markdown
    /// block shortcuts (`# `, `* `, `> `, etc.) ship in
    /// `crate::extensions::MarkdownShortcutsExtension`.
    fn input_rules(&self) -> Vec<crate::inputrules::InputRule> {
        Vec::new()
    }

    /// Markdown emitter overrides for custom node types this
    /// extension contributes. Each entry is `(node_type_name,
    /// emitter)`. The folded list is consumed by
    /// [`crate::runtime::EditorRuntime::markdown_serializer`] when
    /// apps build a serializer from the runtime — letting custom
    /// node types render to markdown without the app having to
    /// register an emitter manually.
    ///
    /// Empty by default — extensions whose node types are already
    /// covered by `markdown::MarkdownSerializer`'s built-in walker
    /// (paragraph, heading, blockquote, lists, code_block, image,
    /// hard_break) need no overrides. Extensions adding novel
    /// shapes (task lists, callouts, mention chips) supply
    /// emitters that push the right pulldown-cmark events.
    fn markdown_node_emitters(&self) -> Vec<(String, crate::markdown::NodeEmitter)> {
        Vec::new()
    }

    /// Markdown emitter overrides for mark types this extension
    /// contributes. Each entry is `(mark_type_name, emitter)`
    /// where the emitter returns a [`crate::markdown::MarkRender`]
    /// — either a `Tag` pair the inner content is wrapped in, or
    /// an atomic `Event` that replaces the text run.
    ///
    /// Empty by default. The built-in `em` / `strong` / `link` /
    /// `code` marks are handled inside the serializer when no
    /// emitter overrides them; extensions adding strikethrough,
    /// highlight, underline-as-HTML, etc. register here.
    fn markdown_mark_emitters(&self) -> Vec<(String, crate::markdown::MarkEmitter)> {
        Vec::new()
    }

    /// Markdown parse rules this extension contributes. Each rule
    /// claims a specific pulldown-cmark event and describes how
    /// to map it to a model construct (block, mark, leaf, or a
    /// custom callback). The folded rule table is consumed by
    /// [`crate::runtime::EditorRuntime::markdown_parser`].
    ///
    /// Empty by default. Extensions with novel shapes register
    /// rules here so apps can import markdown matching those
    /// shapes; built-in CommonMark shapes (paragraph, heading,
    /// list, image, …) work without any rule because the parser
    /// has built-in handling. Rules SHADOW the built-in handling
    /// if they claim the same event, so extensions can also
    /// override defaults.
    fn markdown_parse_rules(&self) -> Vec<crate::markdown::MarkdownParseRule> {
        Vec::new()
    }
}
