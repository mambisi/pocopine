# MCP Tool

Let an AI agent use **external [Model Context Protocol](https://modelcontextprotocol.io)
servers** — but routed through agenkitty's policy, capability, secret-handle,
sandbox, and tracing layers rather than bypassing them. agenkitty is the **MCP
host/client**: it connects to host-configured servers over **stdio** (a sandboxed
child) or **Streamable HTTP** (an SSRF-guarded client), discovers their tools, and
surfaces each as a first-class agenkitty tool.

The trust boundary is **capability-scoped admission over `(server, tool,
capability)`** plus a **TOFU pin** on every tool description. A remote server is
treated as untrusted: its tool outputs are data (never instructions), its
descriptions are pinned and re-approved on change, and it gets no filesystem,
network, or env it was not explicitly granted.

---

## Tool surface

| Tool id | Purpose | Side effect | Default policy |
|---|---|---|---|
| `mcp.servers` | List configured/connected servers + status (no secrets) | read-only | opt-in |
| `mcp.tools` | List discovered remote tools (id, server, pinned description; schema on demand) | read-only | opt-in |
| `mcp.read_resource` | Read an MCP resource by uri → bounded, untrusted content | read-only | opt-in |
| `mcp.get_prompt` | Fetch a prompt template → **inert, untrusted data** (never executed) | read-only | opt-in |
| `mcp.call` | Escape hatch: invoke a discovered tool by `{server, tool, arguments}` | side-effecting | opt-in |
| `mcp.<server>.<tool>` | Each discovered remote tool, namespaced + admitted | side-effecting | per capability admission |

MCP is **opt-in**: nothing is in any default tool set. Register it explicitly via
`register_mcp_tools(builder, runtime)` against an `McpRuntime`; with no configured
servers it registers only the verbs.

---

## Configuration

Two equivalent entry points: a project file or the builder API.

### `.agenkitty/mcp.json` (the `mcpServers` shape)

The familiar `mcpServers` object — an existing config from another harness pastes
in — with agenkitty policy extensions:

```json
{
  "mcpServers": {
    "github": {
      "command": "github-mcp-server",
      "args": ["stdio"],
      "env": { "GITHUB_TOKEN": "secret-handle-id" },
      "trusted": true,
      "allowedTools": ["search_issues"]
    },
    "remote": {
      "type": "http",
      "url": "https://mcp.example.com/",
      "headers": { "Authorization": "secret-handle-id" },
      "capabilities": { "network": { "allow_hosts": ["mcp.example.com"] } }
    }
  }
}
```

```rust
// From the project file (sandbox root = dir):
let runtime = McpRuntime::from_config_dir(project_dir)?;
// Or from an in-memory document:
let runtime = McpRuntime::from_mcp_servers_json(json)?;

let runtime = Arc::new(runtime.with_secret_runtime(secret_runtime));
runtime.connect_all().await;                       // spawn + handshake + discover
let builder = register_mcp_tools(builder, runtime)?; // register adapters + verbs
```

- A server is **stdio** when it has a `command`, **http** when it has a `url`
  (`type` disambiguates). A missing file is not an error (MCP is opt-in).
- **Secrets stay handles (D6).** `env` / `headers` map *values* are secret-handle
  ids resolved at the transport edge — never plaintext credentials in config,
  args, trace, or session state.
- **Defaults are secure:** an entry without `trusted`/`allowedTools`/`defaultMode`
  is untrusted, `Ask`, default-deny on every capability.

---

## Security model (the gate list)

- **Sandboxed stdio (D7).** A stdio server is spawned through the process
  sandbox: `env_clear` + explicit `PATH` (dropping `PATH`/`LD_*`/`DYLD_*`),
  `setrlimit`, Landlock/seccomp, **no network** unless its capability grants it.
- **Capability-scoped admission (D5).** Every `(server, tool)` resolves to
  `Allow`/`Ask`/`Deny` from **explicit config + default-deny**. Un-allowlisted
  tools on an untrusted server are `Deny` and **pre-filtered** (never registered,
  never indexed). Untrusted annotation hints may only *tighten*, never loosen.
- **Pinned descriptions / TOFU (D8).** Each tool's `name+description+inputSchema`
  is hashed and pinned; ANSI/control chars are stripped first. An `Ask` tool runs
  only while its exact pinned hash is approved. A `tools/list_changed` swap that
  changes the hash **clears the approval** (rug-pull guard) — never silently
  adopted.
- **Secret handles, not strings (D6).** Resolved per-server; injected as stdio
  env or HTTP headers at the transport edge for the calling principal, and
  scrubbed back out of every tool output.
- **Untrusted outputs (D9).** Tool results, resource contents, and prompt
  templates are bounded (byte caps + `elided` flag), ANSI-stripped, and
  secret-redacted before they cross to the model. A fetched prompt is returned as
  inert data under `messages` (`untrusted: true`) — never promoted to the
  instruction channel or auto-executed.
- **HTTP: SSRF + RFC 8707 + no token passthrough (D10).** The Streamable-HTTP
  client wraps the network tool's SSRF guard (resolve → validate → pin on every
  hop, no proxy/auto-redirect), is HTTPS + destination-allowlist only, and
  audience-validates any presented JWT. The only `Authorization` ever sent is the
  per-server secret handle.
- **Sampling + elicitation denied (D11).** The client handler refuses
  `sampling/createMessage` (method-not-found) and declines elicitation;
  `roots/list` returns only the configured roots (empty for remote servers).
- **Supply-chain (T5).** The spawn `command` comes **only** from the operator's
  config file (never a server response), is validated (non-empty, single-line,
  no control chars), and is sandboxed. See *Deferred* for the binary-hash pin.

---

## Eventing & audit (Phase 7)

Every connect and every call emits a `FrameworkEvent`
(`ToolStarted`/`ToolCompleted`/`ToolFailed`/`ToolBlocked`) and an in-memory,
session-scoped **audit** row; structured `tracing` goes to `target: "pocopine.log"`
(RFC-069). The audit trail covers **connect / call / approval / hash-change**.

```rust
let events = runtime.events();  // Vec<FrameworkEvent>
let audit  = runtime.audit();   // Vec<McpAuditEntry>  (kind, outcome, server, tool, detail)
```

Redaction is structural: outputs are already secret-scrubbed by the adapter
before any event sees them, and the only free-form text recorded (an error
message) is heuristically redacted + byte-bounded. **No event or audit row
carries a secret value, header, env value, argument payload, or server body.**

---

## Deferred (out of v1)

- **agenkitty as an MCP *server*** (exposing agenkitty tools over MCP).
- **MCP-proxy per-client consent** (the confused-deputy control for re-exposing
  a server through agenkitty to multiple downstream clients).
- **Tasks** (the experimental long-running-call extension).
- **Sampling / elicitation enablement** behind a human-in-the-loop capability.
- **Binary-content command pin** — hash + verify the resolved executable before
  spawn (today the command is operator-pinned by provenance + validated).
- **Interactive OAuth 2.1 flow** — the metadata-discovery / browser-redirect /
  DCR / refresh executor (the *shapes + checks* ship; the static secret-handle
  `Authorization` header is the working remote-auth path).
- **Project-persistent pin store** (v1 is in-memory + session-scoped).
