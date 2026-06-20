//! `pocopine-agenkit-core` — stable, dependency-light contracts for
//! [Pocopine Agenkit](../../rfcs/rfc-093-pocopine-agenkit.md), the
//! Pocopine-native generative-AI runtime.
//!
//! This crate holds the shared payload and descriptor types that flow
//! between the runtime facade ([`pocopine-agenkit`]), generated clients,
//! tests, and future tooling: model/provider refs, content/messages,
//! tool/retriever/embedder descriptors, flow + orchestration types,
//! trace/stream payloads, usage/cost metadata, and `AgenkitError`.
//!
//! Design rules (RFC-093 §D2, §D10, §D12):
//!
//! * **Dependency-light and wasm-friendly.** No provider SDKs, no async
//!   runtime, no `tracing`, no server dependency. The same client-safe
//!   payloads must compile for `wasm32-unknown-unknown`.
//! * **Observe-neutral.** Core defines neutral [`TraceEvent`]/[`TraceSpan`]
//!   payloads and stable event-name constants; the facade owns the mapping
//!   onto `pocopine_observe::ObservedEvent` + `emit_tracing`. Core never
//!   depends on `pocopine-observe`.
//! * **No secret-bearing client config.** Payloads carry model/provider
//!   *aliases* and schema metadata, never provider credentials.
//!
//! Normal apps depend on the [`pocopine-agenkit`] facade, which re-exports
//! the stable surface of this crate. Internal tests, generated clients, and
//! future tooling can import this crate directly for the payload types.
//!
//! [`pocopine-agenkit`]: https://docs.rs/pocopine-agenkit
//! [`TraceEvent`]: crate
//! [`TraceSpan`]: crate
//!
//! # Status
//!
//! Phase 1 in progress: identifiers, leaf enums, and the error type are in
//! place (see the RFC's Implementation Plan for the remaining checkpoints).
#![forbid(unsafe_code)]

pub mod agent;
pub mod content;
pub mod embed;
pub mod enums;
pub mod error;
pub mod events;
pub mod flow;
pub mod id;
pub mod manifest;
pub mod reduce;
pub mod retrieval;
pub mod schema;
pub mod sse;
pub mod stream;
pub mod thread;
pub mod tool;
pub mod tool_name;
pub mod trace;

pub use agent::AiAgentDescriptor;
pub use content::{Content, ContentPart, MediaPart, Message};
pub use embed::{EmbedderDescriptor, Embedding, EmbeddingBatch};
pub use enums::{
    BranchFailurePolicy, ParallelJoin, ReducerKind, ResourceKind, Role, StepKind, StepStatus,
    StreamMode, ThinkingLevel, ThreadRetention, ToolSideEffectPolicy,
};
pub use error::{AgenkitError, AgenkitResult};
pub use flow::{FlowDescriptor, FlowInputSchema, FlowOutputSchema};
pub use id::{
    AgentThreadId, ModelRef, ParallelGroupId, ProviderRef, RunId, SessionId, StepId, TraceId,
};
pub use manifest::{ContextGapDiagnostic, ContextManifest, DeclaredResource};
pub use reduce::ReducerDecision;
pub use retrieval::{
    Citation, RetrievalHit, RetrievalQuery, RetrievalSet, RetrieverDescriptor, SourceRef,
};
pub use schema::SchemaRef;
pub use sse::SseFraming;
pub use stream::FlowStreamEvent;
pub use thread::{AgentThreadDescriptor, ThreadCheckpoint, ThreadMessage};
pub use tool::{ToolCall, ToolDescriptor, ToolResult};
pub use tool_name::{ToolNameMap, sanitize_tool_name};
pub use trace::{CostEstimate, TraceEvent, TraceSpan, Usage};
