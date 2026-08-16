//! Model catalog (roadmap W1) — per-model metadata keyed by [`ModelRef`].
//!
//! A [`ModelRef`](pocopine_agenkit_core::ModelRef) is only a `"provider/model"`
//! alias; it carries no context window, price, or capability. This module
//! resolves an alias to a [`Model`] descriptor so the runtime can meter cost,
//! manage the context window, and pick reasoning behaviour. It is **host-only**
//! (the whole `server` module is gated off wasm) — never shipped to the client.
//!
//! Layout:
//! - [`catalog`](self) (this file) wires the pieces together;
//! - `catalog.rs` is the hand-written core — [`Model`]/[`ModelPricing`] and the
//!   [`lookup`]/[`all`] accessors;
//! - `generated.rs` is **generated** — the model descriptors (`generated()`,
//!   each `id` referencing a [`models`] const) plus the typed [`models`] const
//!   handles themselves.
//!
//! The descriptors and handles are derived from LiteLLM's open price/capability
//! data by `tools/gen-model-catalog` and committed to the repo (git-tracked), so
//! there is no hand-built fallback. Refresh with `cargo run -p gen-model-catalog`.
//!
//! Downstream consumers (cost metering, context-overflow detection) call
//! [`lookup`]; an unknown alias resolves to `None` and those features degrade
//! rather than error.

// The hand-written core; the file name mirrors this `catalog` module path on
// purpose (the generated `generated.rs` sits alongside it).
#[allow(clippy::module_inception)]
mod catalog;
mod generated;
mod generation;

pub use catalog::{Model, ModelPricing, all, lookup};
pub use generation::{
    GenerationKind, GenerationModel, ImageGenerationConfig, VideoGenerationConfig,
    generation_models, lookup_generation,
};

/// Typed `ModelRef` const handles for the known models, grouped by provider
/// (`models::anthropic::CLAUDE_OPUS_4_8`). Generated from the same LiteLLM data
/// as the descriptors, so they never drift; an open-weight model the catalog
/// doesn't index still uses `ModelRef::new("provider/model")`.
pub use generated::models;
