//! Per-tool policy specs — the registration metadata the central
//! [`PolicyEvaluator`](agenkitty_core::PolicyEvaluator) resolves tool calls
//! against.
//!
//! Each built-in tool declares its [`ToolClass`] (which
//! `[policy]` config override applies) and its default [`ToolMode`] (what
//! happens when the project sets no override). The modes mirror each tool
//! family's README contract: read/introspection tools default `Allow`,
//! workspace mutations and command execution default `Ask`, and
//! subsystem-gated families (secrets, MCP) are classed `Other` so no class
//! override can loosen their own gate.

use agenkitty_core::policy::{CapabilitySet, FilesystemCapability, ToolMode};
use agenkitty_core::tools::{ToolClass, ToolSpec};

use super::fs::{
    FS_APPEND_TOOL_ID, FS_COPY_TOOL_ID, FS_EXISTS_TOOL_ID, FS_LIST_TOOL_ID, FS_MKDIR_TOOL_ID,
    FS_MOVE_TOOL_ID, FS_READ_TOOL_ID, FS_REMOVE_TOOL_ID, FS_SEARCH_TOOL_ID, FS_STAT_TOOL_ID,
    FS_WRITE_TOOL_ID,
};
use super::{
    ARTIFACT_DELETE_TOOL_ID, ARTIFACT_LINK_TOOL_ID, ARTIFACT_LIST_TOOL_ID, ARTIFACT_READ_TOOL_ID,
    ARTIFACT_WRITE_TOOL_ID, MEMORY_FORGET_TOOL_ID, MEMORY_READ_TOOL_ID, MEMORY_SEARCH_TOOL_ID,
    MEMORY_UPDATE_TOOL_ID, MEMORY_WRITE_TOOL_ID, NET_FETCH_TOOL_ID, PATCH_APPLY_TOOL_ID,
    PATCH_PREVIEW_TOOL_ID, SECRET_LIST_TOOL_ID, SECRET_REQUEST_TOOL_ID, SECRET_REVOKE_TOOL_ID,
    SECRET_USE_TOOL_ID, SESSION_CHECKPOINT_TOOL_ID, SESSION_EVENTS_TOOL_ID, SESSION_INFO_TOOL_ID,
    SESSION_NOTE_TOOL_ID, SESSION_SUMMARY_TOOL_ID,
};

fn fs_read(id: &str, description: &str) -> ToolSpec {
    ToolSpec::built_in(id, description)
        .with_class(ToolClass::Read)
        .with_mode(ToolMode::Allow)
        .with_capabilities(CapabilitySet {
            filesystem: FilesystemCapability::Read,
            ..CapabilitySet::default()
        })
}

fn fs_write(id: &str, description: &str) -> ToolSpec {
    ToolSpec::built_in(id, description)
        .with_class(ToolClass::Write)
        .with_mode(ToolMode::Ask)
        .with_capabilities(CapabilitySet {
            filesystem: FilesystemCapability::Write,
            ..CapabilitySet::default()
        })
        .side_effecting()
}

fn read(id: &str, description: &str) -> ToolSpec {
    ToolSpec::built_in(id, description)
        .with_class(ToolClass::Read)
        .with_mode(ToolMode::Allow)
}

fn write_allow(id: &str, description: &str) -> ToolSpec {
    ToolSpec::built_in(id, description)
        .with_class(ToolClass::Write)
        .with_mode(ToolMode::Allow)
        .side_effecting()
}

fn other(id: &str, description: &str, mode: ToolMode) -> ToolSpec {
    ToolSpec::built_in(id, description).with_mode(mode)
}

/// The policy specs for every built-in tool family — including the opt-in
/// ones (`net.fetch`, `mcp.*` verbs) that are absent from the default
/// registration sets: a project that opts in still gets a policy class for
/// them. Imported `mcp.<server>.<tool>` adapters are dynamic and deliberately
/// absent — the evaluator recognizes the `mcp.` prefix and defers to MCP
/// capability admission.
pub fn builtin_tool_specs() -> Vec<ToolSpec> {
    let mut specs = vec![
        // fs — bounded, path-validated workspace access.
        fs_read(FS_SEARCH_TOOL_ID, "Search workspace files"),
        fs_read(FS_LIST_TOOL_ID, "List workspace directories"),
        fs_read(FS_READ_TOOL_ID, "Read a workspace file"),
        fs_read(FS_STAT_TOOL_ID, "Stat a workspace path"),
        fs_read(FS_EXISTS_TOOL_ID, "Check a workspace path exists"),
        fs_write(FS_WRITE_TOOL_ID, "Write a workspace file"),
        fs_write(FS_APPEND_TOOL_ID, "Append to a workspace file"),
        fs_write(FS_MKDIR_TOOL_ID, "Create a workspace directory"),
        fs_write(FS_COPY_TOOL_ID, "Copy a workspace file"),
        fs_write(FS_MOVE_TOOL_ID, "Move a workspace path"),
        fs_write(FS_REMOVE_TOOL_ID, "Remove a workspace path"),
        // patch — structured multi-file edits.
        read(PATCH_PREVIEW_TOOL_ID, "Preview a structured patch"),
        fs_write(PATCH_APPLY_TOOL_ID, "Apply a structured patch"),
        // session — bounded, redacted session metadata.
        read(SESSION_INFO_TOOL_ID, "Describe the current session"),
        read(SESSION_EVENTS_TOOL_ID, "List session events"),
        write_allow(SESSION_NOTE_TOOL_ID, "Record a session note"),
        write_allow(SESSION_SUMMARY_TOOL_ID, "Record a session summary"),
        write_allow(SESSION_CHECKPOINT_TOOL_ID, "Record a session checkpoint"),
        // memory — the agent's namespaced durable notebook (opt-in family).
        read(MEMORY_SEARCH_TOOL_ID, "Search durable memory"),
        read(MEMORY_READ_TOOL_ID, "Read a memory entry"),
        write_allow(MEMORY_WRITE_TOOL_ID, "Write a memory entry"),
        write_allow(MEMORY_UPDATE_TOOL_ID, "Update a memory entry"),
        write_allow(MEMORY_FORGET_TOOL_ID, "Tombstone a memory entry"),
        // artifacts — durable run outputs. Session-scope writes default Allow;
        // the *project*-scope Ask lives inside the tool (the runtime consults
        // the host approver per scope — an id-keyed spec can't split on args).
        // Deletion is destructive regardless of scope → dispatch-gated Ask.
        read(ARTIFACT_READ_TOOL_ID, "Read an artifact window"),
        read(ARTIFACT_LIST_TOOL_ID, "List stored artifacts"),
        write_allow(ARTIFACT_WRITE_TOOL_ID, "Store a run-output artifact"),
        write_allow(
            ARTIFACT_LINK_TOOL_ID,
            "Link a workspace file as an artifact",
        ),
        ToolSpec::built_in(ARTIFACT_DELETE_TOOL_ID, "Delete an artifact")
            .with_class(ToolClass::Write)
            .with_mode(ToolMode::Ask)
            .side_effecting(),
        // secrets — subsystem-gated (`Other`): the secret runtime's own
        // mode + exact-tuple preauthorization + approver routing is the
        // authoritative gate, so the outer specs are Allow — an outer Ask
        // would block a headless host's preauthorized grants at dispatch
        // (before the tuple check runs) and double-prompt interactive runs.
        // Class overrides never touch these either way.
        other(SECRET_LIST_TOOL_ID, "List secret metadata", ToolMode::Allow),
        other(
            SECRET_REQUEST_TOOL_ID,
            "Request a secret grant",
            ToolMode::Allow,
        ),
        other(SECRET_USE_TOOL_ID, "Use a secret grant", ToolMode::Allow),
        other(
            SECRET_REVOKE_TOOL_ID,
            "Revoke a secret grant",
            ToolMode::Allow,
        ),
        // network — opt-in; NetPolicy + the SSRF guard stay the inner gate.
        ToolSpec::built_in(NET_FETCH_TOOL_ID, "Fetch an allowlisted URL")
            .with_class(ToolClass::Network)
            .with_mode(ToolMode::Ask)
            .with_capabilities(CapabilitySet {
                network: true,
                ..CapabilitySet::default()
            }),
    ];
    specs.extend(process_tool_specs());
    specs.extend(mcp_tool_specs());
    specs
}

#[cfg(unix)]
fn process_tool_specs() -> Vec<ToolSpec> {
    use super::process::{
        PROCESS_KILL_TOOL_ID, PROCESS_READ_TOOL_ID, PROCESS_RUN_TOOL_ID, PROCESS_SPAWN_TOOL_ID,
        PROCESS_WRITE_TOOL_ID,
    };
    fn command(id: &str, description: &str, mode: ToolMode) -> ToolSpec {
        ToolSpec::built_in(id, description)
            .with_class(ToolClass::Command)
            .with_mode(mode)
            .side_effecting()
    }
    vec![
        command(
            PROCESS_RUN_TOOL_ID,
            "Run a sandboxed command",
            ToolMode::Ask,
        ),
        command(
            PROCESS_SPAWN_TOOL_ID,
            "Spawn a long-running sandboxed process",
            ToolMode::Ask,
        ),
        // Reading a handle's captured output mutates nothing.
        ToolSpec::built_in(PROCESS_READ_TOOL_ID, "Read a process handle's output")
            .with_class(ToolClass::Command)
            .with_mode(ToolMode::Allow),
        command(
            PROCESS_WRITE_TOOL_ID,
            "Write to a process handle's stdin",
            ToolMode::Ask,
        ),
        // Killing the session's own sandboxed child is cleanup, not damage.
        command(
            PROCESS_KILL_TOOL_ID,
            "Kill an owned process handle",
            ToolMode::Allow,
        ),
    ]
}

#[cfg(not(unix))]
fn process_tool_specs() -> Vec<ToolSpec> {
    Vec::new()
}

#[cfg(unix)]
fn mcp_tool_specs() -> Vec<ToolSpec> {
    use super::mcp::{
        MCP_CALL_TOOL_ID, MCP_GET_PROMPT_TOOL_ID, MCP_READ_RESOURCE_TOOL_ID, MCP_SERVERS_TOOL_ID,
        MCP_TOOLS_TOOL_ID,
    };
    // All `Other`/`Allow`: MCP capability-scoped admission (TOFU pins,
    // per-server capability grants) is the authoritative gate inside the tool;
    // the outer evaluator neither duplicates nor overrides it.
    vec![
        other(
            MCP_SERVERS_TOOL_ID,
            "List configured MCP servers",
            ToolMode::Allow,
        ),
        other(
            MCP_TOOLS_TOOL_ID,
            "List admitted MCP tools",
            ToolMode::Allow,
        ),
        other(
            MCP_CALL_TOOL_ID,
            "Call an admitted MCP tool",
            ToolMode::Allow,
        ),
        other(
            MCP_READ_RESOURCE_TOOL_ID,
            "Read an MCP resource",
            ToolMode::Allow,
        ),
        other(MCP_GET_PROMPT_TOOL_ID, "Get an MCP prompt", ToolMode::Allow),
    ]
}

#[cfg(not(unix))]
fn mcp_tool_specs() -> Vec<ToolSpec> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use agenkitty_core::PolicyEvaluator;
    use agenkitty_core::config::PolicyConfigSection;

    use super::super::{
        default_read_only_tool_ids, is_known_tool_id, known_memory_tool_ids, known_patch_tool_ids,
        known_secret_tool_ids, known_session_tool_ids,
    };
    use super::*;

    fn all_known_tool_ids() -> Vec<String> {
        let mut ids: Vec<String> = vec![
            FS_SEARCH_TOOL_ID,
            FS_LIST_TOOL_ID,
            FS_READ_TOOL_ID,
            FS_STAT_TOOL_ID,
            FS_EXISTS_TOOL_ID,
            FS_WRITE_TOOL_ID,
            FS_APPEND_TOOL_ID,
            FS_MKDIR_TOOL_ID,
            FS_COPY_TOOL_ID,
            FS_MOVE_TOOL_ID,
            FS_REMOVE_TOOL_ID,
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        ids.extend(known_session_tool_ids().iter().map(|id| id.to_string()));
        ids.extend(known_memory_tool_ids().iter().map(|id| id.to_string()));
        ids.extend(
            super::super::known_artifact_tool_ids()
                .iter()
                .map(|id| id.to_string()),
        );
        ids.extend(known_patch_tool_ids().iter().map(|id| id.to_string()));
        ids.extend(known_secret_tool_ids().iter().map(|id| id.to_string()));
        #[cfg(unix)]
        ids.extend(
            super::super::known_process_tool_ids()
                .iter()
                .map(|id| id.to_string()),
        );
        ids
    }

    #[test]
    fn every_known_tool_id_has_exactly_one_spec() {
        let specs = builtin_tool_specs();
        let mut seen = HashSet::new();
        for spec in &specs {
            assert!(
                seen.insert(spec.descriptor.id.clone()),
                "duplicate spec for `{}`",
                spec.descriptor.id
            );
        }
        for id in all_known_tool_ids() {
            assert!(seen.contains(&id), "known tool `{id}` has no policy spec");
        }
    }

    #[test]
    fn subsystem_gated_tools_pass_the_outer_gate_untouched() {
        // Secrets (and MCP verbs) are class `Other` AND outer-mode Allow: the
        // subsystem's own gate — the secret runtime's mode + exact-tuple
        // preauthorization, MCP capability admission — is the sole authority.
        // An outer Ask would block a headless host's preauthorized grants at
        // dispatch and double-prompt interactive runs.
        let evaluator = PolicyEvaluator::new(
            // Even with every class loosened/tightened, `Other` is untouched.
            PolicyConfigSection {
                read_mode: Some(ToolMode::Deny),
                write_mode: Some(ToolMode::Deny),
                command_mode: Some(ToolMode::Deny),
                network_mode: Some(ToolMode::Deny),
            },
            builtin_tool_specs(),
        );
        for id in [
            SECRET_LIST_TOOL_ID,
            SECRET_REQUEST_TOOL_ID,
            SECRET_USE_TOOL_ID,
            SECRET_REVOKE_TOOL_ID,
        ] {
            let spec = evaluator.spec(id).expect("spec exists");
            assert_eq!(spec.class, ToolClass::Other, "`{id}` must be Other-class");
            assert_eq!(
                evaluator.effective_mode(spec),
                ToolMode::Allow,
                "`{id}` must pass the outer gate; its runtime is the authority"
            );
        }
    }

    #[test]
    fn extra_spec_ids_are_the_documented_opt_ins() {
        // Specs may cover more than `is_known_tool_id` (net.fetch + the mcp
        // verbs are opt-in registrations) — but nothing else.
        for spec in builtin_tool_specs() {
            let id = &spec.descriptor.id;
            assert!(
                is_known_tool_id(id) || id == NET_FETCH_TOOL_ID || id.starts_with("mcp."),
                "spec `{id}` is neither a known tool id nor a documented opt-in"
            );
        }
    }

    #[test]
    fn default_read_only_set_is_frictionless_under_default_config() {
        // The default tool set must run without prompts when the project sets
        // no policy overrides — otherwise the out-of-box experience is a wall
        // of Asks for read-only introspection.
        let evaluator = PolicyEvaluator::new(PolicyConfigSection::default(), builtin_tool_specs());
        for id in default_read_only_tool_ids() {
            let spec = evaluator.spec(&id).expect("default tool has a spec");
            assert_eq!(
                evaluator.effective_mode(spec),
                ToolMode::Allow,
                "default tool `{id}` must be Allow under default config"
            );
        }
    }

    #[test]
    fn workspace_mutations_default_to_ask() {
        let evaluator = PolicyEvaluator::new(PolicyConfigSection::default(), builtin_tool_specs());
        for id in [
            FS_WRITE_TOOL_ID,
            FS_REMOVE_TOOL_ID,
            PATCH_APPLY_TOOL_ID,
            NET_FETCH_TOOL_ID,
        ] {
            let spec = evaluator.spec(id).expect("spec exists");
            assert_eq!(
                evaluator.effective_mode(spec),
                ToolMode::Ask,
                "`{id}` must default to Ask"
            );
        }
    }
}
