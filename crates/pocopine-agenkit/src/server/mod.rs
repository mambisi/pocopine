//! Host-side runtime (RFC-093 §D2). Everything server-side lives here: the
//! `Agenkit`/`Ai` builders, providers, and — across later checkpoints —
//! tools/retrievers/embedders, flows, agents, threads, parallel orchestration,
//! trace emission, the server-function bridge, and the streaming route.
//!
//! This whole module is gated to non-wasm targets (see `lib.rs`); the browser
//! talks to it only through generated `#[server]` helpers (§D10).

pub mod agenkit;
pub mod agent;
pub mod context;
pub mod embed;
pub mod flow;
pub mod generate;
pub mod observe;
pub mod parallel;
mod partial_json;
pub mod provider;
pub mod reduce;
pub mod retrieval;
pub mod run;
mod schema;
pub mod thread;
pub mod tool;

pub use agenkit::{with_principal, Agenkit, AgenkitBuilder};
pub use agent::{AgentRun, AiAgent, AiAgentBuilder};
pub use context::{AiContext, AiToolContext, AppState, EmbedContext, RetrievalContext};
pub use embed::{AiEmbedder, DynEmbedder, EmbedderRegistry};
pub use flow::{AiFlowContext, Flow, FlowHandler, FlowRegistry, RetrieveBuilder};
pub use generate::{Ai, AiStructured};
pub use observe::{emit_trace_event, to_observed_event};
pub use parallel::ParallelBuilder;
pub use provider::{
    BoxFuture, BoxStream, FinishReason, GenerateRequest, GenerateResponse, MockProvider, Provider,
    ProviderCapabilities, ProviderRegistry, StreamChunk,
};
pub use reduce::ReduceBuilder;
pub use retrieval::{retriever_as_tool, AiRetriever, DynRetriever, RetrieverRegistry};
pub use thread::{AgentThreadHandle, AgentThreadStore, InMemoryThreadStore, ThreadBuilder};
pub use tool::{AiTool, DynTool, ToolRegistry};
