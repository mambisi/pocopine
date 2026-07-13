//! Pluggable content formats for the editor surface.
//!
//! [`Editor::set`](super::Editor::set), [`Editor::get`](super::Editor::get),
//! and [`Editor::on_update`](super::Editor::on_update) are
//! generic over a [`ContentFormat`]. Built-in implementations:
//!
//! - [`Markdown`] (`type Repr = str`, `type Owned = String`):
//!   uses the runtime's
//!   [`markdown_parser`](crate::runtime::EditorRuntime::markdown_parser)
//!   / [`markdown_serializer`](crate::runtime::EditorRuntime::markdown_serializer).
//! - [`Doc`] (`type Repr = Value`, `type Owned = Value`): raw
//!   ProseMirror-style doc JSON.
//!
//! Extension crates can ship their own (HTML, AsciiDoc, custom
//! DSLs, etc.) by implementing [`ContentFormat`] — the editor
//! handle methods pick up the new format with no changes to
//! `pine-richtext` itself.
//!
//! ```ignore
//! use pine_richtext::view::content::{ContentError, ContentFormat};
//! use pine_richtext::model::Node;
//! use pine_richtext::runtime::EditorRuntime;
//! use pine_richtext::state::EditorState;
//!
//! pub struct PlainText;
//!
//! impl ContentFormat for PlainText {
//!     type Repr = str;
//!     type Owned = String;
//!     fn name() -> &'static str { "plain-text" }
//!     fn parse(text: &str, runtime: &EditorRuntime)
//!         -> Result<Node, ContentError> { /* … */ todo!() }
//!     fn serialize(state: &EditorState, _: &EditorRuntime)
//!         -> Result<String, ContentError> { Ok(state.doc().text_content()) }
//! }
//!
//! // editor.get::<PlainText>() now returns the doc's plain-text content.
//! ```

use std::borrow::Borrow;
use std::fmt;

use serde_json::Value;

use crate::model::Node;
use crate::runtime::EditorRuntime;
use crate::state::EditorState;

/// Pluggable serialization format for the editor's doc.
///
/// Implementors convert between an external representation
/// (markdown text, doc JSON, HTML, …) and a doc [`Node`] valid
/// against the runtime's schema. The editor's `set` / `get` /
/// `on_update` methods are parameterized over this trait so
/// extensions can introduce new formats without touching
/// [`crate::view::Editor`].
pub trait ContentFormat {
    /// Borrowed input form. Usually [`str`] (for textual
    /// formats) or [`Value`] (for JSON-shaped formats).
    type Repr: ?Sized;
    /// Owned form returned by [`Self::serialize`] and taken by
    /// callers of `get::<F>()`. For `Repr = str` this is
    /// [`String`]; for `Repr = Value` it's [`Value`].
    type Owned: Borrow<Self::Repr>;

    /// Short identifier used in error messages and logs
    /// (e.g. `"markdown"`, `"doc"`, `"html"`).
    fn name() -> &'static str;

    /// Parse `value` against the runtime's schema into a doc
    /// [`Node`]. Returned errors carry only a stringified
    /// reason so the trait stays object-safe-ish and
    /// extension errors don't have to share a common type.
    fn parse(value: &Self::Repr, runtime: &EditorRuntime) -> Result<Node, ContentError>;

    /// Serialize `state.doc()` back to [`Self::Owned`].
    fn serialize(state: &EditorState, runtime: &EditorRuntime)
    -> Result<Self::Owned, ContentError>;
}

/// Parse-or-serialize failure inside a [`ContentFormat`]
/// implementation. Implementations stringify their native
/// error type into one of these variants — keeps the trait
/// agnostic to whichever error each format uses internally.
#[derive(Debug, Clone)]
pub enum ContentError {
    /// `parse(...)` rejected the input. Carries the
    /// implementation's stringified reason.
    Parse(String),
    /// `serialize(...)` couldn't produce the output form.
    Serialize(String),
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentError::Parse(msg) => write!(f, "content parse failed: {msg}"),
            ContentError::Serialize(msg) => write!(f, "content serialize failed: {msg}"),
        }
    }
}

impl std::error::Error for ContentError {}

/// Markdown — `type Repr = str`, `type Owned = String`. Uses
/// the runtime's
/// [`markdown_parser`](EditorRuntime::markdown_parser) and
/// [`markdown_serializer`](EditorRuntime::markdown_serializer).
pub struct Markdown;

impl ContentFormat for Markdown {
    type Repr = str;
    type Owned = String;

    fn name() -> &'static str {
        "markdown"
    }

    fn parse(value: &str, runtime: &EditorRuntime) -> Result<Node, ContentError> {
        runtime
            .markdown_parser()
            .parse(value, runtime.schema())
            .map_err(|err| ContentError::Parse(err.to_string()))
    }

    fn serialize(state: &EditorState, runtime: &EditorRuntime) -> Result<String, ContentError> {
        runtime
            .export_markdown(state.doc())
            .map_err(|err| ContentError::Serialize(err.to_string()))
    }
}

/// Raw doc JSON — `type Repr = Value`, `type Owned = Value`.
/// Matches the ProseMirror-style `{ type, attrs, content }`
/// shape produced by `Node`'s serde impl. Use when you already
/// hold an opaque `Value` snapshot (e.g. a cross-network
/// payload) and don't want to commit to a `Node` shape.
///
/// Callers that own typed `Node` data should reach for
/// [`DocNode`] instead — it skips the
/// `serde_json::to_value` / `from_value` hop.
pub struct Doc;

impl ContentFormat for Doc {
    type Repr = Value;
    type Owned = Value;

    fn name() -> &'static str {
        "doc"
    }

    fn parse(value: &Value, _runtime: &EditorRuntime) -> Result<Node, ContentError> {
        serde_json::from_value(value.clone()).map_err(|err| ContentError::Parse(err.to_string()))
    }

    fn serialize(state: &EditorState, _runtime: &EditorRuntime) -> Result<Value, ContentError> {
        serde_json::to_value(state.doc()).map_err(|err| ContentError::Serialize(err.to_string()))
    }
}

/// Typed doc — `type Repr = Node`, `type Owned = Node`.
/// Same wire shape as [`Doc`] (the storage layer uses `Node`'s
/// serde impl), but the editor handle returns / accepts the
/// node directly: no `serde_json::Value` intermediate, no
/// `to_value` / `from_value` round-trip per save and per load.
///
/// Prefer this over `Doc` when keep-style apps thread the doc
/// straight from `editor.get::<DocNode>()` into a typed
/// persistence field (e.g. `body_state: Option<Node>`).
pub struct DocNode;

impl ContentFormat for DocNode {
    type Repr = Node;
    type Owned = Node;

    fn name() -> &'static str {
        "doc-node"
    }

    fn parse(node: &Node, _runtime: &EditorRuntime) -> Result<Node, ContentError> {
        // Caller already owns a `Node`; nothing to parse.
        // Cloning is cheap — `Node`'s `Fragment` shares its
        // children via `Arc`, so this is just an Arc bump and
        // a small struct copy.
        Ok(node.clone())
    }

    fn serialize(state: &EditorState, _runtime: &EditorRuntime) -> Result<Node, ContentError> {
        Ok(state.doc().clone())
    }
}
