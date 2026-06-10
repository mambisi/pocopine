//! The curated authoring prelude (RFC-093 §D14).
//!
//! `use pocopine_agenkit::prelude::*;` brings the everyday surface into scope:
//! the runtime entry points, the typed author traits, the flow context, and
//! the core payload types most flows touch. It is host-only — the browser
//! talks to flows through generated `#[server]` helpers, not this runtime.

pub use crate::server::{
    Agenkit, AgenkitBuilder, AgentThreadHandle, Ai, AiAgent, AiAgentBuilder, AiEmbedder,
    AiFlowContext, AiRetriever, AiTool, AiToolContext, BoxFuture, EmbedContext, Flow, MockProvider,
    Provider, RetrievalContext,
};

pub use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, Content, ContentPart, EmbedderDescriptor, FlowStreamEvent,
    Message, ModelRef, ParallelJoin, RetrievalQuery, RetrievalSet, RetrieverDescriptor, Role,
    SchemaRef, StreamMode, ToolDescriptor,
};

/// `schemars::JsonSchema` — derive it on structured-output types so the runtime
/// can constrain and validate generation. Also reachable as
/// `pocopine_agenkit::schemars` for the crate path.
pub use schemars::JsonSchema;
