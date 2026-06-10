//! Host-side runtime (RFC-093 §D2). Everything server-side lives here: the
//! `Agenkit`/`Ai` builders, providers, and — across later checkpoints —
//! tools/retrievers/embedders, flows, agents, threads, parallel orchestration,
//! trace emission, the server-function bridge, and the streaming route.
//!
//! This whole module is gated to non-wasm targets (see `lib.rs`); the browser
//! talks to it only through generated `#[server]` helpers (§D10).

pub mod observe;
pub mod provider;

pub use observe::{emit_trace_event, to_observed_event};
pub use provider::{
    BoxFuture, BoxStream, FinishReason, GenerateRequest, GenerateResponse, MockProvider, Provider,
    ProviderCapabilities, ProviderRegistry, StreamChunk,
};
