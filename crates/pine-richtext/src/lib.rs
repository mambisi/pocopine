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
pub mod history;
pub mod model;
pub mod schema_basic;
pub mod state;
pub mod transform;

pub use error::{RichTextError, RichTextResult};
