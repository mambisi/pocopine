//! Process-execution host tools (unix).
//!
//! Run commands under a confined cwd, a scrubbed environment, a process group,
//! per-child resource limits (`setrlimit`), a wall-clock timeout, and bounded
//! output. argv is the default; `/bin/sh -c` shell mode is a separate grant.
//! Long-running processes are managed through a session-scoped handle table
//! that group-kills and reaps on teardown.
//!
//! On Linux, each child is additionally confined with Landlock (filesystem
//! writes limited to the workspace) and a seccomp filter (deny ptrace/io_uring
//! always, and `AF_INET` sockets when network is off); a bubblewrap
//! namespace backend is also available. See [`SandboxPolicy`] and `README.md`.

mod cgroup;
mod common;
mod egress_shim;
mod handle;
mod registry;
mod run;
mod sandbox;

pub use common::{CapturedStream, ResourceLimits};
// Crate-internal reuse seams for the MCP stdio transport (`tools::mcp`): the
// sandboxed-child builder plus its lifecycle helpers. These stay `pub(crate)`
// (not part of the process tool's public surface) — MCP spawns its server the
// same confined way `process` does instead of reinventing the sandbox.
pub(crate) use common::{prepare_command, shutdown_child, signal_group};
pub use egress_shim::run_egress_shim;
pub use handle::{
    PROCESS_KILL_TOOL_ID, PROCESS_READ_TOOL_ID, PROCESS_SPAWN_TOOL_ID, PROCESS_WRITE_TOOL_ID,
    ProcessKillInput, ProcessKillOutput, ProcessKillTool, ProcessReadInput, ProcessReadOutput,
    ProcessReadTool, ProcessSpawnInput, ProcessSpawnOutput, ProcessSpawnTool, ProcessTable,
    ProcessWriteInput, ProcessWriteOutput, ProcessWriteTool,
};
pub use registry::{
    ProcessToolConfig, default_process_tool_ids, known_process_tool_ids, register_process_tools,
    register_process_tools_with,
};
pub use run::{PROCESS_RUN_TOOL_ID, ProcessRunInput, ProcessRunOutput, ProcessRunTool};
pub use sandbox::{SandboxBackend, SandboxPolicy};
