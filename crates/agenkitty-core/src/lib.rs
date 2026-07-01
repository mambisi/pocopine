//! Wasm-safe Agenkitty framework contracts.
//!
//! This crate contains portable data types only: agent specs, framework events,
//! policy/capability decisions, sandbox descriptions, and tool-use planning
//! contracts. Host execution, filesystem access, provider credentials, and
//! process control stay in the `agenkitty` binary crate.

pub mod config;
pub mod events;
pub mod policy;
pub mod sandbox_api;
pub mod sessions;
pub mod supervisor;
pub mod tools;

pub use events::{FrameworkEvent, FrameworkEventKind, RunStatus};
pub use policy::{CapabilitySet, PolicyDecision, ToolMode};
pub use sessions::{
    SessionArtifactLink, SessionCheckpoint, SessionCheckpointKind, SessionClosure, SessionEvent,
    SessionEventCursor, SessionEventKind, SessionEventPolicy, SessionEventRange, SessionExport,
    SessionIdentity, SessionNote, SessionSourceRef, SessionStoreKind, SessionSummary,
};
pub use supervisor::{AgentSpec, BudgetSpec, SandboxSpec, WorkspaceSpec};
pub use tools::{
    ToolKind, ToolLifecycle, ToolSelection, ToolSpec, ToolUseOutcome, ToolUseRequest, ToolUseStatus,
};
