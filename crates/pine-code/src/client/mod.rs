//! Browser-owned input surface; the document remains owned by the Rust core.
mod component;
mod dom_reader;
mod drag;
mod events;
mod handle;
mod input;
#[cfg(test)]
mod lifecycle_tests;
mod native_change;
mod runtime;
#[cfg(test)]
mod tests;
mod view;

pub use component::PineCodeEditor;
pub use events::Subscription;
pub use handle::CodeEditorHandle;
pub use input::{CodeCommand, KeyBinding};
pub use runtime::{
    CodeFinalSnapshot, CodeMetrics, CodeOptions, CodeSnapshot, CompositionStatus,
    InterruptedComposition,
};
