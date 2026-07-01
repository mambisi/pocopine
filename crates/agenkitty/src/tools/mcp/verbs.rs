//! The read-only agent-facing discovery verbs: `mcp.servers` and `mcp.tools`.
//!
//! Both are `ReadOnly` (default-`Allow`) and expose **only model-safe metadata**
//! — never a server's command/args/env, url/headers, or any secret material.
//! `mcp.servers` reports configured servers + live connection status;
//! `mcp.tools` reports the discovered tools (the registered `mcp.<server>.<tool>`
//! id, the owning server, the pinned/bounded description, and a schema digest)
//! so the agent can discover capabilities on demand without bloating the system
//! prompt (D12).
//!
//! The tool list is read from the index
//! [published](super::runtime::McpRuntime::publish_tool_index) by
//! [`register_mcp_tools`](super::registry::register_mcp_tools), so the ids the
//! verb reports are exactly the ids the adapters registered under.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{MCP_SERVERS_TOOL_ID, MCP_TOOLS_TOOL_ID};
use super::runtime::McpRuntime;

// --- mcp.servers ----------------------------------------------------------

/// Input for `mcp.servers` (no arguments).
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct McpServersInput {}

/// Safe per-server status. Deliberately omits transport details (command/env,
/// url/headers) and every secret reference.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct McpServerStatus {
    /// Server name.
    pub name: String,
    /// Transport kind (`"stdio"` | `"http"`).
    pub transport: String,
    /// Connection lifetime (`"session"` | `"project"`).
    pub scope: String,
    /// Operator trust flag.
    pub trusted: bool,
    /// Whether a live connection is currently pooled.
    pub connected: bool,
    /// Number of discovered tools (0 when not connected).
    pub tool_count: usize,
    /// Negotiated protocol version, when connected.
    pub protocol_version: Option<String>,
}

/// Output for `mcp.servers`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct McpServersOutput {
    /// Configured servers and their status.
    pub servers: Vec<McpServerStatus>,
}

/// `mcp.servers` — list configured/connected servers + status (no secrets).
pub struct McpServersTool {
    runtime: Arc<McpRuntime>,
}

impl McpServersTool {
    /// Bind the verb to a runtime.
    pub fn new(runtime: Arc<McpRuntime>) -> Self {
        Self { runtime }
    }

    /// Produce the server status list (no `AiToolContext` needed).
    pub fn run(&self) -> McpServersOutput {
        let servers = self
            .runtime
            .servers()
            .iter()
            .map(|config| {
                let connection = self.runtime.connection(&config.name);
                let connected = connection.is_some();
                let tool_count = connection.as_ref().map(|c| c.tools().len()).unwrap_or(0);
                let protocol_version = connection
                    .as_ref()
                    .map(|c| c.protocol_version().to_string());
                McpServerStatus {
                    name: config.name.clone(),
                    transport: config.transport.kind().to_string(),
                    scope: scope_label(config.scope),
                    trusted: config.trusted,
                    connected,
                    tool_count,
                    protocol_version,
                }
            })
            .collect();
        McpServersOutput { servers }
    }
}

impl AiTool for McpServersTool {
    const ID: &'static str = MCP_SERVERS_TOOL_ID;
    type Input = McpServersInput;
    type Output = McpServersOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            Self::ID,
            "List configured MCP servers and their connection status (no secrets).",
        )
    }

    fn call(
        &self,
        _input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { Ok(self.run()) })
    }
}

// --- mcp.tools -------------------------------------------------------------

/// Input for `mcp.tools`: optional server-name filter + on-demand schema.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct McpToolsInput {
    /// When set, list only this server's tools.
    #[serde(default)]
    pub server: Option<String>,
    /// Deferred schema loading (D12): when `true`, include each tool's full
    /// input JSON Schema in the listing. Defaults to `false` so the routine
    /// listing stays lean (ids + pinned descriptions + a digest) and never
    /// pre-bloats the model's context with every server's schemas — the model
    /// fetches a specific tool's schema on demand right before it calls it.
    #[serde(default)]
    pub include_schema: bool,
}

/// Safe per-tool metadata (no schema-version bridge, no secrets).
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct McpToolInfo {
    /// The registered `mcp.<server>.<tool>` id.
    pub id: String,
    /// The owning server name.
    pub server: String,
    /// The server-local tool name.
    pub name: String,
    /// The pinned, bounded description.
    pub description: Option<String>,
    /// A stable digest over the tool's input schema (rug-pull reference, D8).
    pub schema_digest: String,
    /// The full input JSON Schema — present only when the caller requested it
    /// (`include_schema: true`); omitted from the lean default listing (D12).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
}

/// Output for `mcp.tools`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct McpToolsOutput {
    /// Discovered tools (optionally filtered by server).
    pub tools: Vec<McpToolInfo>,
}

/// `mcp.tools` — list discovered remote tools (id, server, pinned description,
/// schema digest) for on-demand capability discovery (D12).
pub struct McpToolsTool {
    runtime: Arc<McpRuntime>,
}

impl McpToolsTool {
    /// Bind the verb to a runtime.
    pub fn new(runtime: Arc<McpRuntime>) -> Self {
        Self { runtime }
    }

    /// Produce the tool list (no `AiToolContext` needed). The full input schema
    /// is included only when `input.include_schema` is set (deferred loading,
    /// D12).
    pub fn run(&self, input: McpToolsInput) -> McpToolsOutput {
        let tools = self
            .runtime
            .tool_index()
            .into_iter()
            .filter(|entry| match &input.server {
                Some(server) => &entry.server == server,
                None => true,
            })
            .map(|entry| McpToolInfo {
                id: entry.id,
                server: entry.server,
                name: entry.name,
                description: entry.description,
                schema_digest: entry.schema_digest,
                input_schema: input
                    .include_schema
                    .then_some(Value::Object(entry.input_schema)),
            })
            .collect();
        McpToolsOutput { tools }
    }
}

impl AiTool for McpToolsTool {
    const ID: &'static str = MCP_TOOLS_TOOL_ID;
    type Input = McpToolsInput;
    type Output = McpToolsOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            Self::ID,
            "List discovered MCP tools (id, server, pinned description, schema digest).",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { Ok(self.run(input)) })
    }
}

fn scope_label(scope: super::config::McpScope) -> String {
    match scope {
        super::config::McpScope::Session => "session".to_string(),
        super::config::McpScope::Project => "project".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::common::McpToolEntry;

    #[test]
    fn servers_lists_configured_without_secrets() {
        let mut config = crate::tools::mcp::McpServerConfig::stdio("github", "github-mcp");
        config.trusted = true;
        let runtime = Arc::new(crate::tools::mcp::McpRuntime::new(vec![config]).unwrap());
        let out = McpServersTool::new(runtime).run();
        assert_eq!(out.servers.len(), 1);
        let server = &out.servers[0];
        assert_eq!(server.name, "github");
        assert_eq!(server.transport, "stdio");
        assert_eq!(server.scope, "session");
        assert!(server.trusted);
        assert!(!server.connected);
        assert_eq!(server.tool_count, 0);
        // No secret/command/env field can exist on the status struct: serialize
        // and confirm the secret-handle value never appears.
        let json = serde_json::to_string(&out).unwrap();
        assert!(!json.contains("secret-handle"));
        assert!(!json.contains("github-mcp"));
    }

    #[test]
    fn tools_reads_published_index_and_filters() {
        let runtime = Arc::new(crate::tools::mcp::McpRuntime::empty());
        runtime.publish_tool_index(vec![
            McpToolEntry {
                id: "mcp.github.create_issue".to_string(),
                server: "github".to_string(),
                name: "create_issue".to_string(),
                description: Some("Open an issue".to_string()),
                schema_digest: "abcd".to_string(),
                input_schema: serde_json::from_value(serde_json::json!({
                    "type": "object",
                    "properties": { "title": { "type": "string" } }
                }))
                .unwrap(),
            },
            McpToolEntry {
                id: "mcp.other.do".to_string(),
                server: "other".to_string(),
                name: "do".to_string(),
                description: None,
                schema_digest: "ef01".to_string(),
                input_schema: serde_json::Map::new(),
            },
        ]);

        let all = McpToolsTool::new(runtime.clone()).run(McpToolsInput::default());
        assert_eq!(all.tools.len(), 2);
        // Deferred loading (D12): the lean default listing carries no full schema.
        assert!(all.tools.iter().all(|t| t.input_schema.is_none()));
        let lean_json = serde_json::to_string(&all).unwrap();
        assert!(
            !lean_json.contains("inputSchema") && !lean_json.contains("input_schema"),
            "the default listing must not pre-bloat with schemas: {lean_json}"
        );

        let filtered = McpToolsTool::new(runtime.clone()).run(McpToolsInput {
            server: Some("github".to_string()),
            include_schema: false,
        });
        assert_eq!(filtered.tools.len(), 1);
        assert_eq!(filtered.tools[0].id, "mcp.github.create_issue");
        assert_eq!(filtered.tools[0].schema_digest, "abcd");
        // No secret material in the listing.
        let json = serde_json::to_string(&filtered).unwrap();
        assert!(!json.contains("secret"));

        // On demand (D12): the full input schema is included only when requested.
        let with_schema = McpToolsTool::new(runtime).run(McpToolsInput {
            server: Some("github".to_string()),
            include_schema: true,
        });
        let schema = with_schema.tools[0]
            .input_schema
            .as_ref()
            .expect("schema present on demand");
        assert_eq!(schema["properties"]["title"]["type"], "string");
    }
}
