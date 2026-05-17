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

pub mod input;
mod reconciler;
pub mod root;
pub mod selection;

pub use crate::render::node_views as node_view;
pub use crate::render::render_doc_to_html;
pub use root::PineRichTextRoot;
