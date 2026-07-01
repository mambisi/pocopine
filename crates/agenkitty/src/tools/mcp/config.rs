//! Per-server MCP configuration shapes.
//!
//! These types are the in-memory representation a host builds (from
//! `.agenkitty/mcp.json`'s `mcpServers` shape in Phase 7, or programmatically).
//! Secret material is referenced by **handle id** — resolved at the transport
//! edge into per-server env (stdio) or headers (http), never inlined as
//! plaintext into config, args, trace, or session state (D6).
//!
//! Capability provenance is **explicit config + default-deny** (OQ2): a server
//! gets no filesystem, network, or extra env unless its [`CapabilitySet`] grants
//! it. Tool annotations are untrusted hints and never widen this set.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::policy::ToolMode;

/// Connection lifetime for a configured server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    /// Torn down at session end; fork/resume re-derives from config, never
    /// copies a live transport.
    #[default]
    Session,
    /// Persists across sessions for a project/workspace identity.
    Project,
}

/// Transport-specific connection details.
///
/// The variant is tagged (`"type": "stdio" | "http"`) for an unambiguous
/// internal representation; the `mcpServers`-flavoured JSON loader (Phase 7)
/// maps the looser external shape onto this.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportConfig {
    /// Local child process spoken to over stdio, spawned through the process
    /// sandbox (Phase 1). No network unless the [`CapabilitySet`] grants it.
    Stdio {
        /// Executable to spawn (resolved against the sandbox `PATH`).
        command: String,
        /// Arguments passed to the child. Never carry secrets here.
        #[serde(default)]
        args: Vec<String>,
        /// Per-server env: the map **value is a secret-handle id**, resolved at
        /// spawn into the child's environment (never the raw secret).
        #[serde(default)]
        env: BTreeMap<String, String>,
        /// Working directory for the child, relative to the project root.
        #[serde(default)]
        cwd: Option<String>,
    },
    /// Remote Streamable HTTP endpoint reached through the network policy +
    /// SSRF guard + egress proxy (Phase 4).
    Http {
        /// Server endpoint. Must be HTTPS and pass the SSRF guard on every hop.
        url: String,
        /// Per-server headers: the map **value is a secret-handle id**, resolved
        /// at request time (never the raw secret). OAuth is layered in Phase 4.
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

impl McpTransportConfig {
    /// Stable discriminant for diagnostics / `mcp.servers` metadata.
    pub fn kind(&self) -> &'static str {
        match self {
            McpTransportConfig::Stdio { .. } => "stdio",
            McpTransportConfig::Http { .. } => "http",
        }
    }
}

/// Network egress grant for a server. Absence of this on a [`CapabilitySet`]
/// means **no network** (a stdio sandbox denies inet by default, D7).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkCapability {
    /// Destination host allowlist. Empty = deny all (the capability exists but
    /// admits nothing) — never an implicit allow-all.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Explicit deny list, layered over the SSRF guard's built-in
    /// reserved/loopback/metadata blocks.
    #[serde(default)]
    pub deny_hosts: Vec<String>,
}

/// Resource limits applied to a sandboxed stdio child. `None` fields fall back
/// to the process sandbox defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessCapability {
    #[serde(default)]
    pub max_memory_bytes: Option<u64>,
    #[serde(default)]
    pub max_cpu_seconds: Option<u64>,
    #[serde(default)]
    pub max_processes: Option<u64>,
}

/// What a server is permitted to touch, evaluated **before** any dispatch (D5).
/// Explicit config only; default-deny everywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilitySet {
    /// Filesystem roots (relative to the project root) the server may access.
    /// Empty = no filesystem capability.
    #[serde(default)]
    pub fs_roots: Vec<String>,
    /// Network egress grant. `None` = no network capability.
    #[serde(default)]
    pub network: Option<NetworkCapability>,
    /// Additional env var **names** the server may receive (beyond the
    /// secret-handle env). `PATH`/`LD_*`/`DYLD_*` are always scrubbed regardless.
    #[serde(default)]
    pub env_allow: Vec<String>,
    /// Resource limits for a sandboxed stdio child. `None` = sandbox defaults.
    #[serde(default)]
    pub process: Option<ProcessCapability>,
}

impl CapabilitySet {
    /// Whether any network egress is granted.
    pub fn grants_network(&self) -> bool {
        self.network.is_some()
    }
}

/// Default handshake/init timeout (ms) when a config omits one.
pub fn default_init_timeout_ms() -> u64 {
    30_000
}

/// Default per-request timeout (ms) for `tools/call` / `resources/read` /
/// `prompts/get` when a config omits one (Phase 6). A request that exceeds this
/// is cancelled and surfaced as a timeout error rather than hanging the agent.
pub fn default_request_timeout_ms() -> u64 {
    60_000
}

/// A single configured MCP server. `name` is unique within a registry and is
/// sanitized into tool ids; it is never reverse-parsed back out of an id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpServerConfig {
    /// Unique server name within the registry.
    pub name: String,
    /// How to reach the server.
    pub transport: McpTransportConfig,
    /// Capability grants. Default-deny when omitted.
    #[serde(default)]
    pub capabilities: CapabilitySet,
    /// Per-server tool allowlist. A listed tool is promoted to `Allow`; others
    /// stay at `default_mode` (or `Deny` when the server is untrusted).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Default policy mode for this server's imported tools (D5). Defaults to
    /// `Ask`. Represented in JSON Schema as a `"allow" | "ask" | "deny"` string.
    #[serde(default)]
    #[schemars(with = "String")]
    pub default_mode: ToolMode,
    /// Operator trust flag. An untrusted server's un-allowlisted tools default
    /// to `Deny` (the headline capability-admission gate, Phase 3).
    #[serde(default)]
    pub trusted: bool,
    /// Connection lifetime.
    #[serde(default)]
    pub scope: McpScope,
    /// Handshake/init timeout in milliseconds.
    #[serde(default = "default_init_timeout_ms")]
    pub init_timeout_ms: u64,
    /// Per-request timeout in milliseconds (Phase 6): a `tools/call` /
    /// `resources/read` / `prompts/get` that exceeds it is cancelled and
    /// surfaced as a timeout error.
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
}

impl McpScope {
    /// Whether this scope is torn down at session end (vs persisting for the
    /// project). Used by [`McpRuntime`](super::runtime::McpRuntime) teardown.
    pub fn is_session(self) -> bool {
        matches!(self, McpScope::Session)
    }
}

impl McpServerConfig {
    /// Resolve the policy mode for one of this server's tools (D5).
    ///
    /// An allowlisted tool is promoted to `Allow`; otherwise an **untrusted**
    /// server's tools default to `Deny` (the capability-admission gate), and a
    /// trusted server's fall back to its configured `default_mode`. This is the
    /// value the Phase 2 adapter records; the actual `Allow`/`Ask`/`Deny`
    /// admission decision is enforced in Phase 3.
    pub fn resolved_mode(&self, tool_name: &str) -> ToolMode {
        if self.allowed_tools.iter().any(|t| t == tool_name) {
            ToolMode::Allow
        } else if !self.trusted {
            ToolMode::Deny
        } else {
            self.default_mode
        }
    }

    /// A minimal stdio server config (no capabilities, `Ask` default, untrusted,
    /// session-scoped). Capability/allowlist/trust are layered on by the caller.
    pub fn stdio(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransportConfig::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            },
            capabilities: CapabilitySet::default(),
            allowed_tools: Vec::new(),
            default_mode: ToolMode::Ask,
            trusted: false,
            scope: McpScope::Session,
            init_timeout_ms: default_init_timeout_ms(),
            request_timeout_ms: default_request_timeout_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_config_round_trips() {
        let mut env = BTreeMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "secret-handle-1".to_string());
        let config = McpServerConfig {
            name: "github".to_string(),
            transport: McpTransportConfig::Stdio {
                command: "github-mcp".to_string(),
                args: vec!["--stdio".to_string()],
                env,
                cwd: Some("repo".to_string()),
            },
            capabilities: CapabilitySet {
                fs_roots: vec!["src".to_string()],
                network: Some(NetworkCapability {
                    allow_hosts: vec!["api.github.com".to_string()],
                    deny_hosts: vec![],
                }),
                env_allow: vec!["HOME".to_string()],
                process: Some(ProcessCapability {
                    max_memory_bytes: Some(256 * 1024 * 1024),
                    max_cpu_seconds: Some(30),
                    max_processes: Some(8),
                }),
            },
            allowed_tools: vec!["create_issue".to_string()],
            default_mode: ToolMode::Allow,
            trusted: true,
            scope: McpScope::Project,
            init_timeout_ms: 5_000,
            request_timeout_ms: 10_000,
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, config);
        assert_eq!(parsed.transport.kind(), "stdio");
        assert!(parsed.capabilities.grants_network());
    }

    #[test]
    fn http_config_round_trips() {
        let config = McpServerConfig {
            name: "remote".to_string(),
            transport: McpTransportConfig::Http {
                url: "https://mcp.example.com/".to_string(),
                headers: BTreeMap::from([(
                    "Authorization".to_string(),
                    "secret-handle-2".to_string(),
                )]),
            },
            ..McpServerConfig::stdio("remote", "unused")
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, config);
        assert_eq!(parsed.transport.kind(), "http");
    }

    #[test]
    fn defaults_apply_for_a_minimal_config() {
        // Only the required fields present; everything else defaults.
        let json = r#"{
            "name": "minimal",
            "transport": { "type": "stdio", "command": "srv" }
        }"#;
        let parsed: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.default_mode, ToolMode::Ask);
        assert!(!parsed.trusted);
        assert_eq!(parsed.scope, McpScope::Session);
        assert_eq!(parsed.init_timeout_ms, default_init_timeout_ms());
        assert_eq!(parsed.request_timeout_ms, default_request_timeout_ms());
        assert!(parsed.allowed_tools.is_empty());
        assert!(!parsed.capabilities.grants_network());
    }

    #[test]
    fn default_mode_serializes_as_snake_case_string() {
        let config = McpServerConfig::stdio("s", "c");
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["default_mode"], serde_json::json!("ask"));
    }

    #[test]
    fn resolved_mode_gates_by_allowlist_and_trust() {
        // Untrusted server, tool not allowlisted → Deny.
        let untrusted = McpServerConfig::stdio("s", "c");
        assert_eq!(untrusted.resolved_mode("anything"), ToolMode::Deny);

        // Allowlisted tool → Allow, even on an untrusted server.
        let mut allowlisted = McpServerConfig::stdio("s", "c");
        allowlisted.allowed_tools = vec!["create_issue".to_string()];
        assert_eq!(allowlisted.resolved_mode("create_issue"), ToolMode::Allow);
        assert_eq!(allowlisted.resolved_mode("other"), ToolMode::Deny);

        // Trusted server falls back to its default_mode for un-allowlisted tools.
        let mut trusted = McpServerConfig::stdio("s", "c");
        trusted.trusted = true;
        trusted.default_mode = ToolMode::Ask;
        assert_eq!(trusted.resolved_mode("other"), ToolMode::Ask);
    }

    #[test]
    fn scope_session_flag() {
        assert!(McpScope::Session.is_session());
        assert!(!McpScope::Project.is_session());
    }

    #[test]
    fn config_type_derives_json_schema() {
        // The shapes must expose a JSON Schema (used by metadata verbs / docs).
        let schema = schemars::schema_for!(McpServerConfig);
        let json = serde_json::to_value(&schema).unwrap();
        assert!(json.get("properties").is_some());
    }
}
