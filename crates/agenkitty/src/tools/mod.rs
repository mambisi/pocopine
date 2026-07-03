//! Host adapters for Agenkitty tool contracts.

pub mod artifacts;
pub mod fs;
#[cfg(unix)]
pub mod mcp;
pub mod memory;
pub mod network;
pub mod patch;
#[cfg(unix)]
pub mod process;
pub mod secrets;
pub mod session;
pub mod specs;

use std::path::Path;
use std::sync::Arc;

use pocopine_agenkit::server::AgenkitBuilder;
use pocopine_agenkit_core::AgenkitResult;

pub use agenkitty_core::tools::*;
pub use artifacts::{
    ARTIFACT_DELETE_TOOL_ID, ARTIFACT_LINK_TOOL_ID, ARTIFACT_LIST_TOOL_ID, ARTIFACT_READ_TOOL_ID,
    ARTIFACT_WRITE_TOOL_ID, ArtifactRuntime, ArtifactScope, CurrentArtifactContext,
    InMemoryArtifactStore, LocalArtifactStore, known_artifact_tool_ids, register_artifact_tools,
    resolve_artifact_tool_ids,
};
pub use fs::{
    FS_APPEND_TOOL_ID, FS_COPY_TOOL_ID, FS_EXISTS_TOOL_ID, FS_LIST_TOOL_ID, FS_MKDIR_TOOL_ID,
    FS_MOVE_TOOL_ID, FS_READ_TOOL_ID, FS_REMOVE_TOOL_ID, FS_SEARCH_TOOL_ID, FS_STAT_TOOL_ID,
    FS_WRITE_TOOL_ID, FsAppendInput, FsAppendTool, FsCopyInput, FsCopyTool, FsExistsEntry,
    FsExistsInput, FsExistsOutput, FsExistsTool, FsListEntry, FsListInput, FsListOutput,
    FsListTool, FsMkdirInput, FsMkdirTool, FsMoveInput, FsMoveTool, FsMutationOperation,
    FsMutationOutput, FsPathKind, FsReadInput, FsReadLine, FsReadOutput, FsReadTool, FsRemoveInput,
    FsRemoveTool, FsSearchEngine, FsSearchHit, FsSearchInput, FsSearchOutput, FsSearchTool,
    FsStatInput, FsStatOutput, FsStatTool, FsWriteInput, FsWriteTool,
    default_read_only_tool_ids as default_fs_read_only_tool_ids, register_fs_tools,
    resolve_tool_ids as resolve_fs_tool_ids,
};
#[cfg(unix)]
pub use mcp::{
    CapabilitySet, MCP_CALL_TOOL_ID, MCP_GET_PROMPT_TOOL_ID, MCP_NAMESPACE,
    MCP_READ_RESOURCE_TOOL_ID, MCP_SERVERS_TOOL_ID, MCP_TOOLS_TOOL_ID, McpRuntime, McpScope,
    McpServerConfig, McpTransportConfig, NetworkCapability, ProcessCapability,
    default_mcp_tool_ids, known_mcp_tool_ids, namespaced_tool_id, register_mcp_tools,
};
pub use memory::{
    CurrentMemoryContext, InMemoryMemoryStore, LocalJsonlMemoryStore, MEMORY_FORGET_TOOL_ID,
    MEMORY_READ_TOOL_ID, MEMORY_SEARCH_TOOL_ID, MEMORY_UPDATE_TOOL_ID, MEMORY_WRITE_TOOL_ID,
    MemoryRuntime, MemoryScope, known_memory_tool_ids, register_memory_tools,
    resolve_memory_tool_ids,
};
pub use network::{
    NET_FETCH_TOOL_ID, NetFetchInput, NetFetchOutput, NetFetchTool, NetPolicy,
    known_network_tool_ids, register_network_tools,
};
pub use patch::{
    PATCH_APPLY_TOOL_ID, PATCH_PREVIEW_TOOL_ID, PatchApplyTool, PatchFileSummary, PatchInput,
    PatchOperation, PatchOutput, PatchPreviewTool, known_patch_tool_ids, register_patch_tools,
};
#[cfg(unix)]
pub use process::{
    CapturedStream, PROCESS_KILL_TOOL_ID, PROCESS_READ_TOOL_ID, PROCESS_RUN_TOOL_ID,
    PROCESS_SPAWN_TOOL_ID, PROCESS_WRITE_TOOL_ID, ProcessKillInput, ProcessKillOutput,
    ProcessKillTool, ProcessReadInput, ProcessReadOutput, ProcessReadTool, ProcessRunInput,
    ProcessRunOutput, ProcessRunTool, ProcessSpawnInput, ProcessSpawnOutput, ProcessSpawnTool,
    ProcessTable, ProcessToolConfig, ProcessWriteInput, ProcessWriteOutput, ProcessWriteTool,
    ResourceLimits, SandboxBackend, SandboxPolicy, default_process_tool_ids,
    known_process_tool_ids, register_process_tools, register_process_tools_with,
};
pub use secrets::{
    AgentSecretResolver, EnvSecretResolver, InMemorySecretResolver, SECRET_LIST_TOOL_ID,
    SECRET_REQUEST_TOOL_ID, SECRET_REVOKE_TOOL_ID, SECRET_USE_TOOL_ID, SecretGrant, SecretGrantKey,
    SecretListInput, SecretListOutput, SecretListTool, SecretMetadata, SecretRequestInput,
    SecretRequestOutput, SecretRequestPolicy, SecretRequestTool, SecretRevokeInput,
    SecretRevokeOutput, SecretRevokeTool, SecretRuntime, SecretScope, SecretUseInput,
    SecretUseOutput, SecretUseTool, known_secret_tool_ids, register_secret_tools,
};
pub use specs::builtin_tool_specs;

pub use session::{
    CurrentSessionContext, InMemorySessionMetadataStore, LocalJsonlSessionMetadataStore,
    SESSION_CHECKPOINT_TOOL_ID, SESSION_EVENTS_TOOL_ID, SESSION_INFO_TOOL_ID, SESSION_NOTE_TOOL_ID,
    SESSION_SUMMARY_TOOL_ID, SessionCheckpointInput, SessionCheckpointKindSpec,
    SessionCheckpointOutput, SessionCheckpointTool, SessionEventFilter, SessionEventKindFilter,
    SessionEventRangeSpec, SessionEventView, SessionEventsInput, SessionEventsOutput,
    SessionEventsTool, SessionInfoInput, SessionInfoOutput, SessionInfoTool,
    SessionMetadataCloseResult, SessionMetadataStore, SessionNoteInput, SessionNoteOutput,
    SessionNoteTool, SessionRuntime, SessionSourceRefSpec, SessionSummaryInput,
    SessionSummaryOutput, SessionSummaryTool, current_time_ms, default_session_tool_ids,
    known_session_tool_ids, register_session_tools, resolve_session_tool_ids,
    session_event_from_framework,
};

pub fn register_tools(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
) -> AgenkitResult<AgenkitBuilder> {
    register_tools_with_session_runtime(builder, root, Arc::new(SessionRuntime::in_memory()))
}

/// Register the repo tools with a caller-owned session runtime and an internal
/// in-memory memory runtime. Retained for callers that do not drive memory.
pub fn register_tools_with_session_runtime(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
    session_runtime: Arc<SessionRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    register_tools_with_runtimes(
        builder,
        root,
        session_runtime,
        Arc::new(MemoryRuntime::in_memory()),
    )
}

/// Register the repo tools with caller-owned session and memory runtimes. The
/// runner passes the same runtimes it injects per tool call so the
/// `context_token` handshake resolves.
pub fn register_tools_with_secret_runtime(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
    secret_runtime: Arc<SecretRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    register_tools_with_all_runtimes(
        builder,
        root,
        Arc::new(SessionRuntime::in_memory()),
        Arc::new(MemoryRuntime::in_memory()),
        secret_runtime,
    )
}

pub fn register_tools_with_runtimes(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
    session_runtime: Arc<SessionRuntime>,
    memory_runtime: Arc<MemoryRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    register_tools_with_all_runtimes(
        builder,
        root,
        session_runtime,
        memory_runtime,
        Arc::new(SecretRuntime::empty()),
    )
}

/// Register the repo tools with caller-owned session, memory, and secret
/// runtimes (the artifact runtime defaults to in-memory with the project root
/// as its link workspace; pass a caller-owned one via
/// [`register_tools_with_all_runtimes_and_artifacts`]).
pub fn register_tools_with_all_runtimes(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
    session_runtime: Arc<SessionRuntime>,
    memory_runtime: Arc<MemoryRuntime>,
    secret_runtime: Arc<SecretRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    let artifact_runtime = Arc::new(ArtifactRuntime::new(Arc::new(
        InMemoryArtifactStore::new().with_workspace_root(root.as_ref()),
    )));
    register_tools_with_all_runtimes_and_artifacts(
        builder,
        root,
        session_runtime,
        memory_runtime,
        secret_runtime,
        artifact_runtime,
    )
}

/// Register the repo tools with caller-owned session, memory, secret, and
/// artifact runtimes.
pub fn register_tools_with_all_runtimes_and_artifacts(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
    session_runtime: Arc<SessionRuntime>,
    memory_runtime: Arc<MemoryRuntime>,
    secret_runtime: Arc<SecretRuntime>,
    artifact_runtime: Arc<ArtifactRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    let root = root.as_ref();
    let builder = register_secret_tools(builder, secret_runtime.clone())?;
    let builder = register_fs_tools(builder, root)?;
    let builder = register_patch_tools(builder, root)?;
    let builder = register_session_tools(builder, session_runtime);
    let builder = register_memory_tools(builder, memory_runtime);
    let builder = register_artifact_tools(builder, artifact_runtime);
    #[cfg(unix)]
    let builder = register_process_tools_with(
        builder,
        root,
        &ProcessToolConfig {
            secret_runtime: Some(secret_runtime),
            ..ProcessToolConfig::default()
        },
    )?;
    Ok(builder)
}

pub fn default_read_only_tool_ids() -> Vec<String> {
    let mut ids = default_fs_read_only_tool_ids();
    ids.extend(default_session_tool_ids());
    ids
}

pub fn resolve_tool_ids(raw: &[String]) -> Result<Vec<String>, String> {
    if raw.is_empty() {
        return Ok(default_read_only_tool_ids());
    }

    let mut ids = Vec::new();
    for value in raw {
        for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if id.eq_ignore_ascii_case("none") {
                return Ok(Vec::new());
            }
            if !is_known_tool_id(id) {
                return Err(format!("unknown tool `{id}`"));
            }
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

fn is_known_tool_id(id: &str) -> bool {
    // Network tools (`net.fetch`) are deliberately absent: they are opt-in via
    // `register_network_tools(builder, NetPolicy)` and not part of the default
    // `register_tools` runtime. Accepting the id here would let `--tools
    // net.fetch` resolve to a tool no descriptor is exposed for. They join this
    // set once the project-config policy plumbing lands (Phase 7).
    //
    // The `mcp.*` verbs + imported `mcp.<server>.<tool>` adapters are likewise
    // opt-in via `register_mcp_tools(builder, runtime)` and intentionally absent
    // here: their descriptors do not exist until the verbs land (Phase 2+), and
    // the imported ids are dynamic per-server.
    resolve_fs_tool_ids(&[id.to_string()]).is_ok()
        || known_session_tool_ids().contains(&id)
        || known_memory_tool_ids().contains(&id)
        || known_artifact_tool_ids().contains(&id)
        || known_patch_tool_ids().contains(&id)
        || known_secret_tool_ids().contains(&id)
        || is_known_process_tool_id(id)
}

#[cfg(unix)]
fn is_known_process_tool_id(id: &str) -> bool {
    known_process_tool_ids().contains(&id)
}

#[cfg(not(unix))]
fn is_known_process_tool_id(_id: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tools_include_read_only_session_tools() {
        let ids = default_read_only_tool_ids();
        assert!(ids.contains(&SESSION_INFO_TOOL_ID.to_string()));
        assert!(ids.contains(&SESSION_EVENTS_TOOL_ID.to_string()));
        assert!(!ids.contains(&SESSION_NOTE_TOOL_ID.to_string()));
        assert!(!ids.contains(&SESSION_SUMMARY_TOOL_ID.to_string()));
        assert!(!ids.contains(&SESSION_CHECKPOINT_TOOL_ID.to_string()));
        assert!(!ids.contains(&SECRET_LIST_TOOL_ID.to_string()));
    }

    #[test]
    fn resolver_accepts_secret_tools_when_requested() {
        assert_eq!(
            resolve_tool_ids(&[SECRET_LIST_TOOL_ID.to_string()]).unwrap(),
            vec![SECRET_LIST_TOOL_ID.to_string()]
        );
    }

    #[test]
    fn memory_tools_are_known_but_opt_in() {
        // Resolvable by explicit opt-in via `--tools`.
        assert_eq!(
            resolve_tool_ids(&["memory.search,memory.write".to_string()]).unwrap(),
            vec![
                MEMORY_SEARCH_TOOL_ID.to_string(),
                MEMORY_WRITE_TOOL_ID.to_string()
            ]
        );
        // But never present in the default read-only set.
        let defaults = default_read_only_tool_ids();
        for id in known_memory_tool_ids() {
            assert!(!defaults.contains(&id.to_string()));
        }
    }
}
