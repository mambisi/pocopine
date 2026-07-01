//! Sandboxed stdio transport (Phase 1).
//!
//! Spawns the configured server through the process tool's `SandboxPolicy`
//! (env-scrub of `PATH`/`LD_*`/`DYLD_*` + secrets, `setrlimit`,
//! Landlock/seccomp, no network by default — D7), then hands the child's
//! `(ChildStdout, ChildStdin)` to rmcp's `serve` via the `(R, W)`
//! `IntoTransport` blanket impl (see [`super::super::client`]). We own
//! kill/wait/reap and drain stderr (bounded) ourselves — stderr is *not* part
//! of the rmcp transport.
//!
//! This module owns only the spawn + child lifecycle; the rmcp handshake and
//! discovery live in [`super::super::client`].

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::sys::signal::Signal;
use pocopine_agenkit::server::SecretString;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::task::JoinHandle;

use crate::tools::mcp::config::{McpServerConfig, McpTransportConfig};
use crate::tools::process::{
    ResourceLimits, SandboxPolicy, prepare_command, shutdown_child, signal_group,
};

/// Cap on captured stderr per server. Server logs are free-form and untrusted;
/// we keep a bounded head for diagnostics and drop the rest.
const STDERR_CAP_BYTES: usize = 64 * 1024;

/// Grace period before a SIGTERM escalates to a SIGKILL when closing.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Derive the sandbox policy for a server from its capability set.
///
/// Default-deny (D7): no capability means [`SandboxPolicy::workspace`] — writes
/// confined to the project root + `/tmp`, **no network** (seccomp denies INET).
///
/// Network is **fail-closed** for a stdio child: the stdio sandbox gates the
/// network with seccomp (`AF_INET`/`AF_INET6` on or off), which **cannot do
/// host/port filtering**, and no egress proxy is wired for the stdio transport
/// in v1. So a `network` capability that declares any `allow_hosts`/`deny_hosts`
/// cannot be honored and is **rejected** here rather than silently granting
/// unrestricted INET — which (unlike the SSRF-guarded HTTP transport) would let
/// the child reach RFC1918 services, the cloud metadata endpoint, and arbitrary
/// exfil hosts. An empty `network: {}` capability matches the documented
/// "empty = deny all" contract on [`NetworkCapability`] and grants **no**
/// network. Host-level egress for a stdio server is deferred to the Tier-2
/// bubblewrap egress proxy (mirroring `process`/`network`,
/// `SandboxPolicy::using_bubblewrap().with_egress_proxy_uds(..)`); until that is
/// wired a stdio server runs with no network.
///
/// [`NetworkCapability`]: crate::tools::mcp::config::NetworkCapability
pub(crate) fn sandbox_for(config: &McpServerConfig, root: &Path) -> AgenkitResult<SandboxPolicy> {
    let policy = SandboxPolicy::workspace(root.to_path_buf());
    if let Some(network) = &config.capabilities.network
        && (!network.allow_hosts.is_empty() || !network.deny_hosts.is_empty())
    {
        return Err(AgenkitError::tool_policy(format!(
            "stdio mcp server `{}` declares a network host allow/deny list, which the \
             stdio sandbox cannot enforce (seccomp gates INET on/off but cannot filter \
             hosts, and no egress proxy is wired for the stdio transport in v1); refusing \
             rather than granting unrestricted network. Use an http transport for a remote \
             endpoint, or drop the host list to run the server with no network.",
            config.name
        )));
    }
    Ok(policy)
}

/// Derive per-child resource limits from the server's process capability.
/// `None` fields fall back to the sandbox defaults.
pub(crate) fn limits_for(config: &McpServerConfig) -> ResourceLimits {
    match &config.capabilities.process {
        Some(process) => ResourceLimits {
            memory_bytes: process.max_memory_bytes,
            cpu_seconds: process.max_cpu_seconds,
            max_processes: process.max_processes,
            file_size_bytes: None,
        },
        None => ResourceLimits::default(),
    }
}

/// A bounded, append-only sink for a child's stderr. New bytes past the cap are
/// counted and dropped so an unbounded server log can't exhaust memory.
struct StderrSink {
    buf: Vec<u8>,
    dropped: usize,
}

/// The spawned child plus the pipes rmcp needs and the stderr drain.
pub(crate) struct SpawnedStdio {
    pub child: Child,
    pub pid: u32,
    pub stdout: ChildStdout,
    pub stdin: ChildStdin,
    pub stderr: StdioStderr,
}

/// Owns the bounded stderr buffer and the task draining it.
pub(crate) struct StdioStderr {
    sink: Arc<Mutex<StderrSink>>,
    reader: JoinHandle<()>,
}

impl StdioStderr {
    /// The captured stderr so far (bounded, lossy UTF-8), with a trailing note
    /// when bytes were dropped to stay within the cap.
    pub(crate) fn text(&self) -> String {
        let sink = self.sink.lock().expect("mcp stderr sink poisoned");
        let mut text = String::from_utf8_lossy(&sink.buf).into_owned();
        if sink.dropped > 0 {
            text.push_str(&format!("\n…({} stderr bytes dropped)…", sink.dropped));
        }
        text
    }

    fn abort(&self) {
        self.reader.abort();
    }
}

/// Spawn the configured server as a sandboxed child and return its pipes.
///
/// Mirrors `process::ProcessSpawnTool::run`: build the confined
/// `std::process::Command` via [`prepare_command`] (env_clear + explicit PATH,
/// dropping `PATH`/`LD_*`/`DYLD_*` from any caller env, new process group, a
/// `pre_exec` applying `setrlimit` + Landlock/seccomp), then convert to a tokio
/// command with piped stdio and `kill_on_drop(true)` so an early return on the
/// handshake path still tears the child down.
///
/// `secrets` carries the per-server env (D6) already resolved from secret
/// handles by the runtime: the values are injected into **this child's**
/// environment only (never config/args/trace) by `prepare_command`. An empty
/// map keeps the env_clear + explicit-PATH scrub in effect so the child inherits
/// nothing else from the host.
pub(crate) fn spawn_stdio_server(
    config: &McpServerConfig,
    root: &Path,
    default_path: &str,
    secrets: &BTreeMap<String, SecretString>,
) -> AgenkitResult<SpawnedStdio> {
    let McpTransportConfig::Stdio {
        command, args, cwd, ..
    } = &config.transport
    else {
        return Err(AgenkitError::config(
            "spawn_stdio_server called with a non-stdio transport",
        ));
    };
    if command.trim().is_empty() {
        return Err(AgenkitError::validation(
            "mcp stdio server command must not be empty",
        ));
    }

    let argv: Vec<String> = std::iter::once(command.clone())
        .chain(args.iter().cloned())
        .collect();
    let sandbox = sandbox_for(config, root)?;
    let limits = limits_for(config);

    let (std_cmd, _resolved_cwd) = prepare_command(
        root,
        &argv,
        /* shell */ false,
        /* allow_shell */ false,
        cwd.as_deref(),
        /* env */ None,
        secrets,
        default_path,
        &limits,
        &sandbox,
        /* egress_proxy */ None,
    )?;

    let mut cmd = tokio::process::Command::from(std_cmd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|err| {
        AgenkitError::config(format!("spawn mcp server `{}`: {err}", config.name))
    })?;
    let pid = child
        .id()
        .ok_or_else(|| AgenkitError::internal("mcp server exited before registration"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgenkitError::internal("mcp server stdout was not piped"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AgenkitError::internal("mcp server stdin was not piped"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| AgenkitError::internal("mcp server stderr was not piped"))?;

    let sink = Arc::new(Mutex::new(StderrSink {
        buf: Vec::new(),
        dropped: 0,
    }));
    let reader = spawn_stderr_drain(stderr_pipe, Arc::clone(&sink), config.name.clone());

    Ok(SpawnedStdio {
        child,
        pid,
        stdout,
        stdin,
        stderr: StdioStderr { sink, reader },
    })
}

/// Drain a child's stderr into the bounded sink. Stops on EOF/error.
fn spawn_stderr_drain<R>(
    mut reader: R,
    sink: Arc<Mutex<StderrSink>>,
    server: String,
) -> JoinHandle<()>
where
    R: AsyncReadExt + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut sink) = sink.lock() {
                        let remaining = STDERR_CAP_BYTES.saturating_sub(sink.buf.len());
                        if remaining == 0 {
                            sink.dropped += n;
                        } else {
                            let take = remaining.min(n);
                            sink.buf.extend_from_slice(&chunk[..take]);
                            sink.dropped += n - take;
                        }
                    }
                }
            }
        }
        tracing::debug!(target: "pocopine.log", server = server.as_str(), "mcp server stderr closed");
    })
}

/// Owns the spawned child's lifecycle for an `McpConnection`: SIGTERM → grace →
/// SIGKILL of the whole process group, reap, and stderr-drain abort.
pub(crate) struct StdioChild {
    /// `None` once the child has been reaped by [`shutdown`](Self::shutdown) (or
    /// handed to the detached reaper on [`Drop`]).
    child: Option<Child>,
    pid: u32,
    stderr: StdioStderr,
}

impl StdioChild {
    pub(crate) fn new(child: Child, pid: u32, stderr: StdioStderr) -> Self {
        Self {
            child: Some(child),
            pid,
            stderr,
        }
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn stderr_text(&self) -> String {
        self.stderr.text()
    }

    /// Group-kill (SIGTERM, grace, SIGKILL), **reap** the child (await `wait`),
    /// and stop draining stderr. A missing group is treated as already gone.
    /// Idempotent: a second call (after the child was already reaped) is a no-op.
    ///
    /// Every async failure path that drops a connection (handshake timeout /
    /// failure, protocol-version rejection, discovery error) calls this *before*
    /// returning, so the child is reaped synchronously rather than being left as a
    /// zombie for a best-effort background reaper (H5).
    pub(crate) async fn shutdown(&mut self) -> AgenkitResult<()> {
        if let Some(mut child) = self.child.take() {
            shutdown_child(&mut child, Some(self.pid), CLOSE_GRACE)
                .await
                .map_err(|err| AgenkitError::internal(format!("reap mcp server: {err}")))?;
        }
        self.stderr.abort();
        Ok(())
    }
}

impl Drop for StdioChild {
    fn drop(&mut self) {
        // SIGKILL the whole group so a backgrounded descendant can't outlive the
        // connection (defense in depth alongside `kill_on_drop(true)`).
        let _ = signal_group(self.pid, Signal::SIGKILL);
        // We can't `.await` in `Drop`, so a still-owned child (a connection
        // dropped without a graceful `shutdown`/`close` — e.g. the last `Arc`
        // falling) must still be **reaped**: a SIGKILLed child is otherwise a
        // zombie until some best-effort reaper happens to run. Hand it to a
        // detached reaper. `shutdown` already reaped on the graceful path (the
        // child is `None` here, so this is skipped).
        if let Some(child) = self.child.take() {
            detach_reaper(child, self.pid);
        }
        self.stderr.abort();
    }
}

/// Reap a (just-SIGKILLed) child without blocking the dropping thread.
///
/// Prefers a detached **tokio task** when a runtime is available (the common
/// case — failure-path drops happen inside `connect`'s async context): the task
/// owns the child, so its `wait()` is the sole reaper (no double-reap race). With
/// no runtime to drive tokio's own `kill_on_drop` reaper (e.g. the last `Arc`
/// dropping during runtime teardown) it falls back to a short-lived thread doing
/// a blocking `waitpid` on the known pid — a stale tokio orphan-queue entry then
/// just gets a harmless `ECHILD`.
fn detach_reaper(child: Child, pid: u32) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let mut child = child;
                let _ = child.wait().await;
            });
        }
        Err(_) => {
            // Drop the tokio handle first (its `kill_on_drop` just re-SIGKILLs,
            // harmless); we own the reap via the blocking `waitpid` below.
            drop(child);
            std::thread::spawn(move || {
                let _ = nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(pid as i32), None);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::config::{CapabilitySet, NetworkCapability, ProcessCapability};

    fn stdio_config() -> McpServerConfig {
        McpServerConfig::stdio("fixture", "server")
    }

    #[test]
    fn sandbox_denies_network_without_capability() {
        let config = stdio_config();
        let policy = sandbox_for(&config, Path::new("/tmp")).unwrap();
        assert!(!policy.allow_network);
        // seccomp will deny AF_INET/AF_INET6 when network is off.
        assert!(policy.deny_inet());
    }

    #[test]
    fn empty_network_capability_is_deny_all_no_inet() {
        // `network: {}` matches the documented "empty = deny all" contract: the
        // capability exists but grants no egress, so the stdio child still has
        // no network (seccomp denies INET) — never an implicit allow-all.
        let mut config = stdio_config();
        config.capabilities = CapabilitySet {
            network: Some(NetworkCapability::default()),
            ..CapabilitySet::default()
        };
        let policy = sandbox_for(&config, Path::new("/tmp")).unwrap();
        assert!(!policy.allow_network);
        assert!(policy.deny_inet());
    }

    #[test]
    fn network_host_list_is_rejected_for_stdio() {
        // A host allow/deny list can't be honored by the stdio seccomp sandbox,
        // so it fails closed instead of granting unrestricted INET (which would
        // reach RFC1918 / the cloud metadata endpoint / exfil hosts).
        let mut config = stdio_config();
        config.capabilities = CapabilitySet {
            network: Some(NetworkCapability {
                allow_hosts: vec!["api.example.com".to_string()],
                deny_hosts: vec![],
            }),
            ..CapabilitySet::default()
        };
        let err = sandbox_for(&config, Path::new("/tmp")).unwrap_err();
        assert_eq!(err.kind(), "tool_policy");

        // A deny-list (allow-all-except) is likewise unenforceable → rejected.
        config.capabilities = CapabilitySet {
            network: Some(NetworkCapability {
                allow_hosts: vec![],
                deny_hosts: vec!["evil.example.com".to_string()],
            }),
            ..CapabilitySet::default()
        };
        let err = sandbox_for(&config, Path::new("/tmp")).unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    #[test]
    fn limits_default_when_no_process_capability() {
        let limits = limits_for(&stdio_config());
        assert!(limits.memory_bytes.is_none());
        assert!(limits.max_processes.is_none());
    }

    #[test]
    fn limits_derived_from_process_capability() {
        let mut config = stdio_config();
        config.capabilities = CapabilitySet {
            process: Some(ProcessCapability {
                max_memory_bytes: Some(128 * 1024 * 1024),
                max_cpu_seconds: Some(10),
                max_processes: Some(4),
            }),
            ..CapabilitySet::default()
        };
        let limits = limits_for(&config);
        assert_eq!(limits.memory_bytes, Some(128 * 1024 * 1024));
        assert_eq!(limits.cpu_seconds, Some(10));
        assert_eq!(limits.max_processes, Some(4));
    }

    /// H5: a `StdioChild` dropped with **no tokio runtime available** to drive
    /// tokio's own `kill_on_drop` reaper (e.g. the last `Arc<McpConnection>`
    /// falling during runtime teardown) must STILL reap the SIGKILLed child via
    /// the [`detach_reaper`] off-runtime `waitpid` fallback — otherwise it lingers
    /// as a zombie. This is the exact gap the pre-fix Drop (group-SIGKILL only)
    /// left: with the runtime gone, nothing reaped the child.
    #[cfg(target_os = "linux")]
    #[test]
    fn dropped_child_is_reaped_without_a_runtime() {
        use std::path::PathBuf;

        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .expect("a `sleep` binary for the test child");

        let dir = tempfile::tempdir().unwrap();
        let root: PathBuf = dir.path().canonicalize().unwrap();
        let mut config = McpServerConfig::stdio("reaptest", sleep);
        if let McpTransportConfig::Stdio { args, .. } = &mut config.transport {
            *args = vec!["30".to_string()];
        }

        // Spawn the sandboxed child inside a runtime, then DROP the runtime so no
        // ambient reaper survives — mirroring a connection dropped after teardown.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let stdio_child = rt.block_on(async {
            let spawned =
                spawn_stdio_server(&config, &root, "/usr/bin:/bin", &BTreeMap::new()).unwrap();
            // Hand off the pipes we don't need (as the real connection hands them
            // to rmcp); keep the child + stderr drain in the StdioChild.
            let SpawnedStdio {
                child,
                pid,
                stdout,
                stdin,
                stderr,
            } = spawned;
            drop((stdout, stdin));
            StdioChild::new(child, pid, stderr)
        });
        let pid = stdio_child.pid();
        assert!(
            Path::new(&format!("/proc/{pid}")).exists(),
            "child {pid} should be alive before drop"
        );

        // No runtime → `detach_reaper` takes the blocking-`waitpid` fallback.
        drop(rt);
        drop(stdio_child);

        // The off-runtime reaper thread reaps promptly after the group SIGKILL;
        // poll briefly so the assertion isn't racy.
        let mut reaped = false;
        for _ in 0..200 {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            reaped,
            "child {pid} must be reaped after a no-runtime drop (not left a zombie)"
        );
    }
}
