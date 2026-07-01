use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Component, Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::resource::{Resource, setrlimit};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use pocopine_agenkit::server::SecretString;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use schemars::JsonSchema;
use serde::Serialize;
use tokio::process::Child;

use super::sandbox::{
    SandboxBackend, SandboxInstaller, SandboxPolicy, bwrap_args, trusted_bwrap_path,
};

/// Canonicalize the workspace root (mirrors `fs::common::canonical_root`).
pub(crate) fn canonical_root(root: &Path) -> AgenkitResult<PathBuf> {
    root.canonicalize().map_err(|err| {
        AgenkitError::config(format!("canonicalize root `{}`: {err}", root.display()))
    })
}

/// Resolve a project-relative working directory and confine it to `root`.
/// `None`/empty resolves to the root itself.
pub(crate) fn resolve_cwd(root: &Path, cwd: Option<&str>) -> AgenkitResult<PathBuf> {
    let Some(cwd) = cwd.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(root.to_path_buf());
    };
    let candidate = Path::new(cwd);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(AgenkitError::tool_policy(
            "process cwd must be relative and stay inside the project root",
        ));
    }
    let canonical = root
        .join(candidate)
        .canonicalize()
        .map_err(|err| AgenkitError::not_found(format!("process cwd `{cwd}`: {err}")))?;
    if !canonical.starts_with(root) {
        return Err(AgenkitError::tool_policy(format!(
            "process cwd `{cwd}` escapes the project root"
        )));
    }
    if !canonical.is_dir() {
        return Err(AgenkitError::validation(format!(
            "process cwd `{cwd}` is not a directory"
        )));
    }
    Ok(canonical)
}

/// Display a confined cwd relative to the workspace root (`./sub/dir`).
pub(crate) fn relative_cwd(root: &Path, cwd: &Path) -> String {
    let relative = cwd.strip_prefix(root).unwrap_or(cwd);
    let text = relative.display().to_string().replace('\\', "/");
    if text.is_empty() {
        ".".to_string()
    } else {
        format!("./{text}")
    }
}

/// Per-child resource limits applied with `setrlimit` in a post-fork
/// `pre_exec` hook. `RLIMIT_CORE` is always pinned to 0 (no core dumps, which
/// could leak secrets); the rest are opt-in.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceLimits {
    pub memory_bytes: Option<u64>,
    pub cpu_seconds: Option<u64>,
    pub file_size_bytes: Option<u64>,
    pub max_processes: Option<u64>,
}

fn errno_to_io(err: Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(err as i32)
}

/// Build a `std::process::Command` with confined cwd, scrubbed environment, its
/// own process group, and post-fork resource limits. Stdio is left to the
/// caller. `shell` runs the joined command through `/bin/sh -c` and is only
/// allowed when `allow_shell` is set.
/// Caller-supplied env keys that could subvert the sandbox and are dropped from
/// the request environment: `PATH` (binary resolution — would let an argv-prefix
/// allowlist be satisfied by a workspace-planted binary named e.g. `cargo`) and
/// the dynamic-loader variables (`LD_*` / `DYLD_*`, arbitrary code injection).
/// The host sets `PATH` explicitly; the model never needs to override these.
fn is_unsafe_caller_env(key: &str) -> bool {
    key == "PATH" || key.starts_with("LD_") || key.starts_with("DYLD_")
}

/// The proxy environment variables that route a sandboxed child's outbound
/// HTTP(S) through the host egress proxy. Both upper- and lower-case forms are
/// set since tools differ (cargo/npm/pip/curl/git).
fn proxy_env_vars(addr: SocketAddr) -> Vec<(String, String)> {
    let url = format!("http://{addr}");
    [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .into_iter()
    .map(|key| (key.to_string(), url.clone()))
    .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_command(
    root: &Path,
    command: &[String],
    shell: bool,
    allow_shell: bool,
    cwd: Option<&str>,
    env: Option<&BTreeMap<String, String>>,
    secrets: &BTreeMap<String, SecretString>,
    default_path: &str,
    limits: &ResourceLimits,
    sandbox: &SandboxPolicy,
    egress_proxy: Option<SocketAddr>,
) -> AgenkitResult<(std::process::Command, PathBuf)> {
    if command.is_empty() || command[0].trim().is_empty() {
        return Err(AgenkitError::validation(
            "process command must be a non-empty argv array",
        ));
    }
    if shell && !allow_shell {
        return Err(AgenkitError::tool_policy(
            "process shell mode is not permitted for this tool",
        ));
    }
    // Tier-2 egress lockdown relies on the bubblewrap network namespace.
    if sandbox.egress_proxy_uds.is_some() && !matches!(sandbox.backend, SandboxBackend::Bubblewrap)
    {
        return Err(AgenkitError::config(
            "egress proxy (UDS) requires the bubblewrap backend (it relies on --unshare-net)",
        ));
    }
    let resolved_cwd = resolve_cwd(root, cwd)?;
    // Host egress-proxy vars routed to the child; injected last so host config
    // overrides any caller-supplied proxy value.
    let proxy_vars = egress_proxy.map(proxy_env_vars).unwrap_or_default();

    // The real program + its literal args.
    let (program, args): (String, Vec<String>) = if shell {
        (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), command.join(" ")],
        )
    } else {
        (command[0].clone(), command[1..].to_vec())
    };

    // In bubblewrap mode the command becomes `bwrap <flags> -- <program> <args>`
    // and the in-process Landlock/seccomp installer is skipped (the namespaces
    // do the isolation). Otherwise run the program directly under the installer.
    let mut cmd = match sandbox.backend {
        SandboxBackend::Bubblewrap => {
            // Launch the trusted wrapper by absolute path (never via `PATH`, which
            // the request could redirect) and apply the request env to the
            // sandboxed child via --setenv, not to the wrapper. Drop any request
            // key that collides with a secret, so the host secret (carried in the
            // wrapper's inherited env) still wins — matching the in-process path.
            let bwrap_path = trusted_bwrap_path()?;
            let mut child_env: BTreeMap<String, String> = env
                .map(|env| {
                    env.iter()
                        .filter(|(key, _)| {
                            !secrets.contains_key(*key) && !is_unsafe_caller_env(key)
                        })
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            // Proxy vars reach the sandboxed child via --setenv (not the wrapper),
            // overriding any caller value.
            for (key, value) in &proxy_vars {
                child_env.insert(key.clone(), value.clone());
            }
            let mut bwrap = bwrap_args(sandbox, &resolved_cwd, &child_env);
            // Tier-2: run the user command behind the in-namespace relay shim,
            // which reaches the bound proxy UDS and exports HTTP_PROXY to the
            // child — so the only egress is the allowlisted proxy.
            if let Some(uds) = &sandbox.egress_proxy_uds {
                let shim = match &sandbox.egress_shim_path {
                    Some(path) => path.clone(),
                    None => std::env::current_exe().map_err(|err| {
                        AgenkitError::config(format!("egress shim: current_exe failed: {err}"))
                    })?,
                };
                bwrap.push(shim.display().to_string());
                bwrap.push("__egress-shim".to_string());
                bwrap.push("--uds".to_string());
                bwrap.push(uds.display().to_string());
                bwrap.push("--".to_string());
            }
            bwrap.push(program);
            bwrap.extend(args);
            let mut cmd = std::process::Command::new(&bwrap_path);
            cmd.args(&bwrap);
            cmd
        }
        SandboxBackend::InProcess => {
            let mut cmd = std::process::Command::new(&program);
            cmd.args(&args);
            cmd
        }
    };

    // Start from an empty environment, inject only an explicit PATH plus any
    // caller-provided allowlisted vars. Never inherit the agent's environment.
    // In bubblewrap mode the request env is NOT applied to the (unsandboxed)
    // wrapper — it went to the child via --setenv — so it can't influence the
    // wrapper (e.g. via `LD_PRELOAD`).
    cmd.env_clear();
    cmd.env("PATH", default_path);
    if let (SandboxBackend::InProcess, Some(env)) = (sandbox.backend, env) {
        for (key, value) in env {
            if is_unsafe_caller_env(key) {
                continue;
            }
            cmd.env(key, value);
        }
    }
    // Egress-proxy vars, injected last so the host's proxy config wins over any
    // caller-supplied value. (Bubblewrap got them via --setenv above; setting
    // them on the wrapper would leak them to the unsandboxed `bwrap` process.)
    if matches!(sandbox.backend, SandboxBackend::InProcess) {
        for (key, value) in &proxy_vars {
            cmd.env(key, value);
        }
    }
    // Host-provided secrets resolved at spawn into the environment (never argv,
    // which is world-readable via `/proc/<pid>/cmdline`). In bubblewrap mode
    // they ride the wrapper's owner-only environment and bwrap passes them to
    // the child. Their values are redacted from captured output by the caller.
    for (key, value) in secrets {
        cmd.env(key, value.expose());
    }
    cmd.current_dir(&resolved_cwd);
    // New process group (pgid == child pid) so a timeout/kill reaches the whole
    // tree, not just the direct child.
    cmd.process_group(0);

    let limits = *limits;
    // The Landlock ruleset + seccomp filter are built here, in the parent, where
    // allocation is safe; the child only applies them (syscalls only). In
    // bubblewrap mode `bwrap` does the filesystem/network isolation, so we apply
    // *only* the seccomp filter (ptrace/io_uring + INET-when-off) — it rides the
    // wrapper's exec into the child, and none of its denied syscalls are used by
    // bwrap's namespace setup.
    let mut sandbox = match sandbox.backend {
        SandboxBackend::InProcess => Some(SandboxInstaller::build(sandbox)?),
        SandboxBackend::Bubblewrap => Some(SandboxInstaller::seccomp_only(sandbox)?),
    };
    // SAFETY: the closure runs in the forked child after `fork(2)` and before
    // `execvp(2)`. It performs only `setrlimit(2)` and the pre-built sandbox's
    // enforcement syscalls (Landlock `restrict_self`, seccomp `apply`) —
    // async-signal-safe, no allocation, no locks — and returns an `io::Error` on
    // failure so the spawn fails closed rather than running an unconfined child.
    unsafe {
        cmd.pre_exec(move || {
            setrlimit(Resource::RLIMIT_CORE, 0, 0).map_err(errno_to_io)?;
            if let Some(bytes) = limits.memory_bytes {
                setrlimit(Resource::RLIMIT_AS, bytes, bytes).map_err(errno_to_io)?;
            }
            if let Some(seconds) = limits.cpu_seconds {
                setrlimit(Resource::RLIMIT_CPU, seconds, seconds).map_err(errno_to_io)?;
            }
            if let Some(bytes) = limits.file_size_bytes {
                setrlimit(Resource::RLIMIT_FSIZE, bytes, bytes).map_err(errno_to_io)?;
            }
            if let Some(count) = limits.max_processes {
                setrlimit(Resource::RLIMIT_NPROC, count, count).map_err(errno_to_io)?;
            }
            // Block ptrace / `/proc/<pid>/mem` reads of this process's memory
            // (defense in depth alongside RLIMIT_CORE=0, so secrets in the child
            // can't be lifted out by another local process). `prctl` is unsafe but
            // async-signal-safe; the call sits inside the outer `pre_exec` unsafe
            // block.
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Kernel confinement last: Landlock (filesystem) then seccomp
            // (syscalls). In bubblewrap mode this is seccomp-only (bwrap does the
            // filesystem/network isolation); the filter is inherited by the child.
            if let Some(sandbox) = sandbox.as_mut() {
                sandbox.apply()?;
            }
            Ok(())
        });
    }

    Ok((cmd, resolved_cwd))
}

/// Replace any occurrence of a secret value with `[redacted]`, so secrets a
/// command echoes don't reach the model. The values stay inside `SecretString`
/// (zeroized, redacted-on-debug); each is `expose()`d only here, transiently,
/// rather than copied out into a long-lived plaintext buffer. Empty values are
/// ignored.
pub(crate) fn redact(text: &str, secrets: &BTreeMap<String, SecretString>) -> String {
    let mut out = text.to_string();
    for value in secrets.values() {
        let secret = value.expose();
        if !secret.is_empty() && out.contains(secret) {
            out = out.replace(secret, "[redacted]");
        }
    }
    out
}

/// Enforce an optional argv-prefix allowlist (`CapabilitySet.commands`). Prefix
/// match, not substring. Shell mode bypasses this (it has its own gate).
pub(crate) fn check_allowed_prefixes(
    allowed: &Option<Vec<Vec<String>>>,
    command: &[String],
) -> AgenkitResult<()> {
    let Some(allowed) = allowed else {
        return Ok(());
    };
    let permitted = allowed
        .iter()
        .any(|prefix| !prefix.is_empty() && command.starts_with(prefix));
    if permitted {
        Ok(())
    } else {
        Err(AgenkitError::tool_policy(format!(
            "process command `{}` is not in the allowed command list",
            command.join(" ")
        )))
    }
}

/// Exit code and terminating signal as distinct fields.
pub(crate) fn exit_fields(status: &ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), status.signal())
}

/// Signal a whole process group; a missing group (`ESRCH`) is treated as done.
pub(crate) fn signal_group(pgid: u32, signal: Signal) -> Result<(), Errno> {
    match killpg(Pid::from_raw(pgid as i32), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Graceful group shutdown: SIGTERM, wait up to `grace`, then SIGKILL + reap.
pub(crate) async fn shutdown_child(
    child: &mut Child,
    pgid: Option<u32>,
    grace: Duration,
) -> std::io::Result<ExitStatus> {
    if let Some(pgid) = pgid {
        let _ = signal_group(pgid, Signal::SIGTERM);
    }
    // Give the group a grace period to exit on SIGTERM.
    let graceful = tokio::time::timeout(grace, child.wait()).await;
    // Then SIGKILL the WHOLE group unconditionally — even if the *direct* child
    // already exited, a backgrounded descendant in the group may still be alive
    // (e.g. holding the stdio that kept `process.run` from completing). Gating the
    // SIGKILL on the timeout would let such a descendant survive the tool.
    if let Some(pgid) = pgid {
        let _ = signal_group(pgid, Signal::SIGKILL);
    }
    match graceful {
        Ok(result) => result,
        Err(_) => child.wait().await,
    }
}

/// A bounded captured stream: text plus what was dropped to stay in budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CapturedStream {
    pub text: String,
    pub truncated: bool,
    pub omitted_bytes: usize,
    pub omitted_lines: usize,
}

/// Deterministic head+tail truncation enforcing both a byte cap and a line cap,
/// with an explicit `…(truncated)…` marker (mirrors Codex/opencode output caps).
pub(crate) fn truncate_output(bytes: &[u8], max_bytes: usize, max_lines: usize) -> CapturedStream {
    let full = String::from_utf8_lossy(bytes).into_owned();
    let total_bytes = full.len();
    let lines: Vec<&str> = full.lines().collect();
    let total_lines = lines.len();

    if total_bytes <= max_bytes && total_lines <= max_lines {
        return CapturedStream {
            text: full,
            truncated: false,
            omitted_bytes: 0,
            omitted_lines: 0,
        };
    }

    let (head_n, tail_n) = if total_lines > max_lines {
        let head = (max_lines / 2).max(1).min(total_lines);
        let tail = max_lines.saturating_sub(head).min(total_lines - head);
        (head, tail)
    } else {
        (total_lines, 0)
    };
    let omitted_lines = total_lines.saturating_sub(head_n + tail_n);

    let head = lines[..head_n].join("\n");
    let tail = if tail_n > 0 {
        lines[total_lines - tail_n..].join("\n")
    } else {
        String::new()
    };

    let mut text = String::with_capacity(head.len() + tail.len() + 16);
    text.push_str(&head);
    text.push_str("\n…(truncated)…\n");
    text.push_str(&tail);

    if text.len() > max_bytes {
        let mut end = max_bytes.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n…(truncated)…");
    }

    let omitted_bytes = total_bytes.saturating_sub(text.len());
    CapturedStream {
        text,
        truncated: true,
        omitted_bytes,
        omitted_lines,
    }
}

/// A bounded FIFO byte buffer for live process output. New bytes evict the
/// oldest once `cap` is exceeded; `dropped` counts evicted-then-unread bytes.
pub(crate) struct OutputRing {
    buf: VecDeque<u8>,
    dropped: u64,
    cap: usize,
}

impl OutputRing {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::new(),
            dropped: 0,
            cap,
        }
    }

    pub(crate) fn append(&mut self, data: &[u8]) {
        self.buf.extend(data.iter().copied());
        while self.buf.len() > self.cap {
            self.buf.pop_front();
            self.dropped += 1;
        }
    }

    /// Take everything accumulated since the last drain plus the dropped count.
    pub(crate) fn drain(&mut self) -> (Vec<u8>, u64) {
        let bytes: Vec<u8> = self.buf.drain(..).collect();
        let dropped = self.dropped;
        self.dropped = 0;
        (bytes, dropped)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::tools::process::sandbox::SandboxPolicy;

    #[test]
    fn proxy_env_vars_cover_upper_and_lower_forms() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let vars = proxy_env_vars(addr);
        assert_eq!(vars.len(), 6);
        for key in ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"] {
            assert!(
                vars.iter()
                    .any(|(k, v)| k == key && v == "http://127.0.0.1:8080"),
                "missing {key}"
            );
        }
    }

    #[test]
    fn prepare_command_injects_proxy_env_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical_root(dir.path()).unwrap();
        let sandbox = SandboxPolicy::workspace(&root);
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let (cmd, _cwd) = prepare_command(
            &root,
            &["true".to_string()],
            false,
            false,
            None,
            None,
            &BTreeMap::new(),
            "/usr/bin:/bin",
            &ResourceLimits::default(),
            &sandbox,
            Some(addr),
        )
        .unwrap();
        let want = OsStr::new("http://127.0.0.1:9999");
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "HTTP_PROXY" && v == Some(want)),
            "HTTP_PROXY should be injected into the child env"
        );
    }

    #[test]
    fn prepare_command_omits_proxy_env_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical_root(dir.path()).unwrap();
        let sandbox = SandboxPolicy::workspace(&root);
        let (cmd, _cwd) = prepare_command(
            &root,
            &["true".to_string()],
            false,
            false,
            None,
            None,
            &BTreeMap::new(),
            "/usr/bin:/bin",
            &ResourceLimits::default(),
            &sandbox,
            None,
        )
        .unwrap();
        assert!(
            !cmd.get_envs().any(|(k, _)| k == "HTTP_PROXY"),
            "no proxy env when none configured"
        );
    }

    #[test]
    fn unsafe_caller_env_keys_are_recognized() {
        assert!(is_unsafe_caller_env("PATH"));
        assert!(is_unsafe_caller_env("LD_PRELOAD"));
        assert!(is_unsafe_caller_env("LD_LIBRARY_PATH"));
        assert!(is_unsafe_caller_env("DYLD_INSERT_LIBRARIES"));
        assert!(!is_unsafe_caller_env("FOO"));
        assert!(!is_unsafe_caller_env("HTTP_PROXY"));
    }

    #[test]
    fn prepare_command_drops_caller_path_and_loader_vars() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical_root(dir.path()).unwrap();
        let sandbox = SandboxPolicy::workspace(&root);
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/evil/bin".to_string());
        env.insert("LD_PRELOAD".to_string(), "/evil/x.so".to_string());
        env.insert("FOO".to_string(), "bar".to_string());
        let (cmd, _cwd) = prepare_command(
            &root,
            &["true".to_string()],
            false,
            false,
            None,
            Some(&env),
            &BTreeMap::new(),
            "/usr/bin:/bin",
            &ResourceLimits::default(),
            &sandbox,
            None,
        )
        .unwrap();
        // PATH is the host's, never the caller's; LD_PRELOAD is dropped; an
        // ordinary var survives.
        let path = cmd
            .get_envs()
            .find(|(k, _)| *k == OsStr::new("PATH"))
            .and_then(|(_, v)| v);
        assert_eq!(path, Some(OsStr::new("/usr/bin:/bin")));
        assert!(cmd.get_envs().all(|(k, _)| k != OsStr::new("LD_PRELOAD")));
        let foo = cmd
            .get_envs()
            .find(|(k, _)| *k == OsStr::new("FOO"))
            .and_then(|(_, v)| v);
        assert_eq!(foo, Some(OsStr::new("bar")));
    }
}
