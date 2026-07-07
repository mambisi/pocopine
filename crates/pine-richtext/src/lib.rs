//! Rust-native rich text model, transform, and editor state primitives for Pine.
//!
//! This crate ports the pure ProseMirror core chain into Rust-shaped APIs:
//! document model types, replace/mark/attribute transforms, an editor state
//! layer with pure plugin hooks, and a ported `commands` module. It
//! intentionally does not include a DOM editor view, Pocopine component
//! surface, markdown parser, history, input rules, menu, or collaboration
//! layer.
//!
//! The implementation is inspired by the MIT-licensed ProseMirror packages
//! `prosemirror-model`, `prosemirror-transform`, `prosemirror-state`, and
//! `prosemirror-schema-basic`. See `NOTICE.prosemirror.md` for attribution.

mod error;

pub mod commands;
pub mod extension;
pub mod extensions;
pub mod history;
pub mod inputrules;
pub mod markdown;
pub mod model;
pub mod render;
pub mod runtime;
pub mod schema_basic;
pub mod state;
pub mod transform;

/// Pure single-span text diff powering the reconciler's incremental text-node
/// patch. Compiled with the `view` feature (its only consumer) or under `test`
/// (so its unit/property tests run natively in CI without a browser).
#[cfg(any(feature = "view", test))]
mod text_diff;

/// Browser view layer. Renders an `EditorState` into a contentEditable
/// surface, captures keystrokes, and translates the DOM selection back
/// into a model `Selection`. Gated behind the `view` feature; enable it
/// in apps that target wasm and want an editable surface:
///
/// ```toml
/// pine-richtext = { workspace = true, features = ["view"] }
/// ```
#[cfg(feature = "view")]
pub mod view;

pub use error::{RichTextError, RichTextResult};
