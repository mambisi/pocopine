//! Extension contract for pine-richtext.
//!
//! A [`RichTextExtension`] bundles everything a plugin contributes to
//! the editor in one trait: node specs, mark specs, named commands
//! reachable over the `pine:richtext:command` CustomEvent, keystroke
//! bindings, custom-element node views, and state plugins.
//!
//! Extensions register process-globally via [`registry::register`] on
//! the same lifecycle as `pocopine::App::register::<T>()` — before
//! `App::run()`. Once the schema has been realized by
//! [`crate::schema_basic::schema`], further `register` calls panic so
//! build-time misorder surfaces immediately.
//!
//! Phase 4's minimal-disruption shape: extensions install into the
//! existing process-global registries (`schema_basic`, `node_views`,
//! plus the new command/keymap tables here). Per-instance scoping is
//! Phase 4b. Two editors on the same page share one schema and one
//! extension set.
//!
//! See `docs/extensions.md` for a worked example (added in
//! `Phase 4 commit 4`).

use std::sync::Arc;

use serde_json::Value;

use crate::commands::BoxedCommand;
use crate::model::{MarkSpec, NodeSpec};
use crate::state::Plugin;

pub mod registry;

/// Convenience re-export so apps can write
/// `pine_richtext::extension::register(...)` without naming the
/// registry submodule. Identical to [`registry::register`].
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

/// Custom-element binding for a node type. When the renderer encounters
/// a node whose type is registered here, it emits `<tag>` instead of
/// the default block tag. If `content_selector` is set, the reconciler
/// looks for that selector inside the custom element when threading
/// inline content; otherwise the element itself is treated as the
/// content host.
///
/// Forwarded eagerly into [`crate::render::node_views`] at registration
/// time (see [`registry::register`]) — *not* lazily inside the schema
/// fold, so the reconciler always sees the node-view tag regardless of
/// when `schema()` is first called.
pub struct ExtensionNodeView {
    /// Model node-type name (e.g. `"task_item"`).
    pub node_type: String,
    /// Custom-element tag (e.g. `"pine-task-item"`).
    pub tag: String,
    /// CSS selector identifying the content host inside the custom
    /// element; `None` means the element itself.
    pub content_selector: Option<String>,
}

/// Single point of contribution for a pine-richtext extension. Every
/// method defaults to "contributes nothing" so an extension only
/// overrides what it needs.
///
/// The registration order determines:
///   * Node insertion rank in `schema_basic::schema()` (matters for
///     content-match resolution).
///   * Keymap first-wins for overlapping combos. The base keymap is
///     installed first, so extensions cannot shadow `Backspace`,
///     `Delete`, `Enter`, or `Mod-a`.
///   * Command-name first-wins (a later extension cannot overwrite an
///     earlier extension's `wrap_in_bullet_list`, e.g.).
pub trait RichTextExtension: 'static + Send + Sync {
    /// Stable identifier. Two extensions sharing a `name()` is a
    /// build error — the second registration is dropped with a warning.
    fn name(&self) -> &str;

    /// Node specs this extension contributes. Folded into
    /// `schema_basic::schema()` in registration order.
    fn nodes(&self) -> Vec<NodeSpec> {
        Vec::new()
    }

    /// Mark specs this extension contributes.
    fn marks(&self) -> Vec<MarkSpec> {
        Vec::new()
    }

    /// Keystroke bindings contributed to the default keymap.
    fn key_bindings(&self) -> KeyBindings {
        Vec::new()
    }

    /// Named commands reachable via `pine:richtext:command`'s
    /// `Custom { name, args }` variant.
    fn commands(&self) -> Vec<(String, NamedCommand)> {
        Vec::new()
    }

    /// Per-node-type custom-element bindings.
    fn node_views(&self) -> Vec<ExtensionNodeView> {
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
    /// `crate::extensions::MarkdownShortcutsExtension` (Phase 5 C3).
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
