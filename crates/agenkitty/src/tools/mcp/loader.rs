//! Loading MCP server configuration from `.agenkitty/mcp.json` (Phase 7).
//!
//! The on-disk format uses the familiar **`mcpServers`** object shape (the same
//! shape other harnesses use, so an existing config pastes in) mapped onto the
//! internal [`McpServerConfig`]. A server entry is **stdio** when it carries a
//! `command` and **http** when it carries a `url`; the optional `type`
//! (`"stdio"` / `"http"` / `"sse"` / `"streamable-http"`) disambiguates and is
//! validated against the fields present.
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "github": {
//!       "command": "github-mcp-server",
//!       "args": ["stdio"],
//!       "env": { "GITHUB_TOKEN": "secret-handle-id" },
//!       "trusted": true,
//!       "allowedTools": ["search_issues"]
//!     },
//!     "remote": {
//!       "type": "http",
//!       "url": "https://mcp.example.com/",
//!       "headers": { "Authorization": "secret-handle-id" },
//!       "capabilities": { "network": { "allow_hosts": ["mcp.example.com"] } }
//!     }
//!   }
//! }
//! ```
//!
//! **Secrets stay handles (D6).** The `env` / `headers` map *values* are
//! secret-handle ids resolved at the transport edge — never plaintext credentials
//! in the file.
//!
//! **Supply-chain pinning (T5).** The spawn `command` is taken **only** from this
//! operator-controlled file (never derived from a server response), is validated
//! to be a non-empty, single-line, control-char-free string, and is run through
//! the process sandbox (env-scrubbed `PATH`, `setrlimit`, Landlock/seccomp). The
//! command is therefore operator-pinned by provenance; a stronger binary-content
//! hash pin (verify the resolved executable's digest before spawn) is a deferred
//! follow-up.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};

use crate::policy::ToolMode;

use super::config::{
    CapabilitySet, McpScope, McpServerConfig, McpTransportConfig, default_init_timeout_ms,
    default_request_timeout_ms,
};

/// Conventional project-relative path to the MCP config file.
pub const MCP_CONFIG_PATH: &str = ".agenkitty/mcp.json";

/// The parsed `.agenkitty/mcp.json` file (the `mcpServers` shape).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct McpServersFile {
    /// Configured servers keyed by name. Order is not significant; the runtime
    /// rejects duplicate names.
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: BTreeMap<String, McpServerEntry>,
}

/// One server entry in the `mcpServers` map. The loose external shape: a `command`
/// makes it stdio, a `url` makes it http. agenkitty-specific policy fields
/// (`capabilities`, `allowedTools`, `defaultMode`, `trusted`, `scope`, timeouts)
/// are camelCase extensions; an entry that omits them gets the secure defaults
/// (untrusted, `Ask`, default-deny capabilities).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerEntry {
    /// Explicit transport discriminant (`stdio` / `http` / `sse` /
    /// `streamable-http`). Optional; inferred from `command`/`url` when absent.
    #[serde(default, rename = "type")]
    pub transport_type: Option<String>,

    // --- stdio fields ---
    /// Executable to spawn (stdio). Operator-pinned (T5).
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments passed to the child. Never carry secrets here.
    #[serde(default)]
    pub args: Vec<String>,
    /// Per-server env: each value is a **secret-handle id** (D6).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory for the child, relative to the project root.
    #[serde(default)]
    pub cwd: Option<String>,

    // --- http fields ---
    /// Remote endpoint (http). Must be HTTPS and pass the SSRF guard.
    #[serde(default)]
    pub url: Option<String>,
    /// Per-server headers: each value is a **secret-handle id** (D6).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,

    // --- agenkitty policy extensions ---
    /// Capability grants (default-deny when omitted). Uses the [`CapabilitySet`]
    /// snake_case field names (`fs_roots`, `allow_hosts`, …).
    #[serde(default)]
    pub capabilities: CapabilitySet,
    /// Per-server tool allowlist (promoted to `Allow`).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Default policy mode (`allow` / `ask` / `deny`). Defaults to `Ask`.
    #[serde(default)]
    pub default_mode: Option<ToolMode>,
    /// Operator trust flag (an untrusted server's un-allowlisted tools → `Deny`).
    #[serde(default)]
    pub trusted: bool,
    /// Connection lifetime (`session` / `project`).
    #[serde(default)]
    pub scope: Option<McpScope>,
    /// Handshake/init timeout (ms).
    #[serde(default)]
    pub init_timeout_ms: Option<u64>,
    /// Per-request timeout (ms).
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
}

impl McpServersFile {
    /// Parse the `mcpServers` JSON document.
    pub fn from_json_str(json: &str) -> AgenkitResult<Self> {
        serde_json::from_str(json)
            .map_err(|err| AgenkitError::config(format!("invalid .agenkitty/mcp.json: {err}")))
    }

    /// Convert the parsed file into the internal [`McpServerConfig`] set,
    /// validating each entry. The map key becomes the server `name` (sorted, so
    /// the resulting order is deterministic).
    pub fn into_configs(self) -> AgenkitResult<Vec<McpServerConfig>> {
        let mut configs = Vec::with_capacity(self.mcp_servers.len());
        for (name, entry) in self.mcp_servers {
            configs.push(entry.into_config(name)?);
        }
        Ok(configs)
    }
}

impl McpServerEntry {
    fn into_config(self, name: String) -> AgenkitResult<McpServerConfig> {
        let transport = self.resolve_transport(&name)?;
        Ok(McpServerConfig {
            name,
            transport,
            capabilities: self.capabilities,
            allowed_tools: self.allowed_tools,
            default_mode: self.default_mode.unwrap_or_default(),
            trusted: self.trusted,
            scope: self.scope.unwrap_or_default(),
            init_timeout_ms: self.init_timeout_ms.unwrap_or_else(default_init_timeout_ms),
            request_timeout_ms: self
                .request_timeout_ms
                .unwrap_or_else(default_request_timeout_ms),
        })
    }

    /// Decide stdio vs http from the explicit `type` (if any) and the fields
    /// present, then validate the chosen transport.
    fn resolve_transport(&self, name: &str) -> AgenkitResult<McpTransportConfig> {
        let kind = match self.transport_type.as_deref() {
            Some("stdio") => TransportKind::Stdio,
            // SSE is the legacy streaming flavour of the same HTTP endpoint; both
            // map onto our single Streamable-HTTP client.
            Some("http") | Some("sse") | Some("streamable-http") | Some("streamable_http") => {
                TransportKind::Http
            }
            Some(other) => {
                return Err(AgenkitError::config(format!(
                    "mcp server `{name}`: unknown transport type `{other}` \
                     (expected `stdio`, `http`, or `sse`)"
                )));
            }
            None => match (self.command.is_some(), self.url.is_some()) {
                (true, false) => TransportKind::Stdio,
                (false, true) => TransportKind::Http,
                (true, true) => {
                    return Err(AgenkitError::config(format!(
                        "mcp server `{name}`: declares both `command` (stdio) and `url` (http); \
                         pick one"
                    )));
                }
                (false, false) => {
                    return Err(AgenkitError::config(format!(
                        "mcp server `{name}`: must declare either `command` (stdio) or `url` (http)"
                    )));
                }
            },
        };

        match kind {
            TransportKind::Stdio => {
                let command = self.command.clone().ok_or_else(|| {
                    AgenkitError::config(format!(
                        "mcp server `{name}`: transport type `stdio` requires a `command`"
                    ))
                })?;
                // Supply-chain pin (T5): the command is operator-controlled and
                // must be a single, well-formed token — never a shell line or a
                // value smuggled in via control characters.
                validate_spawn_command(name, &command)?;
                Ok(McpTransportConfig::Stdio {
                    command,
                    args: self.args.clone(),
                    env: self.env.clone(),
                    cwd: self.cwd.clone(),
                })
            }
            TransportKind::Http => {
                let url = self.url.clone().ok_or_else(|| {
                    AgenkitError::config(format!(
                        "mcp server `{name}`: transport type `http` requires a `url`"
                    ))
                })?;
                Ok(McpTransportConfig::Http {
                    url,
                    headers: self.headers.clone(),
                })
            }
        }
    }
}

enum TransportKind {
    Stdio,
    Http,
}

/// Validate an operator-supplied spawn command (T5). The command must be a
/// non-empty, single-line, control-char-free string. The actual binary
/// resolution + sandboxing happens in the stdio transport; this rejects obvious
/// injection / malformed input at config-load time.
fn validate_spawn_command(name: &str, command: &str) -> AgenkitResult<()> {
    if command.trim().is_empty() {
        return Err(AgenkitError::config(format!(
            "mcp server `{name}`: `command` must not be empty"
        )));
    }
    if command.chars().any(|ch| ch.is_control()) {
        return Err(AgenkitError::config(format!(
            "mcp server `{name}`: `command` must not contain control characters"
        )));
    }
    Ok(())
}

/// Load + parse `.agenkitty/mcp.json` under `dir`, returning the configured
/// servers. A **missing** file is not an error — it yields an empty set (MCP is
/// opt-in). A present-but-malformed file is a `config` error.
pub fn load_mcp_config(dir: &Path) -> AgenkitResult<Vec<McpServerConfig>> {
    let path = dir.join(MCP_CONFIG_PATH);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| AgenkitError::config(format!("reading {}: {err}", path.display())))?;
    McpServersFile::from_json_str(&raw)?.into_configs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_and_http_entries() {
        let json = r#"{
            "mcpServers": {
                "github": {
                    "command": "github-mcp",
                    "args": ["stdio"],
                    "env": { "GITHUB_TOKEN": "handle-1" },
                    "trusted": true,
                    "allowedTools": ["search"],
                    "scope": "project"
                },
                "remote": {
                    "type": "http",
                    "url": "https://mcp.example.com/",
                    "headers": { "Authorization": "handle-2" }
                }
            }
        }"#;
        let configs = McpServersFile::from_json_str(json)
            .unwrap()
            .into_configs()
            .unwrap();
        // Sorted by name: github, remote.
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].name, "github");
        assert_eq!(configs[0].transport.kind(), "stdio");
        assert!(configs[0].trusted);
        assert_eq!(configs[0].scope, McpScope::Project);
        assert_eq!(configs[1].name, "remote");
        assert_eq!(configs[1].transport.kind(), "http");
    }

    #[test]
    fn infers_transport_from_fields() {
        let stdio = McpServerEntry {
            command: Some("srv".to_string()),
            ..McpServerEntry::default()
        };
        assert_eq!(
            stdio.into_config("s".to_string()).unwrap().transport.kind(),
            "stdio"
        );
        let http = McpServerEntry {
            url: Some("https://x/".to_string()),
            ..McpServerEntry::default()
        };
        assert_eq!(
            http.into_config("h".to_string()).unwrap().transport.kind(),
            "http"
        );
    }

    #[test]
    fn rejects_entry_with_neither_command_nor_url() {
        let entry = McpServerEntry::default();
        let err = entry.into_config("bad".to_string()).unwrap_err();
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn rejects_entry_with_both_command_and_url() {
        let entry = McpServerEntry {
            command: Some("srv".to_string()),
            url: Some("https://x/".to_string()),
            ..McpServerEntry::default()
        };
        let err = entry.into_config("bad".to_string()).unwrap_err();
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn rejects_command_with_control_chars() {
        let entry = McpServerEntry {
            command: Some("srv\nrm -rf /".to_string()),
            ..McpServerEntry::default()
        };
        let err = entry.into_config("bad".to_string()).unwrap_err();
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn defaults_are_secure_when_omitted() {
        let entry = McpServerEntry {
            command: Some("srv".to_string()),
            ..McpServerEntry::default()
        };
        let config = entry.into_config("s".to_string()).unwrap();
        assert_eq!(config.default_mode, ToolMode::Ask);
        assert!(!config.trusted);
        assert_eq!(config.scope, McpScope::Session);
        assert!(config.allowed_tools.is_empty());
        assert!(!config.capabilities.grants_network());
    }

    #[test]
    fn round_trips_through_config_set() {
        let json = r#"{ "mcpServers": { "s": { "command": "c", "defaultMode": "allow" } } }"#;
        let configs = load_mcp_config_from_str(json).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].default_mode, ToolMode::Allow);
    }

    #[test]
    fn empty_file_yields_no_servers() {
        let configs = load_mcp_config_from_str("{}").unwrap();
        assert!(configs.is_empty());
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let configs = load_mcp_config(dir.path()).unwrap();
        assert!(configs.is_empty());
    }

    #[test]
    fn loads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".agenkitty")).unwrap();
        std::fs::write(
            dir.path().join(MCP_CONFIG_PATH),
            r#"{ "mcpServers": { "s": { "command": "srv" } } }"#,
        )
        .unwrap();
        let configs = load_mcp_config(dir.path()).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "s");
    }

    #[test]
    fn rejects_unknown_field() {
        // `deny_unknown_fields` catches typos / smuggled keys.
        let json = r#"{ "mcpServers": { "s": { "command": "c", "evil": true } } }"#;
        let err = load_mcp_config_from_str(json).unwrap_err();
        assert_eq!(err.kind(), "config");
    }

    /// Test helper: parse + convert in one step.
    fn load_mcp_config_from_str(json: &str) -> AgenkitResult<Vec<McpServerConfig>> {
        McpServersFile::from_json_str(json)?.into_configs()
    }
}
