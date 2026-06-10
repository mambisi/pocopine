//! Stable AI event-name constants (RFC-093 §D12).
//!
//! These are the names the facade stamps onto `pocopine_observe::ObservedEvent`
//! via `emit_tracing`. They are part of the public contract: dashboards and
//! the future debugger key off them, so they must stay stable.

// Flow lifecycle.
/// A public flow began.
pub const AI_FLOW_STARTED: &str = "ai_flow_started";
/// A public flow completed successfully.
pub const AI_FLOW_COMPLETED: &str = "ai_flow_completed";
/// A public flow failed.
pub const AI_FLOW_FAILED: &str = "ai_flow_failed";

// Flow context diagnostics.
/// A framework-mediated resource was used but not declared in the manifest.
pub const AI_CONTEXT_GAP: &str = "ai_context_gap";
/// A flow's context manifest version changed between runs.
pub const AI_CONTEXT_MANIFEST_CHANGED: &str = "ai_context_manifest_changed";

// Step orchestration.
/// A flow step began.
pub const AI_STEP_STARTED: &str = "ai_step_started";
/// A flow step completed.
pub const AI_STEP_COMPLETED: &str = "ai_step_completed";
/// A flow step failed.
pub const AI_STEP_FAILED: &str = "ai_step_failed";

// Parallel and reducer steps.
/// A parallel fan-out group began.
pub const AI_PARALLEL_STARTED: &str = "ai_parallel_started";
/// A parallel fan-out group completed.
pub const AI_PARALLEL_COMPLETED: &str = "ai_parallel_completed";
/// A reducer/fan-in step began.
pub const AI_REDUCER_STARTED: &str = "ai_reducer_started";
/// A reducer/fan-in step completed.
pub const AI_REDUCER_COMPLETED: &str = "ai_reducer_completed";

// Model calls.
/// A model request was sent.
pub const AI_MODEL_REQUEST: &str = "ai_model_request";
/// A model response was received.
pub const AI_MODEL_RESPONSE: &str = "ai_model_response";
/// A model call failed.
pub const AI_MODEL_FAILED: &str = "ai_model_failed";

// Tool calls.
/// A tool invocation began.
pub const AI_TOOL_STARTED: &str = "ai_tool_started";
/// A tool invocation completed.
pub const AI_TOOL_COMPLETED: &str = "ai_tool_completed";
/// A tool invocation failed.
pub const AI_TOOL_FAILED: &str = "ai_tool_failed";

// Retrieval calls.
/// A retrieval call began.
pub const AI_RETRIEVAL_STARTED: &str = "ai_retrieval_started";
/// A retrieval call completed.
pub const AI_RETRIEVAL_COMPLETED: &str = "ai_retrieval_completed";
/// A retrieval call failed.
pub const AI_RETRIEVAL_FAILED: &str = "ai_retrieval_failed";

// Embedding calls.
/// An embedding call began.
pub const AI_EMBEDDING_STARTED: &str = "ai_embedding_started";
/// An embedding call completed.
pub const AI_EMBEDDING_COMPLETED: &str = "ai_embedding_completed";
/// An embedding call failed.
pub const AI_EMBEDDING_FAILED: &str = "ai_embedding_failed";

// Agent thread lifecycle.
/// An agent thread was created.
pub const AI_THREAD_CREATED: &str = "ai_thread_created";
/// An agent thread was resumed.
pub const AI_THREAD_RESUMED: &str = "ai_thread_resumed";
/// An agent thread was checkpointed.
pub const AI_THREAD_CHECKPOINTED: &str = "ai_thread_checkpointed";
/// An agent thread was deleted.
pub const AI_THREAD_DELETED: &str = "ai_thread_deleted";
