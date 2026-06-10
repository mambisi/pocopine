//! `pocopine-agenkit` — the unified runtime facade for
//! [Pocopine Agenkit](../../rfcs/rfc-093-pocopine-agenkit.md), the
//! Pocopine-native generative-AI runtime.
//!
//! This is the crate app authors depend on. It owns the runtime surface —
//! `Agenkit`/`Ai` builders, provider traits and registry, flow registry,
//! tool/retriever/embedder execution, agents and threads, parallel
//! orchestration, trace recording, redaction policy, and server-function
//! helpers — and re-exports the stable contracts from
//! [`pocopine_agenkit_core`].
//!
//! The runtime maps the neutral [`pocopine_agenkit_core`] trace payloads
//! onto `pocopine_observe::ObservedEvent` + `emit_tracing`, so AI events
//! ride the existing Pocopine observability stack rather than a parallel
//! telemetry runtime (RFC-093 §D12).
//!
//! # Module layout
//!
//! - [`shared`] — the contract vocabulary both sides exchange (compiles on
//!   every target).
//! - [`client`] — the client-side surface (typed flow call + stream consumer);
//!   compiles on every target.
//! - [`server`] — the host-side runtime; gated to non-wasm targets so the
//!   workspace `wasm32-unknown-unknown` build keeps compiling this crate.
//!   Larger server submodules are split into files under `server/`.
//!
//! Browser clients call public flows through generated `#[server]` helpers,
//! never the [`server`] runtime directly (§D10).
//!
//! A curated `prelude` and root re-exports land as the final checkpoint of
//! Phase 2, once the surface stabilises (RFC-093 §D14).
#![forbid(unsafe_code)]

pub mod client;
pub mod shared;

#[cfg(not(target_arch = "wasm32"))]
pub mod server;

/// Re-exported so apps can derive `JsonSchema` on structured-output types
/// without depending on `schemars` directly.
#[cfg(not(target_arch = "wasm32"))]
pub use schemars;
