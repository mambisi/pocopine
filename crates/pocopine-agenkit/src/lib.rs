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

/// The curated host-side authoring prelude (`use pocopine_agenkit::prelude::*`).
#[cfg(not(target_arch = "wasm32"))]
pub mod prelude;

/// Re-exported so apps can derive `JsonSchema` on structured-output types
/// without depending on `schemars` directly.
#[cfg(not(target_arch = "wasm32"))]
pub use schemars;

/// Re-exported for the `#[ai_flow]` macro's generated `FlowHandler` impl, whose
/// `run_json` names `serde_json::Value`. Not part of the public API.
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub use serde_json;

/// Authoring macros (RFC-093 §D14): `#[ai_tool]` derives an `AiTool` from a
/// typed async fn; `#[ai_flow]` turns a flow body into a `FlowHandler`
/// constructor with its manifest declared in the attribute. Server-only — the
/// generated code references the host runtime.
#[cfg(not(target_arch = "wasm32"))]
pub use pocopine_agenkit_macros::{ai_flow, ai_tool};
