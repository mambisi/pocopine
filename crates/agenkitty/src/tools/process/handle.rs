use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::sys::signal::Signal;
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture, Principal, SecretString};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::task::JoinHandle;

use super::cgroup::CgroupGuard;
use super::common::{
    CapturedStream, OutputRing, ResourceLimits, canonical_root, check_allowed_prefixes,
    exit_fields, prepare_command, redact, relative_cwd, shutdown_child, signal_group,
    truncate_output,
};
use super::sandbox::SandboxPolicy;
use crate::tools::secrets::{SecretRuntime, validate_secret_env_name};

pub const PROCESS_SPAWN_TOOL_ID: &str = "process.spawn";
pub const PROCESS_READ_TOOL_ID: &str = "process.read";
pub const PROCESS_WRITE_TOOL_ID: &str = "process.write";
pub const PROCESS_KILL_TOOL_ID: &str = "process.kill";

const RING_CAP_BYTES: usize = 262_144;
const READ_MAX_BYTES: usize = 64_000;
const READ_MAX_LINES: usize = 400;
const KILL_GRACE: Duration = Duration::from_secs(2);

type Ring = Arc<Mutex<OutputRing>>;

struct ProcessEntry {
    /// The principal that spawned this process. read/write/kill are rejected
    /// (as `not_found`) for any other caller, so a shared table across sessions
    /// can't be hijacked by guessing a `process-N` id.
    owner: Principal,
    pid: u32,
    command: Vec<String>,
    cwd: String,
    child: Child,
    stdin: Arc<tokio::sync::Mutex<Option<ChildStdin>>>,
    stdout: Ring,
    stderr: Ring,
    readers: [JoinHandle<()>; 2],
    /// Secrets redacted from this process's captured output (shared with the
    /// spawn tool; values stay inside `SecretString`).
    redact_secrets: Arc<BTreeMap<String, SecretString>>,
    // Removed on drop, once the child has been reaped.
    _cgroup: Option<CgroupGuard>,
}

struct TableInner {
    entries: HashMap<String, ProcessEntry>,
    counter: u64,
}

impl TableInner {
    /// Look up an entry the `caller` owns, or `None` (caller can't tell a
    /// missing id from someone else's id).
    fn owned(&self, id: &str, caller: &Principal) -> Option<&ProcessEntry> {
        self.entries.get(id).filter(|entry| &entry.owner == caller)
    }
}

impl Drop for TableInner {
    fn drop(&mut self) {
        // Session teardown: nothing survives. SIGKILL every live group; the
        // direct child also dies via `kill_on_drop` as the entry drops.
        for entry in self.entries.values() {
            let _ = signal_group(entry.pid, Signal::SIGKILL);
        }
    }
}

/// A session-scoped registry of spawned processes shared by the spawn/read/
/// write/kill tools. Dropping the last handle SIGKILLs every remaining group.
#[derive(Clone)]
pub struct ProcessTable {
    inner: Arc<Mutex<TableInner>>,
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTable {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TableInner {
                entries: HashMap::new(),
                counter: 0,
            })),
        }
    }
}

fn spawn_reader<R>(mut reader: R, ring: Ring) -> JoinHandle<()>
where
    R: AsyncReadExt + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut ring) = ring.lock() {
                        ring.append(&chunk[..n]);
                    }
                }
            }
        }
    })
}

fn drain_stream(
    ring: &Ring,
    max_bytes: usize,
    secrets: &BTreeMap<String, SecretString>,
) -> (CapturedStream, u64) {
    let (bytes, dropped) = ring
        .lock()
        .map(|mut ring| ring.drain())
        .unwrap_or_else(|_| (Vec::new(), 0));
    let text = redact(&String::from_utf8_lossy(&bytes), secrets);
    let mut stream = truncate_output(text.as_bytes(), max_bytes, READ_MAX_LINES);
    if dropped > 0 {
        stream.truncated = true;
        stream.omitted_bytes += dropped as usize;
    }
    (stream, dropped)
}

// --------------------------------------------------------------------------
// process.spawn
// --------------------------------------------------------------------------

/// Start a long-running process and return a handle. Output is buffered in a
/// bounded ring; read it with `process.read`, drive stdin with `process.write`,
/// and stop it with `process.kill`.
#[derive(Clone)]
pub struct ProcessSpawnTool {
    root: PathBuf,
    allow_shell: bool,
    allowed_prefixes: Option<Vec<Vec<String>>>,
    default_path: String,
    limits: ResourceLimits,
    sandbox: SandboxPolicy,
    /// `Arc`-shared so each spawned entry references the same `SecretString`s
    /// (zeroized, redacted) for redaction without copying secret material.
    secrets: Arc<BTreeMap<String, SecretString>>,
    secret_runtime: Option<Arc<SecretRuntime>>,
    egress_proxy: Option<SocketAddr>,
    table: ProcessTable,
}

impl ProcessSpawnTool {
    pub fn new(root: impl AsRef<Path>, table: ProcessTable) -> AgenkitResult<Self> {
        let root = canonical_root(root.as_ref())?;
        Ok(Self {
            sandbox: SandboxPolicy::workspace(root.clone()),
            root,
            allow_shell: false,
            allowed_prefixes: None,
            default_path: std::env::var("PATH")
                .unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string()),
            limits: ResourceLimits::default(),
            secrets: Arc::new(BTreeMap::new()),
            secret_runtime: None,
            egress_proxy: None,
            table,
        })
    }

    /// Inject a host-provided secret as an environment variable in the child
    /// (never argv), redacting its value from this process's captured output.
    pub fn with_secret_env(mut self, name: impl Into<String>, value: SecretString) -> Self {
        Arc::make_mut(&mut self.secrets).insert(name.into(), value);
        self
    }

    pub fn with_secret_runtime(mut self, runtime: Arc<SecretRuntime>) -> Self {
        self.secret_runtime = Some(runtime);
        self
    }

    pub fn with_shell(mut self, allow: bool) -> Self {
        self.allow_shell = allow;
        self
    }

    pub fn with_allowed_prefixes(mut self, prefixes: Vec<Vec<String>>) -> Self {
        self.allowed_prefixes = Some(prefixes);
        self
    }

    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Override the kernel sandbox policy (filesystem confinement + network).
    pub fn with_sandbox(mut self, sandbox: SandboxPolicy) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Route the spawned child's outbound HTTP(S) through a host egress proxy.
    pub fn with_egress_proxy(mut self, addr: SocketAddr) -> Self {
        self.egress_proxy = Some(addr);
        self
    }

    pub async fn run(
        &self,
        input: ProcessSpawnInput,
        owner: Principal,
    ) -> AgenkitResult<ProcessSpawnOutput> {
        let shell = input.shell.unwrap_or(false);
        if !shell {
            check_allowed_prefixes(&self.allowed_prefixes, &input.command)?;
        }
        let secrets = Arc::new(self.resolve_secret_env(&input.secret_env, &owner).await?);

        let (std_cmd, resolved_cwd) = prepare_command(
            &self.root,
            &input.command,
            shell,
            self.allow_shell,
            input.cwd.as_deref(),
            input.env.as_ref(),
            &secrets,
            &self.default_path,
            &self.limits,
            &self.sandbox,
            self.egress_proxy,
        )?;

        let mut cmd = tokio::process::Command::from(std_cmd);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let cgroup = CgroupGuard::create(self.limits.memory_bytes, self.limits.max_processes);

        let mut child = cmd.spawn().map_err(|err| {
            AgenkitError::not_found(format!("spawn `{}`: {err}", input.command.join(" ")))
        })?;
        let pid = child
            .id()
            .ok_or_else(|| AgenkitError::internal("spawned process exited before registration"))?;
        if let Some(cgroup) = cgroup.as_ref() {
            cgroup.add_pid(pid);
        }

        let stdin = child.stdin.take();
        let stdout = Arc::new(Mutex::new(OutputRing::new(RING_CAP_BYTES)));
        let stderr = Arc::new(Mutex::new(OutputRing::new(RING_CAP_BYTES)));
        let out_reader = spawn_reader(child.stdout.take().unwrap(), Arc::clone(&stdout));
        let err_reader = spawn_reader(child.stderr.take().unwrap(), Arc::clone(&stderr));

        let cwd = relative_cwd(&self.root, &resolved_cwd);
        let entry = ProcessEntry {
            owner,
            pid,
            command: input.command.clone(),
            cwd: cwd.clone(),
            child,
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            stdout,
            stderr,
            readers: [out_reader, err_reader],
            redact_secrets: Arc::clone(&secrets),
            _cgroup: cgroup,
        };

        let handle_id = {
            let mut table = self.table.inner.lock().expect("process table poisoned");
            table.counter += 1;
            // Append an unguessable token: the owner check only blocks a *different*
            // principal, so without this a concurrent session for the SAME (or the
            // anonymous) principal could reach this handle by guessing `process-N`.
            let id = format!("process-{}-{:016x}", table.counter, rand::random::<u64>());
            table.entries.insert(id.clone(), entry);
            id
        };

        tracing::info!(
            target: "pocopine.log",
            tool = PROCESS_SPAWN_TOOL_ID,
            handle_id = handle_id.as_str(),
            pid,
            program = if shell {
                "/bin/sh"
            } else {
                input.command.first().map(String::as_str).unwrap_or("")
            },
            "process.spawn started"
        );

        Ok(ProcessSpawnOutput {
            handle_id,
            pid,
            command: input.command,
            cwd,
        })
    }

    async fn resolve_secret_env(
        &self,
        requested: &BTreeMap<String, String>,
        owner: &Principal,
    ) -> AgenkitResult<BTreeMap<String, SecretString>> {
        let mut secrets = (*self.secrets).clone();
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
                    owner,
                    PROCESS_SPAWN_TOOL_ID,
                    Some(env_name.as_str()),
                )
                .await?;
            secrets.insert(env_name.clone(), secret);
        }
        Ok(secrets)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ProcessSpawnInput {
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
    #[serde(default)]
    pub shell: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProcessSpawnOutput {
    pub handle_id: String,
    pub pid: u32,
    pub command: Vec<String>,
    pub cwd: String,
}

impl AiTool for ProcessSpawnTool {
    const ID: &'static str = PROCESS_SPAWN_TOOL_ID;
    type Input = ProcessSpawnInput;
    type Output = ProcessSpawnOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            PROCESS_SPAWN_TOOL_ID,
            "Start a long-running process (dev server, watcher) and return a handle. Read its \
             output with process.read, write stdin with process.write, and stop it with \
             process.kill.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let owner = ctx.principal().clone();
        Box::pin(async move { self.run(input, owner).await })
    }
}

// --------------------------------------------------------------------------
// process.read
// --------------------------------------------------------------------------

/// Drain buffered output produced since the last read for a spawned process.
#[derive(Clone)]
pub struct ProcessReadTool {
    table: ProcessTable,
}

impl ProcessReadTool {
    pub fn new(table: ProcessTable) -> Self {
        Self { table }
    }

    pub fn run(
        &self,
        input: ProcessReadInput,
        caller: &Principal,
    ) -> AgenkitResult<ProcessReadOutput> {
        let mut table = self.table.inner.lock().expect("process table poisoned");
        if table.owned(&input.handle_id, caller).is_none() {
            return Err(AgenkitError::not_found(format!(
                "unknown process `{}`",
                input.handle_id
            )));
        }
        let entry = table
            .entries
            .get_mut(&input.handle_id)
            .expect("ownership checked above");

        let (stdout, _) = drain_stream(&entry.stdout, READ_MAX_BYTES, &entry.redact_secrets);
        let (stderr, _) = drain_stream(&entry.stderr, READ_MAX_BYTES, &entry.redact_secrets);

        let status = entry
            .child
            .try_wait()
            .map_err(|err| AgenkitError::internal(format!("poll `{}`: {err}", input.handle_id)))?;
        let running = status.is_none();
        let (exit_code, signal) = match status.as_ref() {
            Some(status) => exit_fields(status),
            None => (None, None),
        };

        let readers_done = entry.readers.iter().all(|handle| handle.is_finished());
        let drained_empty = entry
            .stdout
            .lock()
            .map(|ring| ring.is_empty())
            .unwrap_or(true)
            && entry
                .stderr
                .lock()
                .map(|ring| ring.is_empty())
                .unwrap_or(true);
        if !running && readers_done && drained_empty {
            table.entries.remove(&input.handle_id);
        }

        Ok(ProcessReadOutput {
            handle_id: input.handle_id,
            running,
            exit_code,
            signal,
            stdout,
            stderr,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ProcessReadInput {
    pub handle_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProcessReadOutput {
    pub handle_id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: CapturedStream,
    pub stderr: CapturedStream,
}

impl AiTool for ProcessReadTool {
    const ID: &'static str = PROCESS_READ_TOOL_ID;
    type Input = ProcessReadInput;
    type Output = ProcessReadOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            PROCESS_READ_TOOL_ID,
            "Return output buffered since the last read for a spawned process, plus whether it \
             is still running and its exit code/signal once it ends.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let caller = ctx.principal().clone();
        Box::pin(async move { self.run(input, &caller) })
    }
}

// --------------------------------------------------------------------------
// process.write
// --------------------------------------------------------------------------

/// Write bytes to a spawned process's stdin.
#[derive(Clone)]
pub struct ProcessWriteTool {
    table: ProcessTable,
}

impl ProcessWriteTool {
    pub fn new(table: ProcessTable) -> Self {
        Self { table }
    }

    pub async fn run(
        &self,
        input: ProcessWriteInput,
        caller: &Principal,
    ) -> AgenkitResult<ProcessWriteOutput> {
        let stdin = {
            let table = self.table.inner.lock().expect("process table poisoned");
            let entry = table.owned(&input.handle_id, caller).ok_or_else(|| {
                AgenkitError::not_found(format!("unknown process `{}`", input.handle_id))
            })?;
            Arc::clone(&entry.stdin)
        };

        let mut data = input.data.into_bytes();
        if input.newline.unwrap_or(false) {
            data.push(b'\n');
        }
        let written = data.len();

        let mut guard = stdin.lock().await;
        let pipe = guard.as_mut().ok_or_else(|| {
            AgenkitError::validation(format!("process `{}` has no open stdin", input.handle_id))
        })?;
        pipe.write_all(&data)
            .await
            .map_err(|err| AgenkitError::internal(format!("write stdin: {err}")))?;
        pipe.flush()
            .await
            .map_err(|err| AgenkitError::internal(format!("flush stdin: {err}")))?;

        Ok(ProcessWriteOutput {
            handle_id: input.handle_id,
            bytes_written: written,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ProcessWriteInput {
    pub handle_id: String,
    pub data: String,
    /// Append a trailing newline (submit a line to a prompt).
    #[serde(default)]
    pub newline: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProcessWriteOutput {
    pub handle_id: String,
    pub bytes_written: usize,
}

impl AiTool for ProcessWriteTool {
    const ID: &'static str = PROCESS_WRITE_TOOL_ID;
    type Input = ProcessWriteInput;
    type Output = ProcessWriteOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            PROCESS_WRITE_TOOL_ID,
            "Write bytes to a spawned process's stdin (optionally appending a newline).",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let caller = ctx.principal().clone();
        Box::pin(async move { self.run(input, &caller).await })
    }
}

// --------------------------------------------------------------------------
// process.kill
// --------------------------------------------------------------------------

/// Terminate a spawned process group (SIGTERM, grace, then SIGKILL) and reap it.
#[derive(Clone)]
pub struct ProcessKillTool {
    table: ProcessTable,
}

impl ProcessKillTool {
    pub fn new(table: ProcessTable) -> Self {
        Self { table }
    }

    pub async fn run(
        &self,
        input: ProcessKillInput,
        caller: &Principal,
    ) -> AgenkitResult<ProcessKillOutput> {
        let mut entry = {
            let mut table = self.table.inner.lock().expect("process table poisoned");
            if table.owned(&input.handle_id, caller).is_none() {
                return Err(AgenkitError::not_found(format!(
                    "unknown process `{}`",
                    input.handle_id
                )));
            }
            table
                .entries
                .remove(&input.handle_id)
                .expect("ownership checked above")
        };

        let status = shutdown_child(&mut entry.child, Some(entry.pid), KILL_GRACE)
            .await
            .map_err(|err| AgenkitError::internal(format!("kill `{}`: {err}", input.handle_id)))?;
        let (exit_code, signal) = exit_fields(&status);

        // The child is reaped and its group killed, so the pipes are closing.
        // Wait (bounded) for the reader tasks to append any final bytes before
        // draining, so we don't drop late output (a SIGTERM handler's logs or
        // bytes still buffered in the pipe).
        let [out_reader, err_reader] = entry.readers;
        let _ = tokio::time::timeout(Duration::from_secs(1), async move {
            let _ = out_reader.await;
            let _ = err_reader.await;
        })
        .await;

        let (stdout, _) = drain_stream(&entry.stdout, READ_MAX_BYTES, &entry.redact_secrets);
        let (stderr, _) = drain_stream(&entry.stderr, READ_MAX_BYTES, &entry.redact_secrets);

        tracing::info!(
            target: "pocopine.log",
            tool = PROCESS_KILL_TOOL_ID,
            handle_id = input.handle_id.as_str(),
            exit_code = ?exit_code,
            signal = ?signal,
            "process.kill terminated"
        );

        Ok(ProcessKillOutput {
            handle_id: input.handle_id,
            command: entry.command,
            cwd: entry.cwd,
            exit_code,
            signal,
            stdout,
            stderr,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct ProcessKillInput {
    pub handle_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProcessKillOutput {
    pub handle_id: String,
    pub command: Vec<String>,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: CapturedStream,
    pub stderr: CapturedStream,
}

impl AiTool for ProcessKillTool {
    const ID: &'static str = PROCESS_KILL_TOOL_ID;
    type Input = ProcessKillInput;
    type Output = ProcessKillOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            PROCESS_KILL_TOOL_ID,
            "Terminate a spawned process group (SIGTERM, grace period, then SIGKILL), reap it, \
             and return any final buffered output.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let caller = ctx.principal().clone();
        Box::pin(async move { self.run(input, &caller).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ToolMode;
    use crate::tools::secrets::{
        InMemorySecretResolver, SecretMetadata, SecretRequestInput, SecretRuntime, SecretScope,
    };

    fn spawn_tool() -> (ProcessTable, ProcessSpawnTool) {
        let table = ProcessTable::new();
        let tool = ProcessSpawnTool::new(std::env::temp_dir(), table.clone()).unwrap();
        (table, tool)
    }

    fn owner() -> Principal {
        Principal::anonymous()
    }

    fn spawn_input(command: &[&str]) -> ProcessSpawnInput {
        ProcessSpawnInput {
            command: command.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            env: None,
            secret_env: BTreeMap::new(),
            shell: None,
        }
    }

    #[tokio::test]
    async fn spawn_write_read_kill_roundtrip() {
        let (table, spawn) = spawn_tool();
        let handle = spawn.run(spawn_input(&["cat"]), owner()).await.unwrap();
        assert!(handle.pid > 0);

        let write = ProcessWriteTool::new(table.clone());
        write
            .run(
                ProcessWriteInput {
                    handle_id: handle.handle_id.clone(),
                    data: "ping".to_string(),
                    newline: Some(true),
                },
                &owner(),
            )
            .await
            .unwrap();

        // Let `cat` echo it back.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let read = ProcessReadTool::new(table.clone());
        let out = read
            .run(
                ProcessReadInput {
                    handle_id: handle.handle_id.clone(),
                },
                &owner(),
            )
            .unwrap();
        assert!(out.running);
        assert!(out.stdout.text.contains("ping"));

        let kill = ProcessKillTool::new(table.clone());
        let killed = kill
            .run(
                ProcessKillInput {
                    handle_id: handle.handle_id.clone(),
                },
                &owner(),
            )
            .await
            .unwrap();
        assert!(killed.exit_code.is_some() || killed.signal.is_some());

        // Entry is gone after kill.
        let err = read
            .run(
                ProcessReadInput {
                    handle_id: handle.handle_id,
                },
                &owner(),
            )
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }

    #[tokio::test]
    async fn spawn_accepts_secret_handle_and_redacts_reads() {
        let dir = tempfile::tempdir().unwrap();
        let table = ProcessTable::new();
        let principal = owner();
        let runtime = Arc::new(
            SecretRuntime::new(Arc::new(
                InMemorySecretResolver::new().insert(
                    SecretMetadata::new("api-token", "API token", SecretScope::User)
                        .with_purposes(["server-auth"])
                        .with_target_tools([PROCESS_SPAWN_TOOL_ID])
                        .with_destinations(["API_TOKEN"]),
                    SecretString::new("spawn-secret-value".to_string()),
                ),
            ))
            .with_request_mode(ToolMode::Allow),
        );
        let grant = runtime
            .request(
                SecretRequestInput {
                    secret_ref: "api-token".to_string(),
                    purpose: "server-auth".to_string(),
                    target_tool: PROCESS_SPAWN_TOOL_ID.to_string(),
                    destination: Some("API_TOKEN".to_string()),
                    ttl_ms: None,
                    inheritable: None,
                },
                &principal,
            )
            .await
            .unwrap();

        let spawn = ProcessSpawnTool::new(dir.path(), table.clone())
            .unwrap()
            .with_secret_runtime(runtime);
        let mut req = spawn_input(&["printenv", "API_TOKEN"]);
        req.secret_env
            .insert("API_TOKEN".to_string(), grant.handle_id);
        let handle = spawn.run(req, principal.clone()).await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        let out = ProcessReadTool::new(table)
            .run(
                ProcessReadInput {
                    handle_id: handle.handle_id,
                },
                &principal,
            )
            .unwrap();

        assert_eq!(out.exit_code, Some(0));
        assert!(!out.stdout.text.contains("spawn-secret-value"));
        assert!(out.stdout.text.contains("[redacted]"));
    }

    #[tokio::test]
    async fn read_reports_exit_for_finished_process() {
        let (table, spawn) = spawn_tool();
        let handle = spawn.run(spawn_input(&["true"]), owner()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let read = ProcessReadTool::new(table.clone());
        let out = read
            .run(
                ProcessReadInput {
                    handle_id: handle.handle_id,
                },
                &owner(),
            )
            .unwrap();
        assert!(!out.running);
        assert_eq!(out.exit_code, Some(0));
    }

    #[tokio::test]
    async fn write_to_unknown_handle_is_not_found() {
        let (table, _spawn) = spawn_tool();
        let write = ProcessWriteTool::new(table);
        let err = write
            .run(
                ProcessWriteInput {
                    handle_id: "process-404".to_string(),
                    data: "x".to_string(),
                    newline: None,
                },
                &owner(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }

    #[tokio::test]
    async fn shell_spawn_denied_without_grant() {
        let (_table, spawn) = spawn_tool();
        let mut req = spawn_input(&["echo hi"]);
        req.shell = Some(true);
        let err = spawn.run(req, owner()).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    /// A different principal cannot read or kill another caller's process, even
    /// knowing its (predictable) handle id.
    #[tokio::test]
    async fn handles_are_isolated_per_owner() {
        use pocopine_agenkit::server::AuthUser;

        let (table, spawn) = spawn_tool();
        let alice = Principal::from_user(AuthUser::new("alice"));
        let bob = Principal::from_user(AuthUser::new("bob"));

        let handle = spawn.run(spawn_input(&["cat"]), alice).await.unwrap();

        // Bob guesses the id but is denied (indistinguishable from missing).
        let read = ProcessReadTool::new(table.clone());
        let read_err = read
            .run(
                ProcessReadInput {
                    handle_id: handle.handle_id.clone(),
                },
                &bob,
            )
            .unwrap_err();
        assert_eq!(read_err.kind(), "not_found");

        let kill = ProcessKillTool::new(table.clone());
        let kill_err = kill
            .run(
                ProcessKillInput {
                    handle_id: handle.handle_id.clone(),
                },
                &bob,
            )
            .await
            .unwrap_err();
        assert_eq!(kill_err.kind(), "not_found");

        // The owner can still reach it.
        let ok = kill
            .run(
                ProcessKillInput {
                    handle_id: handle.handle_id,
                },
                &owner_alice(),
            )
            .await;
        assert!(ok.is_ok());
    }

    fn owner_alice() -> Principal {
        use pocopine_agenkit::server::AuthUser;
        Principal::from_user(AuthUser::new("alice"))
    }
}
