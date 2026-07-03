# Process Tool

Run local commands for an AI agent **safely**: under a confined working
directory, a scrubbed environment, a wall-clock timeout, bounded output, and a
real OS sandbox. This is the highest-risk tool family (it executes arbitrary
code on the host), so the design treats the **OS sandbox as the trust boundary**
and everything else as defence in depth.

A line-by-line technical walkthrough of how every piece works lives in the
Obsidian note **`agenkitty process tool internals`**.

---

## Tool surface

| Tool id | Purpose | Side effect | Default policy |
|---|---|---|---|
| `process.run` | Run a command to completion, return exit code/signal + bounded stdout/stderr | side-effecting | `Ask` |
| `process.spawn` | Start a long-running process (dev server, REPL), return a handle | side-effecting | `Ask` |
| `process.read` | Drain output buffered since the last read for a spawned process | read-only | `Allow` |
| `process.write` | Write to a spawned process's stdin | side-effecting | `Ask` |
| `process.kill` | Terminate a spawned process group and reap it | side-effecting | `Ask` |

Unix only. Kernel sandboxing (Landlock/seccomp/cgroups) is Linux; on other unix
targets those layers are a no-op and the portable protections still apply.

---

## Quick start

Register the family against a workspace root (secure defaults):

```rust
use agenkitty::tools::register_process_tools;

let agenkit = register_process_tools(Agenkit::builder().provider(p), &root)?
    .build()?;
```

Or configure it declaratively:

```rust
use agenkitty::tools::{register_process_tools_with, ProcessToolConfig};

let config = ProcessToolConfig {
    allow_shell: false,                 // /bin/sh -c is a separate, stronger grant
    allowed_prefixes: None,             // Some([vec!["cargo".into()]]) to allowlist argv
    allow_network: false,               // deny INET sockets in the sandbox
    extra_writable_roots: vec![],       // beyond the workspace root + /tmp
    use_bubblewrap: false,              // namespace backend instead of in-process LSM
};
let agenkit = register_process_tools_with(builder, &root, &config)?.build()?;
```

`process.run` is the only id in `default_process_tool_ids()`; the long-running
handle tools are opt-in because they hold state across calls.

A model invokes `process.run` like any other tool:

```json
{ "command": ["cargo", "test", "--", "--nocapture"], "timeout_ms": 120000 }
```

and gets structured output back:

```json
{
  "command": ["cargo", "test", "--", "--nocapture"],
  "cwd": "./",
  "exit_code": 0, "signal": null, "timed_out": false,
  "stdout": { "text": "…", "truncated": false, "omitted_bytes": 0, "omitted_lines": 0 },
  "stderr": { "text": "…", "truncated": false, "omitted_bytes": 0, "omitted_lines": 0 },
  "duration_ms": 8421
}
```

---

## Execution backends

The sandbox is selected per-policy via `SandboxBackend` (default: in-process).

### In-process (default) — `SandboxBackend::InProcess`

Confines the child *as a normal host process* using kernel features applied in
the post-fork `pre_exec` hook:

- **Landlock** confines filesystem **writes** to `writable_roots`; the rest of
  `/` stays readable + executable (so binaries/libraries load) but not writable.
- **seccomp-bpf** denies the `ptrace` / `io_uring` escape vectors **always**, and
  denies `AF_INET`/`AF_INET6` socket creation when network is off (`AF_UNIX` /
  `AF_NETLINK` still work).

Zero startup cost, edits the workspace in place. Shares the host kernel.

### Bubblewrap — `SandboxBackend::Bubblewrap`

Wraps the command in `bwrap` with **mount / pid / net / user namespaces** — the
child gets its own process table (can't see or signal host processes) and a
fresh network namespace. Stronger isolation at the cost of spawning `bwrap`.

```rust
let policy = SandboxPolicy::workspace(&root).using_bubblewrap();
let tool   = ProcessRunTool::new(&root)?.with_sandbox(policy);
// or: ProcessToolConfig { use_bubblewrap: true, .. }
```

Maps the policy to `--ro-bind / /` + `--bind <writable_root>`, `--dev`/`--proc`,
`--unshare-pid`, `--unshare-net` (network off), `--die-with-parent`,
`--new-session`, `--chdir`. Landlock is skipped (the namespaces do filesystem
isolation) but the **seccomp filter still applies** — it's inherited across
`bwrap`'s `exec` into the child, so ptrace/io_uring stay denied. `bwrap` is
launched by **trusted absolute path** (never resolved via the request's `PATH`),
and the request environment is passed to the sandboxed child via `--setenv`, not
to the wrapper (so it can't influence it, e.g. via `LD_PRELOAD`).

---

## What's enforced

| Concern | Mechanism |
|---|---|
| argv-only execution | `command` is an argv array; shell is a separate `with_shell` grant. The OS sandbox — not argv parsing — is the boundary. |
| Working directory | project-relative, validated, confined to the workspace root. |
| Environment | `env_clear()` + explicit `PATH` + only caller-allowlisted vars. The agent's env (API keys, `DATABASE_URL`, …) is never inherited. |
| Resource limits | `setrlimit` (`RLIMIT_AS`/`CPU`/`FSIZE`/`NPROC`, `CORE`=0) + best-effort cgroup v2 (`memory.max`/`pids.max`). |
| Timeout | wall-clock; on expiry the **whole process group** is killed (a backgrounded child can't outlive the limit). |
| Output | deterministic head+tail truncation with byte **and** line caps; full stdout/stderr never unbounded. |
| Lifecycle | own process group via `setsid`-equivalent; `SIGTERM → grace → SIGKILL → waitpid`; session-scoped SIGKILL sweep on drop — no orphans, zombies, or FD leaks. |
| Filesystem | Landlock (in-process) or `--ro-bind`/`--bind` (bwrap). |
| Network | seccomp INET denial (in-process) or `--unshare-net` (bwrap). |
| Escape vectors | seccomp denies `ptrace` + `io_uring`; `PR_SET_DUMPABLE=0` blocks `/proc/<pid>/mem` reads. |
| Secrets | injected as env (never argv), redacted from output, kept inside `SecretString`. |
| Multi-session | handles are owner-scoped — one principal can't read/kill another's process by guessing a `process-N` id. |

**Secure by default:** `ProcessRunTool::new(root)` confines writes to the
workspace + `/tmp`, denies network, denies shell, and emits a trace event per
call — no extra configuration required.

---

## Secrets

Host-provided secrets go in through the tool, never the model's JSON:

```rust
let tool = ProcessRunTool::new(&root)?
    .with_secret_env("DATABASE_URL", SecretString::new(url));
```

The value is set in the child's **environment** (not argv, which is
world-readable via `/proc/<pid>/cmdline`), **redacted** (`[redacted]`) out of the
captured stdout/stderr if the command echoes it, and kept inside `SecretString`
(zeroized, redacted-on-debug). In bubblewrap mode a secret keeps precedence over
a colliding request env key and never lands on `bwrap`'s argv.

---

## Observability

Each verb emits one `tracing` event (`target: "pocopine.log"`, per RFC-069) with
the **program name + outcome only** — exit code, signal, `timed_out`, duration,
handle id. Never arguments or output (which can carry sensitive data); in shell
mode the program is logged as `/bin/sh`, not the script.

---

## Configuration surface

Builder methods on `ProcessRunTool` / `ProcessSpawnTool`:

- `with_shell(bool)` — permit `/bin/sh -c`.
- `with_allowed_prefixes(Vec<Vec<String>>)` — argv-prefix allowlist.
- `with_sandbox(SandboxPolicy)` — `workspace` / `confined` / `unconfined`, `.with_network()`, `.using_bubblewrap()`.
- `with_limits(ResourceLimits)` — memory/cpu/fsize/pids.
- `with_secret_env(name, SecretString)` — secret env injection.
- `with_path(String)` — the `PATH` set in the scrubbed child env.

`ProcessToolConfig` is the declarative form mapped through
`register_process_tools_with` (the target for a project config block).

---

## Module layout

| File | Responsibility |
|---|---|
| `mod.rs` | module wiring + public re-exports |
| `common.rs` | `prepare_command` (the heart): argv/shell, env scrub, process group, `pre_exec` rlimits + `PR_SET_DUMPABLE`, backend branch; output truncation + ring buffer + redaction + signal/shutdown helpers |
| `run.rs` | `process.run` — capture loop (timeout bounded on pipe-EOF), shared capped buffers |
| `handle.rs` | `process.spawn`/`read`/`write`/`kill` + the session-scoped `ProcessTable` (owner-scoped, group-kill on drop) |
| `sandbox.rs` | `SandboxPolicy`/`SandboxBackend`, the Landlock+seccomp `SandboxInstaller`, and `bwrap` argument building |
| `cgroup.rs` | best-effort cgroup v2 (`memory.max`/`pids.max`) with rlimit fallback |
| `registry.rs` | `register_process_tools[_with]`, `ProcessToolConfig`, tool-id helpers |

---

## Testing

- `cargo test -p agenkitty tools::process` — unit tests.
- `cargo test -p agenkitty --test process_tool` — agent-path examples (a mock
  model issues real tool calls).

Sandbox tests are gated on capability: they skip when `python3` is absent, and
the bubblewrap tests probe a real minimal namespace sandbox (`bwrap_usable()`)
rather than just `bwrap --version`, so they skip where unprivileged user
namespaces are disabled — printing a `SKIP <test>: …` line so a skip is visible
in `--nocapture` output rather than a silent vacuous pass.

The **Tier-2 egress lockdown is validated end-to-end** by
`--test process_tool::egress_shim_is_the_only_route_out_of_the_namespace` on a
bwrap-capable host: a `--unshare-net` child's only egress is the bound proxy
UDS via the in-namespace shim (direct egress dead, proxied egress round-trips).

---

## Further reading

- **`agenkitty process tool internals`** (Obsidian) — the deep technical
  walkthrough of how every piece works.
- **`Linux Syscalls & Tooling`** (Obsidian) — beginner-friendly background on
  the kernel features used here (fork/exec, signals, rlimits, cgroups, Landlock,
  seccomp, ptrace/io_uring, namespaces, /proc, sockets, errno).
