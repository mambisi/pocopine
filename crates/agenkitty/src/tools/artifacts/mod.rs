//! Durable run-output artifacts.
//!
//! Artifacts store files produced during agent work — reports, logs, build
//! products, command outputs — separated from workspace edits (`fs.*`/
//! `patch.*`) and from semantic memory (`memory.*`). Each artifact has a
//! stable citable id, name/media-type/size/hash metadata, provenance
//! source-refs, and a session or project scope. The README captures the
//! contract.
//!
//! Layering mirrors the memory family: `common` defines the types, the
//! [`ArtifactStore`] trait, validation helpers, and the [`ArtifactRuntime`]
//! context plumbing; `store` holds the in-memory backend + shared window/link
//! helpers; `local` is the durable backend (JSONL metadata log +
//! content-addressed blobs); `write`/`read`/`list`/`link`/`delete` are the
//! model-facing [`AiTool`](pocopine_agenkit::server::AiTool)s, wired through
//! `registry`.

mod common;
mod delete;
mod link;
mod list;
mod local;
mod read;
mod registry;
mod store;
mod write;

pub use common::{
    ArtifactContentWindow, ArtifactDraft, ArtifactEncoding, ArtifactFuture, ArtifactMetadata,
    ArtifactRuntime, ArtifactScope, ArtifactStore, ArtifactStoreKind, CurrentArtifactContext,
    MAX_CONTENT_BYTES, MAX_LIST_LIMIT, MAX_NAME_BYTES, MAX_READ_WINDOW_BYTES, content_hash,
    current_time_ms, validate_artifact_name, validate_media_type,
};
pub use delete::{ARTIFACT_DELETE_TOOL_ID, ArtifactDeleteInput, ArtifactDeleteTool};
pub use link::{ARTIFACT_LINK_TOOL_ID, ArtifactLinkInput, ArtifactLinkTool};
pub use list::{ARTIFACT_LIST_TOOL_ID, ArtifactListInput, ArtifactListOutput, ArtifactListTool};
pub use local::LocalArtifactStore;
pub use read::{ARTIFACT_READ_TOOL_ID, ArtifactReadInput, ArtifactReadOutput, ArtifactReadTool};
pub use registry::{known_artifact_tool_ids, register_artifact_tools, resolve_artifact_tool_ids};
pub use store::InMemoryArtifactStore;
pub use write::{
    ARTIFACT_WRITE_TOOL_ID, ArtifactWriteInput, ArtifactWriteOutput, ArtifactWriteTool,
};
