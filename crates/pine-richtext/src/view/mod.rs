//! Browser view layer for pine-richtext.
//!
//! Gated behind the `view` cargo feature so non-wasm consumers don't
//! pull in pocopine / wasm-bindgen / web-sys. Provides:
//! - [`PineRichTextRoot`] — pocopine component that renders an
//!   `EditorState` into a contentEditable surface and dispatches
//!   commands on keystrokes.
//! - [`selection`] — DOM ↔ model selection translation.
//! - [`input`] — keystroke + beforeinput handlers.
//!
//! The pure HTML serializer lives outside this module (in
//! [`crate::render`]) so server-side rendering can reuse it.

pub mod content;
mod event_boundary;
pub mod input;
pub mod interop;
mod node_view_handle;
mod node_view_manager;
mod reconciler;
pub mod root;
pub mod selection;
mod selection_observer;
pub mod typed_node_views;

pub use crate::render::render_doc_to_html;
pub use content::{ContentError, ContentFormat, Doc, DocNode, Markdown};
pub use interop::{ChangeInfo, DocChangeSubscription, Editor, EditorError};
pub use node_view_handle::{NodeCommand, NodeCommandTarget, NodeViewHandle, use_node_view_handle};
pub use root::{LOAD_ERROR_EVENT, PineRichTextRoot};
pub use selection_observer::{SelectionChangeSubscription, SelectionSnapshot, ViewportRect};
pub use typed_node_views::{
    NodeViewError, NodeViewHost, NodeViewKind, NodeViewSelection, NodeViewSpec, NodeViewUpdate,
    RichTextNodeView, RichTextViewExtension,
};
