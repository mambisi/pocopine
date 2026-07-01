//! MCP (Model Context Protocol) host/client tools.
//!
//! The README captures the public contract. agenkitty's v1 role is **MCP
//! host/client**: it connects to host-configured servers over stdio (a
//! sandboxed child) or Streamable HTTP (an SSRF-guarded client), discovers their
//! tools, and surfaces them as first-class `AiTool`s — every remote call routed
//! through agenkitty's policy, capability, secret-handle, sandbox, and tracing
//! layers rather than bypassing them.
//!
//! Module layout (mirrors `network/`): [`common`] holds namespacing, error
//! mapping, and the verb ids; [`config`] the per-server config shapes;
//! [`runtime`] the host runtime; [`client`] the connection wrapper; [`adapter`]
//! the remote-tool-to-`AiTool` bridge; [`handler`] the server-to-client request
//! handler; [`transport`] the injected stdio/http transports; and [`registry`]
//! the opt-in registration.
//!
//! **cfg gating:** the whole module is `#[cfg(unix)]` (see `tools/mod.rs`),
//! mirroring `process`, because the stdio transport reuses the unix-only process
//! sandbox. The HTTP transport is cross-platform in principle; re-splitting it
//! out of the unix gate is deferred until a non-unix host needs remote-only MCP.

pub mod adapter;
pub mod admission;
pub mod call;
pub mod client;
pub mod common;
pub mod config;
pub mod handler;
pub mod loader;
pub mod oauth;
pub mod observer;
pub mod pins;
pub mod registry;
pub mod resources;
pub mod runtime;
pub mod transport;
pub mod verbs;

pub use adapter::McpToolAdapter;
pub use admission::{
    AdmissionDecision, ApprovalTransport, AskPrompt, admit, build_ask_prompt, resolve_mode,
};
pub use call::{McpCallInput, McpCallOutput, McpCallTool};
pub use client::{McpConnection, McpToolDef, SUPPORTED_PROTOCOL_VERSIONS};
pub use common::{
    MAX_MCP_TOOL_ID_LEN, MAX_SCHEMA_BYTES, MCP_CALL_TOOL_ID, MCP_GET_PROMPT_TOOL_ID, MCP_NAMESPACE,
    MCP_READ_RESOURCE_TOOL_ID, MCP_SERVERS_TOOL_ID, MCP_TOOLS_TOOL_ID, McpToolEntry, bound_text,
    map_service_error, namespaced_tool_id, pin_hash, sanitize_description, sanitize_id_segment,
    sanitize_schema, schema_digest,
};
pub use config::{
    CapabilitySet, McpScope, McpServerConfig, McpTransportConfig, NetworkCapability,
    ProcessCapability, default_init_timeout_ms, default_request_timeout_ms,
};
pub use handler::McpClientHandler;
pub use loader::{MCP_CONFIG_PATH, McpServerEntry, McpServersFile, load_mcp_config};
pub use oauth::{
    OAuthError, authorization_request_params, resource_indicator, token_request_params,
    validate_audience, validate_bearer_credential,
};
pub use observer::{McpAuditEntry, McpAuditKind, McpAuditOutcome, McpObserver};
pub use pins::{PinStatus, PinStore};
pub use registry::{default_mcp_tool_ids, known_mcp_tool_ids, register_mcp_tools};
pub use resources::{
    ALLOWED_RESOURCE_SCHEMES, McpGetPromptInput, McpGetPromptOutput, McpGetPromptTool,
    McpPromptMessage, McpReadResourceInput, McpReadResourceOutput, McpReadResourceTool,
    McpResourceContent,
};
pub use runtime::McpRuntime;
pub use transport::http::{GuardedHttpClient, HttpTransportError, server_net_policy};
pub use verbs::{
    McpServerStatus, McpServersInput, McpServersOutput, McpServersTool, McpToolInfo, McpToolsInput,
    McpToolsOutput, McpToolsTool,
};
