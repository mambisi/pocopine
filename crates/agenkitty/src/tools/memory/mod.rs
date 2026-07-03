//! Durable, inspectable agent memory.
//!
//! Memory is context the harness can retrieve, write, revise, and forget across
//! turns and worktrees — not policy enforcement, and not a second transcript
//! store. The README captures the contract and deferred adapters.
//!
//! Layering: `common` defines the types, the [`MemoryStore`] trait, the
//! bounded-text/secret helpers, and the [`MemoryRuntime`] context plumbing;
//! `store` holds the default in-memory backend; `search`/`read`/`write`/
//! `update`/`forget` are the model-facing [`AiTool`](pocopine_agenkit::server::AiTool)s,
//! wired through `registry`. `local` provides the durable JSONL backend and
//! `retriever` provides deterministic flow-side retrieval.

mod common;
mod forget;
mod local;
mod read;
mod registry;
mod retriever;
mod search;
mod store;
mod update;
mod write;

pub use common::{
    CurrentMemoryContext, DEFAULT_SEARCH_LIMIT, MAX_BODY_BYTES, MAX_SEARCH_LIMIT,
    MAX_SNIPPET_BYTES, MAX_TAG_BYTES, MAX_TAGS, MAX_TITLE_BYTES, MemoryCompactionReport,
    MemoryCompactionRequest, MemoryEntry, MemoryEntryView, MemoryFuture, MemoryKind, MemoryPatch,
    MemoryRelation, MemoryRelationKind, MemoryRetention, MemoryRuntime, MemoryScope,
    MemorySearchFilter, MemorySearchHit, MemorySource, MemoryStore, MemoryStoreKind,
    MemoryTombstone, bound_text, current_time_ms, normalize_tags,
};
pub use forget::{MEMORY_FORGET_TOOL_ID, MemoryForgetInput, MemoryForgetOutput, MemoryForgetTool};
pub use local::LocalJsonlMemoryStore;
pub use read::{MEMORY_READ_TOOL_ID, MemoryReadInput, MemoryReadTool};
pub use registry::{known_memory_tool_ids, register_memory_tools, resolve_memory_tool_ids};
pub use retriever::{MEMORY_RETRIEVER_ID, MemoryRetrieveQuery, MemoryRetriever};
pub use search::{MEMORY_SEARCH_TOOL_ID, MemorySearchInput, MemorySearchOutput, MemorySearchTool};
pub use store::{InMemoryMemoryStore, namespace_for_write};
pub use update::{MEMORY_UPDATE_TOOL_ID, MemoryUpdateInput, MemoryUpdateTool};
pub use write::{MEMORY_WRITE_TOOL_ID, MemoryWriteInput, MemoryWriteOutput, MemoryWriteTool};
