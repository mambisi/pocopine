use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pocopine_agenkit::server::AgenkitBuilder;
use pocopine_agenkit_core::AgenkitResult;

use super::common::canonical_root;
use super::handle::{
    PROCESS_KILL_TOOL_ID, PROCESS_READ_TOOL_ID, PROCESS_SPAWN_TOOL_ID, PROCESS_WRITE_TOOL_ID,
    ProcessKillTool, ProcessReadTool, ProcessSpawnTool, ProcessTable, ProcessWriteTool,
};
use super::run::{PROCESS_RUN_TOOL_ID, ProcessRunTool};
use super::sandbox::SandboxPolicy;
use crate::tools::secrets::SecretRuntime;

/// Declarative configuration for the process tool family — the mapping target
/// for a project config block. Defaults match the secure baseline:
/// argv-only (no shell), workspace filesystem confinement, network off.
#[derive(Clone, Debug, Default)]
pub struct ProcessToolConfig {
    /// Permit `shell: true` (`/bin/sh -c`).
    pub allow_shell: bool,
    /// Restrict argv to these prefixes (`None` = no allowlist).
    pub allowed_prefixes: Option<Vec<Vec<String>>>,
    /// Allow `AF_INET`/`AF_INET6` sockets in the sandbox.
    pub allow_network: bool,
    /// Extra writable roots beyond the workspace root + `/tmp`.
    pub extra_writable_roots: Vec<PathBuf>,
    /// Enforce via bubblewrap namespaces instead of in-process Landlock+seccomp.
    pub use_bubblewrap: bool,
    /// Tier-1: route children's outbound HTTP(S) through a host egress proxy
    /// (e.g. the network tool's `EgressProxy`) by injecting `HTTP_PROXY`/…. Routes
    /// well-behaved clients; the child must be allowed to reach the proxy
    /// (`allow_network`). Does not prevent a direct-socket bypass.
    pub egress_proxy: Option<SocketAddr>,
    /// Tier-2: lock children into a bubblewrap network namespace whose only
    /// egress is the host proxy at this Unix socket (auto-enables the bubblewrap
    /// backend). Prevents a direct-socket bypass, not just well-behaved clients.
    pub egress_proxy_uds: Option<PathBuf>,
    /// Binary providing the `__egress-shim` entrypoint for Tier-2. `None` uses the
    /// running executable (correct when it is the agenkitty CLI); set this when
    /// embedding agenkitty as a library in another binary.
    pub egress_shim_path: Option<PathBuf>,
    /// Optional secret-handle runtime shared with `secret.*` tools. When set,
    /// `process.run`/`process.spawn` may accept approved handles through their
    /// `secret_env` maps and resolve values just before spawning the child.
    pub secret_runtime: Option<Arc<SecretRuntime>>,
}

/// Register the process tool family against a workspace root with the default
/// (secure) configuration. The spawn/read/write/kill tools share one
/// session-scoped [`ProcessTable`]; dropping the built runtime SIGKILLs every
/// still-running process group.
pub fn register_process_tools(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
) -> AgenkitResult<AgenkitBuilder> {
    register_process_tools_with(builder, root, &ProcessToolConfig::default())
}

/// Register the process tool family with an explicit [`ProcessToolConfig`].
pub fn register_process_tools_with(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
    config: &ProcessToolConfig,
) -> AgenkitResult<AgenkitBuilder> {
    let root = canonical_root(root.as_ref())?;

    let mut sandbox = SandboxPolicy::workspace(&root);
    sandbox
        .writable_roots
        .extend(config.extra_writable_roots.iter().cloned());
    sandbox.allow_network = config.allow_network;
    if config.use_bubblewrap {
        sandbox = sandbox.using_bubblewrap();
    }
    // Tier-2 egress lockdown requires the bubblewrap network namespace, so it
    // auto-selects that backend.
    if let Some(uds) = &config.egress_proxy_uds {
        sandbox = sandbox
            .using_bubblewrap()
            .with_egress_proxy_uds(uds.clone());
        if let Some(shim) = &config.egress_shim_path {
            sandbox = sandbox.with_egress_shim_path(shim.clone());
        }
    }

    let table = ProcessTable::new();
    let mut run = ProcessRunTool::new(&root)?
        .with_shell(config.allow_shell)
        .with_sandbox(sandbox.clone());
    let mut spawn = ProcessSpawnTool::new(&root, table.clone())?
        .with_shell(config.allow_shell)
        .with_sandbox(sandbox);
    if let Some(prefixes) = &config.allowed_prefixes {
        run = run.with_allowed_prefixes(prefixes.clone());
        spawn = spawn.with_allowed_prefixes(prefixes.clone());
    }
    if let Some(addr) = config.egress_proxy {
        run = run.with_egress_proxy(addr);
        spawn = spawn.with_egress_proxy(addr);
    }
    if let Some(runtime) = &config.secret_runtime {
        run = run.with_secret_runtime(runtime.clone());
        spawn = spawn.with_secret_runtime(runtime.clone());
    }

    Ok(builder
        .tool(run)
        .tool(spawn)
        .tool(ProcessReadTool::new(table.clone()))
        .tool(ProcessWriteTool::new(table.clone()))
        .tool(ProcessKillTool::new(table)))
}

/// Side-effecting by nature; `process.run` is the only one enabled by default.
pub fn default_process_tool_ids() -> Vec<String> {
    vec![PROCESS_RUN_TOOL_ID.to_string()]
}

pub fn known_process_tool_ids() -> [&'static str; 5] {
    [
        PROCESS_RUN_TOOL_ID,
        PROCESS_SPAWN_TOOL_ID,
        PROCESS_READ_TOOL_ID,
        PROCESS_WRITE_TOOL_ID,
        PROCESS_KILL_TOOL_ID,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids_cover_the_family() {
        let ids = known_process_tool_ids();
        assert!(ids.contains(&PROCESS_RUN_TOOL_ID));
        assert!(ids.contains(&PROCESS_KILL_TOOL_ID));
        assert_eq!(
            default_process_tool_ids(),
            vec![PROCESS_RUN_TOOL_ID.to_string()]
        );
    }
}
