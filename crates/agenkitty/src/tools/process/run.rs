use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture, Principal, SecretString};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use super::cgroup::CgroupGuard;
use super::common::{
    CapturedStream, ResourceLimits, check_allowed_prefixes, exit_fields, prepare_command, redact,
    relative_cwd, shutdown_child, truncate_output,
};
use super::sandbox::SandboxPolicy;
use crate::tools::secrets::{SecretRuntime, validate_secret_env_name};

pub const PROCESS_RUN_TOOL_ID: &str = "process.run";

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
const DEFAULT_MAX_STDOUT_BYTES: usize = 64_000;
const DEFAULT_MAX_STDERR_BYTES: usize = 32_000;
const DEFAULT_MAX_LINES: usize = 400;
const CAPTURE_HARD_CAP_BYTES: usize = 1_048_576;
const KILL_GRACE: Duration = Duration::from_secs(2);

/// Run a command to completion under explicit cwd, environment, timeout, and
/// output limits. argv-only by default; shell mode is a separate stronger gate.
#[derive(Clone, Debug)]
pub struct ProcessRunTool {
    root: PathBuf,
    allow_shell: bool,
    allowed_prefixes: Option<Vec<Vec<String>>>,
    default_path: String,
    limits: ResourceLimits,
    sandbox: SandboxPolicy,
    secrets: BTreeMap<String, SecretString>,
    secret_runtime: Option<Arc<SecretRuntime>>,
    egress_proxy: Option<SocketAddr>,
}

impl ProcessRunTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        let root = super::common::canonical_root(root.as_ref())?;
        Ok(Self {
            sandbox: SandboxPolicy::workspace(root.clone()),
            root,
            allow_shell: false,
            allowed_prefixes: None,
            default_path: default_path(),
            limits: ResourceLimits::default(),
            secrets: BTreeMap::new(),
            secret_runtime: None,
            egress_proxy: None,
        })
    }

    /// Inject a host-provided secret as an environment variable in the child
    /// (never argv), redacting its value from captured output.
    pub fn with_secret_env(mut self, name: impl Into<String>, value: SecretString) -> Self {
        self.secrets.insert(name.into(), value);
        self
    }

    /// Attach the shared secret-handle runtime used by `secret.request`.
    pub fn with_secret_runtime(mut self, runtime: Arc<SecretRuntime>) -> Self {
        self.secret_runtime = Some(runtime);
        self
    }

    /// Permit `shell: true` (run through `/bin/sh -c`). Off by default.
    pub fn with_shell(mut self, allow: bool) -> Self {
        self.allow_shell = allow;
        self
    }

    /// Restrict to an argv-prefix allowlist (`CapabilitySet.commands`).
    pub fn with_allowed_prefixes(mut self, prefixes: Vec<Vec<String>>) -> Self {
        self.allowed_prefixes = Some(prefixes);
        self
    }

    /// Override the `PATH` injected into the scrubbed child environment.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.default_path = path.into();
        self
    }

    /// Set default per-child resource limits.
    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Override the kernel sandbox policy (filesystem confinement + network).
    pub fn with_sandbox(mut self, sandbox: SandboxPolicy) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Route the child's outbound HTTP(S) through a host egress proxy (sets
    /// `HTTP_PROXY`/`HTTPS_PROXY`/…). Requires the sandbox to permit reaching the
    /// proxy address; see the network tool's `EgressProxy`.
    pub fn with_egress_proxy(mut self, addr: SocketAddr) -> Self {
        self.egress_proxy = Some(addr);
        self
    }

    pub async fn run(&self, input: ProcessRunInput) -> AgenkitResult<ProcessRunOutput> {
        self.run_for(input, &Principal::anonymous()).await
    }

    pub async fn run_for(
        &self,
        input: ProcessRunInput,
        caller: &Principal,
    ) -> AgenkitResult<ProcessRunOutput> {
        let shell = input.shell.unwrap_or(false);
        if !shell {
            check_allowed_prefixes(&self.allowed_prefixes, &input.command)?;
        }

        let mut limits = self.limits;
        if let Some(mb) = input.memory_mb {
            limits.memory_bytes = Some(mb.saturating_mul(1_048_576));
        }

        // Best-effort cgroup (memory.max/pids.max); falls back to rlimits when
        // the host doesn't delegate a writable subtree. Held until the child is
        // reaped, then removed on drop.
        let cgroup = CgroupGuard::create(limits.memory_bytes, limits.max_processes);
        let secrets = self.resolve_secret_env(&input.secret_env, caller).await?;

        let (std_cmd, resolved_cwd) = prepare_command(
            &self.root,
            &input.command,
            shell,
            self.allow_shell,
            input.cwd.as_deref(),
            input.env.as_ref(),
            &secrets,
            &self.default_path,
            &limits,
            &self.sandbox,
            self.egress_proxy,
        )?;

        let mut cmd = tokio::process::Command::from(std_cmd);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let timeout = Duration::from_millis(
            input
                .timeout_ms
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .clamp(1, MAX_TIMEOUT_MS),
        );

        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|err| {
            AgenkitError::not_found(format!("spawn `{}`: {err}", input.command.join(" ")))
        })?;
        let pgid = child.id();
        if let (Some(cgroup), Some(pid)) = (cgroup.as_ref(), pgid) {
            cgroup.add_pid(pid);
        }
        // Drain stdout/stderr into shared capped buffers *inside* the timeout, so
        // completion means "the child exited AND every stdio holder closed the
        // pipes". A command that backgrounds a child and exits keeps the pipe
        // open, so it still hits the wall-clock limit instead of leaking the
        // group. The buffers are shared, so output read before a timeout survives
        // the cancellation.
        let stdout_buf = Arc::new(Mutex::new(CappedBuf::new(CAPTURE_HARD_CAP_BYTES)));
        let stderr_buf = Arc::new(Mutex::new(CappedBuf::new(CAPTURE_HARD_CAP_BYTES)));
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let captured = {
            let out_buf = Arc::clone(&stdout_buf);
            let err_buf = Arc::clone(&stderr_buf);
            let child_ref = &mut child;
            tokio::time::timeout(timeout, async move {
                tokio::join!(read_into(stdout, out_buf), read_into(stderr, err_buf));
                child_ref.wait().await
            })
            .await
        };

        let (status, timed_out) = match captured {
            Ok(Ok(status)) => (Some(status), false),
            Ok(Err(err)) => {
                return Err(AgenkitError::internal(format!(
                    "run `{}`: {err}",
                    input.command.join(" ")
                )));
            }
            // Timed out (child still running, or a descendant still holds stdio):
            // tear down the whole group and reap. Output read so far is kept.
            Err(_elapsed) => (
                shutdown_child(&mut child, pgid, KILL_GRACE).await.ok(),
                true,
            ),
        };

        let (out_bytes, out_over) = take_capped(stdout_buf);
        let (err_bytes, err_over) = take_capped(stderr_buf);
        let (exit_code, signal) = match status.as_ref() {
            Some(status) => exit_fields(status),
            None => (None, None),
        };
        let duration_ms = started.elapsed().as_millis() as u64;

        // Observability per RFC-069: program name + outcome only — never args or
        // output, which can carry sensitive data. In shell mode the first argv
        // element is the whole script, so log `/bin/sh` instead.
        let logged_program = if shell {
            "/bin/sh"
        } else {
            input.command.first().map(String::as_str).unwrap_or("")
        };
        tracing::info!(
            target: "pocopine.log",
            tool = PROCESS_RUN_TOOL_ID,
            program = logged_program,
            exit_code = ?exit_code,
            signal = ?signal,
            timed_out,
            duration_ms,
            "process.run finished"
        );

        Ok(ProcessRunOutput {
            command: input.command,
            cwd: relative_cwd(&self.root, &resolved_cwd),
            exit_code,
            signal,
            timed_out,
            stdout: finish_stream(&out_bytes, out_over, DEFAULT_MAX_STDOUT_BYTES, &secrets),
            stderr: finish_stream(&err_bytes, err_over, DEFAULT_MAX_STDERR_BYTES, &secrets),
            duration_ms,
        })
    }

    async fn resolve_secret_env(
        &self,
        requested: &BTreeMap<String, String>,
        caller: &Principal,
    ) -> AgenkitResult<BTreeMap<String, SecretString>> {
        let mut secrets = self.secrets.clone();
        if requested.is_empty() {
            return Ok(secrets);
        }
        let runtime = self.secret_runtime.as_ref().ok_or_else(|| {
            AgenkitError::tool_policy("process secret handles are not configured for this runtime")
        })?;
        for (env_name, handle_id) in requested {
            validate_secret_env_name(env_name)?;
            let secret = runtime
                .resolve_handle(
                    handle_id,
                    caller,
                    PROCESS_RUN_TOOL_ID,
                    Some(env_name.as_str()),
                )
                .await?;
            secrets.insert(env_name.clone(), secret);
        }
        Ok(secrets)
    }
}

fn default_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string())
}

fn finish_stream(
    bytes: &[u8],
    overflowed: bool,
    max_bytes: usize,
    secrets: &BTreeMap<String, SecretString>,
) -> CapturedStream {
    let text = redact(&String::from_utf8_lossy(bytes), secrets);
    let mut stream = truncate_output(text.as_bytes(), max_bytes, DEFAULT_MAX_LINES);
    if overflowed {
        stream.truncated = true;
    }
    stream
}

/// A buffer that keeps the first `cap` bytes (head) and flags any overflow.
struct CappedBuf {
    buf: Vec<u8>,
    cap: usize,
    overflow: bool,
}

impl CappedBuf {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
            overflow: false,
        }
    }

    fn append(&mut self, data: &[u8]) {
        if self.buf.len() < self.cap {
            let take = (self.cap - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            if take < data.len() {
                self.overflow = true;
            }
        } else {
            self.overflow = true;
        }
    }
}

/// Drain a stream into a shared capped buffer, continuing to read past the cap
/// so the child never blocks on a full pipe. Runs inline within the capture
/// timeout; the buffer is shared, so output read before a timeout survives the
/// cancellation, and the read completing means the pipe reached EOF.
async fn read_into<R>(reader: Option<R>, buf: Arc<Mutex<CappedBuf>>)
where
    R: AsyncReadExt + Unpin,
{
    let Some(mut reader) = reader else {
        return;
    };
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Ok(mut buf) = buf.lock() {
                    buf.append(&chunk[..n]);
                }
            }
        }
    }
}

/// Take the captured bytes out of a (now reader-idle) shared buffer.
fn take_capped(buf: Arc<Mutex<CappedBuf>>) -> (Vec<u8>, bool) {
    match Arc::try_unwrap(buf) {
        Ok(mutex) => {
            let buf = mutex.into_inner().unwrap_or_else(|err| err.into_inner());
            (buf.buf, buf.overflow)
        }
        Err(arc) => {
            let buf = arc.lock().unwrap_or_else(|err| err.into_inner());
            (buf.buf.clone(), buf.overflow)
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ProcessRunInput {
    /// argv array. The first element is the program; remaining elements are
    /// literal arguments (no shell interpretation in the default mode).
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Explicit environment variables. The child starts from an empty
    /// environment plus `PATH`; only these keys are added.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    /// Map child env var name -> approved secret handle. Values are resolved by
    /// the host at spawn time and never appear in model-visible JSON.
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Optional address-space cap in MiB (`RLIMIT_AS`).
    #[serde(default)]
    pub memory_mb: Option<u64>,
    /// Run through `/bin/sh -c` (requires the tool's shell grant).
    #[serde(default)]
    pub shell: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProcessRunOutput {
    pub command: Vec<String>,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub stdout: CapturedStream,
    pub stderr: CapturedStream,
    pub duration_ms: u64,
}

impl AiTool for ProcessRunTool {
    const ID: &'static str = PROCESS_RUN_TOOL_ID;
    type Input = ProcessRunInput;
    type Output = ProcessRunOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            PROCESS_RUN_TOOL_ID,
            "Run a command to completion under a confined cwd, scrubbed environment, timeout, \
             and bounded output. Provide an argv array (no shell). Returns exit code, signal, \
             and truncated stdout/stderr.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let caller = ctx.principal().clone();
        Box::pin(async move { self.run_for(input, &caller).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ToolMode;
    use crate::tools::secrets::{
        InMemorySecretResolver, SecretMetadata, SecretRequestInput, SecretRuntime, SecretScope,
    };

    fn tool() -> ProcessRunTool {
        let dir = std::env::temp_dir();
        ProcessRunTool::new(dir).unwrap()
    }

    fn input(command: &[&str]) -> ProcessRunInput {
        ProcessRunInput {
            command: command.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            env: None,
            secret_env: BTreeMap::new(),
            timeout_ms: Some(10_000),
            memory_mb: None,
            shell: None,
        }
    }

    #[tokio::test]
    async fn runs_argv_and_captures_stdout() {
        let out = tool().run(input(&["echo", "hello"])).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.text.contains("hello"));
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn reports_nonzero_exit() {
        let out = tool().run(input(&["false"])).await.unwrap();
        assert_eq!(out.exit_code, Some(1));
    }

    #[tokio::test]
    async fn empty_command_is_validation_error() {
        let err = tool().run(input(&[])).await.unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[tokio::test]
    async fn cwd_escape_is_tool_policy() {
        let mut req = input(&["echo", "x"]);
        req.cwd = Some("../".to_string());
        let err = tool().run(req).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    #[tokio::test]
    async fn environment_is_scrubbed() {
        // HOME is not in the allowlist, so `printenv HOME` finds nothing (exit 1).
        let out = tool().run(input(&["printenv", "HOME"])).await.unwrap();
        assert_eq!(out.exit_code, Some(1));
        assert!(out.stdout.text.trim().is_empty());
    }

    #[tokio::test]
    async fn explicit_env_is_passed() {
        let mut req = input(&["printenv", "AGENKITTY_TEST"]);
        req.env = Some(BTreeMap::from([(
            "AGENKITTY_TEST".to_string(),
            "present".to_string(),
        )]));
        let out = tool().run(req).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout.text.trim(), "present");
    }

    #[tokio::test]
    async fn secret_env_is_injected_and_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ProcessRunTool::new(dir.path())
            .unwrap()
            .with_secret_env("API_TOKEN", SecretString::new("s3cr3t-value".to_string()));

        // The child sees the secret (printenv finds it, exit 0), but the value
        // is redacted out of the captured output before it reaches the model.
        let out = tool.run(input(&["printenv", "API_TOKEN"])).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(
            !out.stdout.text.contains("s3cr3t-value"),
            "secret leaked into output: {:?}",
            out.stdout.text
        );
        assert!(out.stdout.text.contains("[redacted]"));
    }

    #[tokio::test]
    async fn secret_handle_env_is_injected_and_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let principal = Principal::anonymous();
        let runtime = Arc::new(
            SecretRuntime::new(Arc::new(
                InMemorySecretResolver::new().insert(
                    SecretMetadata::new("api-token", "API token", SecretScope::User)
                        .with_purposes(["command-auth"])
                        .with_target_tools([PROCESS_RUN_TOOL_ID])
                        .with_destinations(["API_TOKEN"]),
                    SecretString::new("handle-secret-value".to_string()),
                ),
            ))
            .with_request_mode(ToolMode::Allow),
        );
        let grant = runtime
            .request(
                SecretRequestInput {
                    secret_ref: "api-token".to_string(),
                    purpose: "command-auth".to_string(),
                    target_tool: PROCESS_RUN_TOOL_ID.to_string(),
                    destination: Some("API_TOKEN".to_string()),
                    ttl_ms: None,
                    inheritable: None,
                },
                &principal,
            )
            .await
            .unwrap();

        let mut req = input(&["printenv", "API_TOKEN"]);
        req.secret_env
            .insert("API_TOKEN".to_string(), grant.handle_id);
        let out = ProcessRunTool::new(dir.path())
            .unwrap()
            .with_secret_runtime(runtime)
            .run_for(req, &principal)
            .await
            .unwrap();

        assert_eq!(out.exit_code, Some(0));
        assert!(!out.stdout.text.contains("handle-secret-value"));
        assert!(out.stdout.text.contains("[redacted]"));
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn run_emits_a_trace_event() {
        tool().run(input(&["echo", "hi"])).await.unwrap();
        // Observability goes through `tracing` (RFC-069), not raw stdio.
        assert!(logs_contain("process.run finished"));
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn shell_mode_does_not_log_the_script() {
        // In shell mode the first argv element is the whole script (which may
        // carry secrets); only `/bin/sh` should be logged, never the script.
        let mut req = input(&["echo do-not-log-this-script"]);
        req.shell = Some(true);
        tool().with_shell(true).run(req).await.unwrap();
        assert!(
            !logs_contain("do-not-log-this-script"),
            "the shell script leaked into the trace log"
        );
        assert!(logs_contain("/bin/sh"));
    }

    #[tokio::test]
    async fn timeout_kills_the_process() {
        let out = tool()
            .run(ProcessRunInput {
                command: vec!["sleep".to_string(), "5".to_string()],
                cwd: None,
                env: None,
                secret_env: BTreeMap::new(),
                timeout_ms: Some(150),
                memory_mb: None,
                shell: None,
            })
            .await
            .unwrap();
        assert!(out.timed_out);
        assert!(out.duration_ms < 4_000);
    }

    #[tokio::test]
    async fn shell_mode_denied_without_grant() {
        let mut req = input(&["echo hi"]);
        req.shell = Some(true);
        let err = tool().run(req).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    #[tokio::test]
    async fn shell_mode_runs_when_granted() {
        let mut req = input(&["echo from-shell"]);
        req.shell = Some(true);
        let out = tool().with_shell(true).run(req).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.text.contains("from-shell"));
    }

    #[tokio::test]
    async fn allowed_prefixes_block_other_commands() {
        let tool = tool().with_allowed_prefixes(vec![vec!["echo".to_string()]]);
        assert!(tool.run(input(&["echo", "ok"])).await.is_ok());
        let err = tool.run(input(&["cat", "x"])).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    #[tokio::test]
    async fn large_output_is_truncated() {
        let out = tool().run(input(&["seq", "1", "5000"])).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.truncated);
        assert!(out.stdout.omitted_lines > 0);
    }

    #[tokio::test]
    async fn timeout_preserves_partial_output() {
        // Print, then hang past the timeout. The early output must survive the
        // kill rather than being lost with the cancelled read futures.
        let mut req = input(&["printf early-output; sleep 5"]);
        req.shell = Some(true);
        req.timeout_ms = Some(400);
        let out = tool().with_shell(true).run(req).await.unwrap();
        assert!(out.timed_out);
        assert!(
            out.stdout.text.contains("early-output"),
            "stdout was {:?}",
            out.stdout.text
        );
    }

    #[tokio::test]
    async fn timeout_fires_for_backgrounded_descendant() {
        // The shell exits immediately but backgrounds a sleeper that inherits
        // stdio. The wall-clock limit must still fire (so the group is killed)
        // rather than returning `timed_out: false` and leaking the descendant.
        let mut req = input(&["sleep 5 & echo started"]);
        req.shell = Some(true);
        req.timeout_ms = Some(400);
        let out = tool().with_shell(true).run(req).await.unwrap();
        assert!(
            out.timed_out,
            "a backgrounded child holding stdio must keep the run bounded by the timeout"
        );
        assert!(out.duration_ms < 4_000);
    }
}

// Kernel-sandbox behaviour (Phase 5) is Linux-only.
#[cfg(all(test, target_os = "linux"))]
mod sandbox_tests {
    use std::collections::BTreeMap;

    use pocopine_agenkit::server::SecretString;

    use super::{ProcessRunInput, ProcessRunTool};
    use crate::tools::process::{SandboxBackend, SandboxPolicy};

    fn run_input(command: &[&str]) -> ProcessRunInput {
        ProcessRunInput {
            command: command.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            env: None,
            secret_env: BTreeMap::new(),
            timeout_ms: Some(10_000),
            memory_mb: None,
            shell: None,
        }
    }

    fn have(binary: &str) -> bool {
        std::process::Command::new(binary)
            .arg("--version")
            .output()
            .is_ok()
    }

    /// `bwrap` can be installed yet unable to create namespaces (e.g. userns
    /// disabled in CI). Probe a real minimal sandbox, not just `--version`.
    fn bwrap_usable() -> bool {
        std::process::Command::new("bwrap")
            .args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-pid",
                "--",
                "/bin/true",
            ])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// Landlock confines writes to the granted roots: a write inside succeeds,
    /// the same write to a path outside is denied even though the user could
    /// normally create it.
    #[tokio::test]
    async fn landlock_blocks_writes_outside_writable_roots() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let writable = workspace.path().canonicalize().unwrap();
        let tool = ProcessRunTool::new(&writable)
            .unwrap()
            .with_sandbox(SandboxPolicy::confined(vec![writable.clone()]));

        // Allowed: write inside the workspace (the run's cwd).
        let allowed = tool
            .run(run_input(&["touch", "allowed.txt"]))
            .await
            .unwrap();
        assert_eq!(allowed.exit_code, Some(0));
        assert!(writable.join("allowed.txt").exists());

        // Denied: write to a path outside the writable roots.
        let denied_path = outside.path().canonicalize().unwrap().join("denied.txt");
        let denied = tool
            .run(run_input(&["touch", denied_path.to_str().unwrap()]))
            .await
            .unwrap();
        assert_ne!(denied.exit_code, Some(0));
        assert!(!denied_path.exists());
    }

    /// seccomp denies `AF_INET` socket creation when network is off, and allows
    /// it when network is on. Skipped when python3 is unavailable.
    #[tokio::test]
    async fn seccomp_blocks_inet_sockets_when_network_off() {
        if !have("python3") {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let probe = "import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM)";

        // Network off (filesystem unconfined so python reads its stdlib freely).
        let off = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy {
                confine_filesystem: false,
                writable_roots: vec![],
                allow_network: false,
                egress_proxy_uds: None,
                egress_shim_path: None,
                backend: SandboxBackend::InProcess,
            });
        let blocked = off.run(run_input(&["python3", "-c", probe])).await.unwrap();
        assert_ne!(
            blocked.exit_code,
            Some(0),
            "an AF_INET socket should be denied with network off (stderr: {})",
            blocked.stderr.text
        );

        // Network on: the same probe succeeds.
        let on = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy::unconfined());
        let allowed = on.run(run_input(&["python3", "-c", probe])).await.unwrap();
        assert_eq!(allowed.exit_code, Some(0));
    }

    /// seccomp denies `ptrace` (an escape vector) whenever any confinement is
    /// active, regardless of network policy. Skipped when python3 is unavailable.
    #[tokio::test]
    async fn seccomp_blocks_ptrace() {
        if !have("python3") {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        // PTRACE_TRACEME (request 0) returns 0 normally, -1 (EPERM) under seccomp.
        let probe = "import ctypes, sys; libc = ctypes.CDLL('libc.so.6', use_errno=True); \
             sys.exit(0 if libc.ptrace(0, 0, 0, 0) == 0 else 1)";

        // Seccomp active (network off) but filesystem unconfined, so the only
        // thing in play is the always-on ptrace/io_uring denial.
        let denied = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy {
                confine_filesystem: false,
                writable_roots: vec![],
                allow_network: false,
                egress_proxy_uds: None,
                egress_shim_path: None,
                backend: SandboxBackend::InProcess,
            });
        let blocked = denied
            .run(run_input(&["python3", "-c", probe]))
            .await
            .unwrap();
        assert_ne!(
            blocked.exit_code,
            Some(0),
            "ptrace should be denied (stderr: {})",
            blocked.stderr.text
        );

        // Fully unconfined installs no filter: ptrace is allowed.
        let allowed = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy::unconfined());
        let ok = allowed
            .run(run_input(&["python3", "-c", probe]))
            .await
            .unwrap();
        assert_eq!(ok.exit_code, Some(0));
    }

    /// The default-on sandbox (workspace writes + network off) still runs an
    /// ordinary command that reads/executes from the read-only system tree.
    #[tokio::test]
    async fn default_sandbox_still_runs_normal_commands() {
        let workspace = tempfile::tempdir().unwrap();
        let tool = ProcessRunTool::new(workspace.path()).unwrap();
        let out = tool.run(run_input(&["echo", "ok"])).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.text.contains("ok"));
    }

    /// The `memory_mb` cap (RLIMIT_AS, the always-on floor) actually constrains
    /// the child: a large allocation fails under a small cap but succeeds under
    /// a generous one. Skipped when python3 is unavailable.
    #[tokio::test]
    async fn memory_cap_constrains_the_child() {
        if !have("python3") {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let tool = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy::unconfined());
        let alloc = "bytearray(256 * 1024 * 1024)"; // 256 MiB

        let mut tight = run_input(&["python3", "-c", alloc]);
        tight.memory_mb = Some(64);
        let blocked = tool.run(tight).await.unwrap();
        assert_ne!(
            blocked.exit_code,
            Some(0),
            "256 MiB alloc should fail under a 64 MiB cap (stderr: {})",
            blocked.stderr.text
        );

        let mut roomy = run_input(&["python3", "-c", alloc]);
        roomy.memory_mb = Some(1024);
        let allowed = tool.run(roomy).await.unwrap();
        assert_eq!(allowed.exit_code, Some(0));
    }

    /// The bubblewrap backend confines writes to the bound roots: a write inside
    /// the workspace succeeds, one to a path outside is denied (read-only `/`).
    /// Skipped when `bwrap` is unavailable.
    #[tokio::test]
    async fn bubblewrap_confines_writes() {
        if !bwrap_usable() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let writable = workspace.path().canonicalize().unwrap();
        let tool = ProcessRunTool::new(&writable)
            .unwrap()
            .with_sandbox(SandboxPolicy::confined(vec![writable.clone()]).using_bubblewrap());

        let allowed = tool
            .run(run_input(&["touch", "allowed.txt"]))
            .await
            .unwrap();
        assert_eq!(
            allowed.exit_code,
            Some(0),
            "stderr: {}",
            allowed.stderr.text
        );
        assert!(writable.join("allowed.txt").exists());

        let denied_path = outside.path().canonicalize().unwrap().join("denied.txt");
        let denied = tool
            .run(run_input(&["touch", denied_path.to_str().unwrap()]))
            .await
            .unwrap();
        assert_ne!(denied.exit_code, Some(0));
        assert!(!denied_path.exists());
    }

    /// The bubblewrap backend puts the child in its own pid namespace, so it sees
    /// only a handful of processes rather than the host's. Skipped without
    /// `bwrap`/`python3`.
    #[tokio::test]
    async fn bubblewrap_isolates_pid_namespace() {
        if !bwrap_usable() || !have("python3") {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let probe = "import os; print(len([d for d in os.listdir('/proc') if d.isdigit()]))";

        // Unsandboxed: the child sees every host process.
        let plain = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy::unconfined());
        let host = plain
            .run(run_input(&["python3", "-c", probe]))
            .await
            .unwrap();
        let host_count: usize = host.stdout.text.trim().parse().unwrap_or(0);

        // Bubblewrap with `--unshare-pid`: a fresh pid namespace.
        let sandboxed = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy::workspace(workspace.path()).using_bubblewrap());
        let inside = sandboxed
            .run(run_input(&["python3", "-c", probe]))
            .await
            .unwrap();
        let ns_count: usize = inside.stdout.text.trim().parse().unwrap_or(usize::MAX);

        assert!(
            ns_count < host_count,
            "pid namespace should hide host processes: ns={ns_count} host={host_count}"
        );
        assert!(
            ns_count < 10,
            "a fresh pid namespace should show only a few processes, got {ns_count} (stderr: {})",
            inside.stderr.text
        );
    }

    /// In bubblewrap mode the request env reaches the child via `--setenv` (it is
    /// applied to the sandboxed child, not the trusted wrapper).
    #[tokio::test]
    async fn bubblewrap_passes_env_to_child() {
        if !bwrap_usable() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let tool = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy::workspace(workspace.path()).using_bubblewrap());
        let mut req = run_input(&["printenv", "AGENKITTY_BWRAP"]);
        req.env = Some(BTreeMap::from([(
            "AGENKITTY_BWRAP".to_string(),
            "via-setenv".to_string(),
        )]));
        let out = tool.run(req).await.unwrap();
        assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr.text);
        assert_eq!(out.stdout.text.trim(), "via-setenv");
    }

    /// A host secret keeps precedence over a colliding request env key in
    /// bubblewrap mode (the request value must not shadow the secret).
    #[tokio::test]
    async fn bubblewrap_secret_wins_over_request_env() {
        if !bwrap_usable() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let tool = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy::workspace(workspace.path()).using_bubblewrap())
            .with_secret_env("TOKEN", SecretString::new("secret-wins".to_string()));
        let mut req = run_input(&["printenv", "TOKEN"]);
        req.env = Some(BTreeMap::from([(
            "TOKEN".to_string(),
            "request-value".to_string(),
        )]));
        let out = tool.run(req).await.unwrap();
        assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr.text);
        assert!(
            !out.stdout.text.contains("request-value"),
            "request env shadowed the host secret: {:?}",
            out.stdout.text
        );
        // The secret value itself is redacted from captured output.
        assert!(
            out.stdout.text.contains("[redacted]"),
            "expected the (redacted) secret, got: {:?}",
            out.stdout.text
        );
    }

    /// bubblewrap mode still applies the seccomp ptrace/io_uring denial: the
    /// filter rides bwrap's exec into the child. Skipped without `bwrap`/`python3`.
    #[tokio::test]
    async fn bubblewrap_denies_ptrace() {
        if !bwrap_usable() || !have("python3") {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let probe = "import ctypes, sys; libc = ctypes.CDLL('libc.so.6', use_errno=True); \
             sys.exit(0 if libc.ptrace(0, 0, 0, 0) == 0 else 1)";
        let tool = ProcessRunTool::new(workspace.path())
            .unwrap()
            .with_sandbox(SandboxPolicy::workspace(workspace.path()).using_bubblewrap());
        let out = tool
            .run(run_input(&["python3", "-c", probe]))
            .await
            .unwrap();
        assert_ne!(
            out.exit_code,
            Some(0),
            "ptrace should be denied in bubblewrap mode too (stderr: {})",
            out.stderr.text
        );
    }
}
