//! Registration for the MCP tool family.
//!
//! Like `network` (and unlike the default `fs`/`session` set), MCP is **opt-in**:
//! it is registered explicitly via [`register_mcp_tools`] against an
//! [`McpRuntime`], never as part of any default tool set. The agent-facing
//! `mcp.*` verbs and the imported `mcp.<server>.<tool>` adapters land in Phase 2;
//! Phase 0 establishes the registration entry point and the id surface.

use std::collections::HashSet;
use std::sync::Arc;

use pocopine_agenkit::server::AgenkitBuilder;
use pocopine_agenkit_core::AgenkitResult;

use crate::policy::ToolMode;

use super::adapter::McpToolAdapter;
use super::admission::resolve_mode;
use super::call::McpCallTool;
use super::common::{
    MCP_CALL_TOOL_ID, MCP_GET_PROMPT_TOOL_ID, MCP_READ_RESOURCE_TOOL_ID, MCP_SERVERS_TOOL_ID,
    MCP_TOOLS_TOOL_ID, McpToolEntry, namespaced_tool_id, schema_digest,
};
use super::pins::PinStatus;
use super::resources::{McpGetPromptTool, McpReadResourceTool};
use super::runtime::McpRuntime;
use super::verbs::{McpServersTool, McpToolsTool};

/// Register the MCP tool family bound to `runtime`.
///
/// This registers, against the runtime's **already-connected** servers (call
/// [`McpRuntime::connect_all`](super::runtime::McpRuntime::connect_all) first):
/// - each discovered remote tool as a namespaced `mcp.<server>.<tool>`
///   [`McpToolAdapter`] (an erased [`DynTool`](pocopine_agenkit::server::DynTool)
///   registered via [`AgenkitBuilder::tool_dyn`]); and
/// - the agent-facing `mcp.servers` / `mcp.tools` discovery verbs.
///
/// Ids are minted once here (the single point holding the `taken` collision
/// set), and the resulting secret-free index is published onto the runtime so
/// `mcp.tools` reports exactly the registered ids. Like `register_network_tools`
/// this is **opt-in** — never part of any default tool set.
///
/// A tool whose `inputSchema` is not an object schema is skipped (logged), so
/// one malformed tool can't block the rest. Discovery (tool names, schemas,
/// pins) is read once here from the registration-time connection; each adapter
/// then resolves a live connection **per calling principal** at dispatch (C1),
/// so a secret-bearing child is never shared across identities and the redaction
/// set is always the caller's own resolved secrets.
pub fn register_mcp_tools(
    mut builder: AgenkitBuilder,
    runtime: Arc<McpRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    let mut taken: HashSet<String> = HashSet::new();
    let mut index: Vec<McpToolEntry> = Vec::new();
    let mut seen_servers: HashSet<String> = HashSet::new();
    let pins = runtime.pins();
    let observer = runtime.observer();

    for connection in runtime.connections_snapshot() {
        let server = connection.server_name().to_string();
        // The pool can hold one connection per (server, principal) (C1); a
        // server's tool definitions are identity-independent, so discover each
        // server exactly once (the first connection encountered) to keep ids and
        // pins single-minted.
        if !seen_servers.insert(server.clone()) {
            continue;
        }
        // The config drives capability-scoped admission per tool (D5).
        let config = runtime.server(&server).cloned();

        for tool in connection.tools() {
            // Capability-scoped admission (D5), with untrusted annotation hints.
            let mode = match config.as_ref() {
                Some(cfg) => resolve_mode(cfg, &tool.name, tool.annotations.as_ref()),
                None => ToolMode::Deny,
            };

            // Deny tools are pre-filtered: not registered, not indexed, not even
            // surfaced to the model (capability pre-filter, D5/D12).
            if mode == ToolMode::Deny {
                tracing::debug!(
                    target: "pocopine.log",
                    server = server.as_str(),
                    tool = tool.name.as_str(),
                    "mcp tool denied by capability admission; not registered"
                );
                continue;
            }

            // TOFU pin (D8/T2): record the definition hash on first sight. An
            // explicit Allow (operator allowlist / trusted default-Allow) approves
            // it; an Ask tool stays unapproved until the host approves it.
            //
            // C2: a Changed observation is a rug pull (a different definition for an
            // id seen earlier this session) — NEVER auto-approve it, even when
            // allowlisted; require an explicit host re-approval. `is_approved` then
            // re-denies the changed definition on every dispatch mode.
            let hash = tool.pin_hash();
            let status = pins.observe(&server, &tool.name, &hash);
            if mode == ToolMode::Allow {
                if status == PinStatus::Changed {
                    tracing::warn!(
                        target: "pocopine.log",
                        server = server.as_str(),
                        tool = tool.name.as_str(),
                        "allowlisted mcp tool definition changed since a prior sighting; \
                         not auto-approving the rug-pulled definition (re-approval required)"
                    );
                } else {
                    pins.approve(&server, &tool.name, &hash);
                    observer.approval(&server, &tool.name);
                }
            }

            let id = namespaced_tool_id(&server, &tool.name, &taken);
            taken.insert(id.clone());

            index.push(McpToolEntry {
                id: id.clone(),
                server: server.clone(),
                name: tool.name.clone(),
                description: tool.description.clone(),
                schema_digest: schema_digest(&tool.input_schema),
                // Retained for deferred schema loading (D12): surfaced by
                // `mcp.tools` only on demand, never in the default listing.
                input_schema: tool.input_schema.clone(),
            });

            // `DynTool::id` returns `&'static str`; intern the namespaced id
            // (bounded by config size — one leak per imported tool at startup).
            let static_id: &'static str = Box::leak(id.into_boxed_str());
            // The adapter holds the runtime and resolves a live connection
            // **per calling principal** at dispatch (C1) — never the shared
            // registration-time connection.
            match McpToolAdapter::new(
                static_id,
                server.clone(),
                tool,
                runtime.clone(),
                mode,
                pins.clone(),
                observer.clone(),
            ) {
                Ok(adapter) => builder = builder.tool_dyn(Arc::new(adapter)),
                Err(err) => tracing::warn!(
                    target: "pocopine.log",
                    server = server.as_str(),
                    tool = tool.name.as_str(),
                    error = %err,
                    "skipping mcp tool with an unusable input schema"
                ),
            }
        }
    }

    runtime.publish_tool_index(index);

    builder = builder.tool(McpServersTool::new(runtime.clone()));
    builder = builder.tool(McpToolsTool::new(runtime.clone()));
    // Resources & prompts verbs (Phase 5): bounded resource reads + untrusted
    // prompt templates returned as inert data (never auto-executed).
    builder = builder.tool(McpReadResourceTool::new(runtime.clone()));
    builder = builder.tool(McpGetPromptTool::new(runtime.clone()));
    // The `mcp.call` escape hatch (Phase 6): invoke a discovered tool by
    // {server, tool, args}, subject to the same admission + pin gate.
    builder = builder.tool(McpCallTool::new(runtime));
    Ok(builder)
}

/// The static agent-facing `mcp.*` verb ids.
///
/// Imported remote tools (`mcp.<server>.<tool>`) are **dynamic** — their ids are
/// derived per server at runtime by
/// [`namespaced_tool_id`](super::common::namespaced_tool_id) and are deliberately
/// excluded from this set.
pub fn known_mcp_tool_ids() -> [&'static str; 5] {
    [
        MCP_SERVERS_TOOL_ID,
        MCP_TOOLS_TOOL_ID,
        MCP_READ_RESOURCE_TOOL_ID,
        MCP_GET_PROMPT_TOOL_ID,
        MCP_CALL_TOOL_ID,
    ]
}

/// Default MCP tool ids: **empty**. MCP is opt-in, never in a default set.
pub fn default_mcp_tool_ids() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids_are_the_static_verbs_only() {
        let ids = known_mcp_tool_ids();
        assert!(ids.contains(&MCP_SERVERS_TOOL_ID));
        assert!(ids.contains(&MCP_CALL_TOOL_ID));
        // Dynamic imported ids are never listed as "known".
        assert!(!ids.iter().any(|id| id.starts_with("mcp.github.")));
    }

    #[test]
    fn default_ids_are_empty() {
        assert!(default_mcp_tool_ids().is_empty());
    }

    #[test]
    fn register_with_no_servers_registers_only_the_verbs() {
        // With no connected servers there are no adapters to register; the two
        // discovery verbs are still wired and the published index is empty.
        let builder = AgenkitBuilder::default();
        let runtime = Arc::new(McpRuntime::empty());
        assert!(register_mcp_tools(builder, runtime.clone()).is_ok());
        assert!(runtime.tool_index().is_empty());
    }
}
