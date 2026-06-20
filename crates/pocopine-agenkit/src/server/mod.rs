//! Host-side runtime (RFC-093 §D2). Everything server-side lives here: the
//! `Agenkit`/`Ai` builders, providers, and — across later checkpoints —
//! tools/retrievers/embedders, flows, agents, threads, parallel orchestration,
//! trace emission, the server-function bridge, and the streaming route.
//!
//! This whole module is gated to non-wasm targets (see `lib.rs`); the browser
//! talks to it only through generated `#[server]` helpers (§D10).

pub mod agenkit;
pub mod agent;
pub mod bridge;
pub mod catalog;
pub mod context;
pub mod credentials;
pub mod embed;
pub mod flow;
pub mod generate;
/// Shared model↔tool loop primitives (the model call + tool dispatch + a trace
/// observer) used by both the single-shot typed agent loop and the long-lived
/// runtime, so the engine lives once and both lifecycles emit the same trace.
mod loop_core;
pub mod oauth;
pub mod observe;
pub mod overflow;
pub mod parallel;
mod partial_json;
pub mod plugin;
pub mod provider;
pub mod reduce;
pub mod retrieval;
pub mod run;
/// The long-lived, multi-turn agent runtime (Layer 2): a conversational
/// `AgentSession` over the W7 session layer, with a typed event firehose and
/// (L3) steering hooks.
pub mod runtime;
mod schema;
/// Durable, branchable conversation sessions (roadmap W7) — a single-writer
/// append-only event log with parent-pointer forks and compaction checkpoints.
pub mod session;
pub mod thread;
pub mod tool;

pub use agenkit::{Agenkit, AgenkitBuilder, FlowCall, FlowKey, TypedFlowCall, with_principal};
pub use agent::{AgentRun, AiAgent, AiAgentBuilder};
pub use bridge::{stream_filter, to_server_error};
pub use catalog::{Model, ModelPricing, models};
pub use context::{AiContext, AiToolContext, AppState, EmbedContext, RetrievalContext};
pub use credentials::{EnvCredentials, ProviderCredential, ProviderCredentials, SecretString};
pub use embed::{AiEmbedder, DynEmbedder, EmbedderRegistry};
pub use flow::{AiFlowContext, Flow, FlowDef, FlowHandler, FlowRegistry, RetrieveBuilder};
pub use generate::{Ai, AiStructured};
pub use oauth::{
    Authorization, OAuthConfig, OAuthCredentials, OAuthError, OAuthToken, OAuthTokenStore,
    StoreFuture, begin_authorization, complete_authorization, refresh,
};
pub use observe::{emit_trace_event, to_observed_event};
pub use overflow::{ContextHeadroom, context_headroom, estimate_input_tokens, is_context_overflow};
pub use parallel::ParallelBuilder;
pub use plugin::{PrincipalLayer, PrincipalService, agenkit_server_plugin, principal_layer};
pub use pocopine_agenkit_core::FlowDescriptor;
// Re-exported for apps implementing `ProviderCredentials` (the `resolve` callback
// keys on the caller `Principal`) and for the principal-scoping seam (§D15 DC-5).
pub use pocopine_auth::{AuthUser, Principal};
pub use provider::{
    BoxFuture, BoxStream, FinishReason, GenerateRequest, GenerateResponse, MockProvider, Provider,
    ProviderCapabilities, ProviderContext, ProviderRegistry, StreamChunk,
};
pub use reduce::ReduceBuilder;
pub use retrieval::{AiRetriever, DynRetriever, RetrieverRegistry, retriever_as_tool};
pub use runtime::{
    AbortHandle, AgentConfig, AgentEvent, AgentSession, AgentSessionBuilder, StopReason,
    ToolDecision,
};
pub use thread::{
    AgentThreadHandle, AgentThreadStore, SessionThreadStore, ThreadBuilder, ThreadOwner,
};
pub use tool::{AiTool, DynTool, ToolRegistry};
