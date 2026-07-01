//! `McpToolAdapter`: one remote MCP tool exposed as an agenkitty tool.
//!
//! The adapter is the bridge from a discovered remote tool to the registry. It
//! is **dynamic** — its id and input schema are known only at runtime — so it
//! implements the erased [`DynTool`] directly (rather than the typed [`AiTool`])
//! and is registered through [`AgenkitBuilder::tool_dyn`]. The id is interned to
//! `'static` at registration (bounded by config size), since [`DynTool::id`]
//! returns `&'static str`.
//!
//! Behaviour:
//! - **descriptor** — input schema is the server's `inputSchema` *verbatim*
//!   (rejected at construction if it is not an object schema / is null) and the
//!   tool is marked `.side_effecting()`.
//! - **admission** (D5/D8/T2/C2) — enforced at call time: `Deny` refuses, and
//!   **every** dispatchable mode (`Allow` *and* `Ask`) requires an *approved
//!   current pin*. `Allow` is auto-approved at registration, but a rug-pulled or
//!   `tools/list_changed`-invalidated definition clears that approval, so even an
//!   allowlisted tool fails closed until re-approval.
//! - **per-principal connection** (C1) — the live connection is resolved for the
//!   *calling* principal at dispatch (via the runtime's per-`(server, principal)`
//!   pool) and re-discovered first if the server announced a `tools/list_changed`
//!   (H1); a secret-bearing child is never shared across caller identities.
//! - **call** — proxies `tools/call`; a `CallToolResult` with `isError: true`
//!   is surfaced as a **recoverable** result (its content/`structuredContent`
//!   pass through with `isError` set), **not** an `Err` (SEP-1303); and the
//!   result is **bounded** (per-text byte cap + an `elided` flag) and
//!   **redacted** with the resolved principal's own secret values before it
//!   crosses to the model or the trace (D9).
//!
//! [`AgenkitBuilder::tool_dyn`]: pocopine_agenkit::server::AgenkitBuilder::tool_dyn
//! [`AiTool`]: pocopine_agenkit::server::AiTool

use std::sync::Arc;

use pocopine_agenkit::server::{AiToolContext, BoxFuture, DynTool, Principal, SecretString};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, SchemaRef, ToolDescriptor};
use serde_json::{Map, Value, json};

use crate::policy::ToolMode;

use super::admission::build_ask_prompt;
use super::client::McpToolDef;
use super::common::{bound_text, redact_secrets};
use super::observer::McpObserver;
use super::pins::PinStore;
use super::runtime::McpRuntime;

/// Per-text-block byte cap applied to bounded tool output.
const MAX_TEXT_CONTENT_BYTES: usize = 8 * 1024;
/// Cap on the serialized size of `structuredContent`; over this it is dropped
/// (replaced with an `elided` marker) so a server cannot blow the context.
const MAX_STRUCTURED_BYTES: usize = 16 * 1024;
/// Cap on the number of content blocks surfaced.
const MAX_CONTENT_BLOCKS: usize = 64;

/// One remote MCP tool, adapted to the erased [`DynTool`] contract.
pub struct McpToolAdapter {
    /// The interned `mcp.<server>.<tool>` id.
    id: &'static str,
    /// The owning server name (diagnostics / trace).
    server: String,
    /// The server-local tool name proxied to `tools/call`.
    tool_name: String,
    /// The pinned (bounded) description shown to the model.
    description: Option<String>,
    /// The server's `inputSchema`, validated as an object schema.
    input_schema: Map<String, Value>,
    /// Resolved capability-admission mode (D5), enforced at call time:
    /// `Deny` refuses; `Ask` requires an approved pin; `Allow` proceeds.
    mode: ToolMode,
    /// The pinned definition hash (D8) checked against the pin store on **every**
    /// dispatch (`Allow` and `Ask` alike, C2): a tool dispatches only while this
    /// exact hash is approved, so a rug-pulled (re-hashed) definition — even an
    /// allowlisted one — is disabled pending re-approval (T2).
    pin_hash: String,
    /// Shared session pin store (TOFU + approvals), owned by the runtime.
    pins: Arc<PinStore>,
    /// The host runtime. The live connection is resolved **per call** for the
    /// calling principal (C1) — a secret-bearing child is never shared across
    /// identities — and re-discovered first if the server announced a
    /// `tools/list_changed` (T2/H1).
    runtime: Arc<McpRuntime>,
    /// Session eventing + audit collector (Phase 7): a `ToolStarted`/`Completed`/
    /// `Failed`/`Blocked` event + audit row per call. Shared with the runtime.
    observer: Arc<McpObserver>,
}

impl McpToolAdapter {
    /// Build an adapter for one discovered tool.
    ///
    /// `id` must already be the interned, namespaced
    /// (`mcp.<server>.<tool>`) id. Fails with a `validation` error if the tool's
    /// `inputSchema` is not an object schema (a null/non-object schema can't be
    /// surfaced as a function-calling `parameters` shape).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &'static str,
        server: impl Into<String>,
        tool: &McpToolDef,
        runtime: Arc<McpRuntime>,
        mode: ToolMode,
        pins: Arc<PinStore>,
        observer: Arc<McpObserver>,
    ) -> AgenkitResult<Self> {
        validate_object_schema(id, &tool.input_schema)?;
        Ok(Self {
            id,
            server: server.into(),
            tool_name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
            mode,
            pin_hash: tool.pin_hash(),
            pins,
            runtime,
            observer,
        })
    }

    /// The resolved policy mode for this tool (admission is enforced in Phase 3).
    pub fn mode(&self) -> ToolMode {
        self.mode
    }

    /// The owning server name.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Proxy a `tools/call` and shape the result into bounded, redacted JSON.
    ///
    /// This is the real work behind [`DynTool::call_json`]; it is split out so
    /// tests can drive it without constructing an [`AiToolContext`].
    ///
    /// Brackets the call with framework events + audit (Phase 7): a `ToolStarted`
    /// up front, then `ToolCompleted` on success, `ToolBlocked` on a policy
    /// refusal (`tool_policy`), or `ToolFailed` on any other error.
    pub async fn invoke(&self, args: Value, principal: &Principal) -> AgenkitResult<Value> {
        self.observer
            .call_started(self.id, &self.server, &self.tool_name);
        match self.invoke_inner(args, principal).await {
            Ok(value) => {
                self.observer
                    .call_completed(self.id, &self.server, &self.tool_name);
                Ok(value)
            }
            Err(err) => {
                if err.kind() == "tool_policy" {
                    self.observer.call_blocked(
                        self.id,
                        &self.server,
                        &self.tool_name,
                        &err.to_string(),
                    );
                } else {
                    self.observer
                        .call_failed(self.id, &self.server, &self.tool_name, &err);
                }
                Err(err)
            }
        }
    }

    async fn invoke_inner(&self, args: Value, principal: &Principal) -> AgenkitResult<Value> {
        // Resolve the connection **for this principal** (C1) and process any
        // pending `tools/list_changed` (rediscover → re-pin → revoke-missing)
        // *before* the admission/pin decision (RH1 TOCTOU): a notified mutation
        // must re-deny the call, never be invoked under a stale approval. A prior
        // ordering checked the pin first, so a list-changed that arrived and was
        // rediscovered here (revoking the approval) still reached `call_tool`.
        // `ready_connection_as` serializes the rediscovery per `(server,
        // principal)` and clears the dirty flag only after the re-pin completes
        // (RH2), so `check_admission` below observes the post-rediscovery pin
        // store. A secret-bearing child is never shared across identities; the
        // redaction set is this principal's own resolved secrets.
        let connection = self
            .runtime
            .ready_connection_as(&self.server, principal)
            .await?;

        // Capability-scoped admission (D5) + pin/rug-pull guard (D8/T2), decided
        // *after* rediscovery and immediately before dispatch.
        self.check_admission()?;

        let arguments = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                return Err(AgenkitError::validation(format!(
                    "mcp tool `{}` arguments must be a JSON object, got {}",
                    self.id,
                    json_type_name(&other),
                )));
            }
        };

        let result = connection.call_tool(&self.tool_name, arguments).await?;

        Ok(shape_call_result(result, &connection.redaction_secrets()))
    }

    /// Enforce the resolved admission decision (D5) plus the pin guard (D8/T2).
    /// Every dispatchable mode requires an **approved current pin** (C2):
    ///
    /// - `Deny` → refused outright (`tool_policy`); the model never reaches it.
    /// - `Allow` / `Ask` → dispatched only while this tool's pinned hash is
    ///   **approved** in the store. `Allow` is auto-approved at registration (the
    ///   operator allowlist), but a rug-pulled (re-hashed) or list-changed
    ///   definition clears that approval, so even an allowlisted tool fails closed
    ///   until re-approval. `Ask` additionally requires an explicit host approval.
    fn check_admission(&self) -> AgenkitResult<()> {
        match self.mode {
            ToolMode::Deny => Err(AgenkitError::tool_policy(format!(
                "mcp tool `{}` is denied by policy (un-allowlisted tool on an untrusted server)",
                self.id
            ))),
            ToolMode::Allow | ToolMode::Ask => {
                if self
                    .pins
                    .is_approved(&self.server, &self.tool_name, &self.pin_hash)
                {
                    Ok(())
                } else {
                    // An unapproved `Ask` tool: surface the T1-complete approval
                    // request (command/origin + pinned description + params, built
                    // from the live server config) for a host to act on, then fail
                    // closed (the interactive approve/deny loop is deferred, M4).
                    if self.mode == ToolMode::Ask
                        && let Some(config) = self.runtime.server(&self.server)
                    {
                        let prompt = build_ask_prompt(
                            config,
                            &self.tool_name,
                            self.description.as_deref(),
                            &self.input_schema,
                        );
                        self.observer.approval_requested(
                            &self.server,
                            &self.tool_name,
                            &prompt.summary(),
                        );
                    }
                    Err(AgenkitError::tool_policy(format!(
                        "mcp tool `{}` is not currently approved: its pinned definition was \
                         never approved or has changed since approval (re-approval required)",
                        self.id
                    )))
                }
            }
        }
    }
}

/// Bound + redact a [`CallToolResult`](rmcp::model::CallToolResult) into the
/// JSON the model/trace sees (D9): per-text byte caps + an `elided` flag,
/// `structuredContent` dropped over its cap, content blocks capped, and every
/// string scrubbed of each resolved secret value. `isError: true` is surfaced
/// as a normal output carrying the flag (a recoverable tool-execution error,
/// SEP-1303), never an `Err`.
///
/// Shared by [`McpToolAdapter`] (statically-registered tools) and the
/// `mcp.call` escape hatch so both shape results identically.
pub(crate) fn shape_call_result(
    result: rmcp::model::CallToolResult,
    secrets: &[SecretString],
) -> Value {
    let mut elided = false;

    let mut content = Vec::with_capacity(result.content.len().min(MAX_CONTENT_BLOCKS));
    for (index, block) in result.content.into_iter().enumerate() {
        if index >= MAX_CONTENT_BLOCKS {
            elided = true;
            break;
        }
        let mut value = serde_json::to_value(&block).unwrap_or(Value::Null);
        bound_and_redact(&mut value, secrets, &mut elided);
        content.push(value);
    }

    let structured = result.structured_content.map(|mut sc| {
        bound_and_redact(&mut sc, secrets, &mut elided);
        let encoded = serde_json::to_string(&sc).unwrap_or_default();
        if encoded.len() > MAX_STRUCTURED_BYTES {
            elided = true;
            json!({ "elided": true })
        } else {
            sc
        }
    });

    let is_error = result.is_error.unwrap_or(false);

    let mut out = Map::new();
    out.insert("content".to_string(), Value::Array(content));
    if let Some(structured) = structured {
        out.insert("structuredContent".to_string(), structured);
    }
    out.insert("isError".to_string(), Value::Bool(is_error));
    out.insert("elided".to_string(), Value::Bool(elided));
    Value::Object(out)
}

impl DynTool for McpToolAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn descriptor(&self) -> ToolDescriptor {
        let description = self.description.clone().unwrap_or_else(|| {
            format!(
                "MCP tool `{}` on server `{}` (no description provided)",
                self.tool_name, self.server
            )
        });
        // The server's inputSchema is attached verbatim so the model sees the
        // real argument shape; the typed-derive path can't express a per-server
        // runtime schema, which is why this is a hand-rolled `DynTool`.
        let input = SchemaRef {
            name: self.tool_name.clone(),
            json_schema: Some(Value::Object(self.input_schema.clone())),
        };
        ToolDescriptor::new(self.id, description)
            .side_effecting()
            .with_input(input)
    }

    fn call_json<'a>(
        &'a self,
        args: Value,
        ctx: AiToolContext,
    ) -> BoxFuture<'a, AgenkitResult<Value>> {
        // The call principal drives per-principal connection resolution (C1): a
        // secret-bearing child is never shared across caller identities.
        let principal = ctx.principal().clone();
        Box::pin(async move { self.invoke(args, &principal).await })
    }
}

/// Reject a tool whose `inputSchema` is not an object schema. The container is
/// already a JSON object (rmcp types it as `JsonObject`), so the check is on the
/// declared `"type"`: if present it must be `"object"` (a `"string"`/array/...
/// schema can't be a tool's argument shape). An absent `type` is accepted — the
/// MCP spec implies object for a tool input schema.
fn validate_object_schema(id: &str, schema: &Map<String, Value>) -> AgenkitResult<()> {
    match schema.get("type") {
        None => Ok(()),
        Some(Value::String(ty)) if ty == "object" => Ok(()),
        Some(other) => Err(AgenkitError::validation(format!(
            "mcp tool `{id}` inputSchema must be an object schema, got type {other}"
        ))),
    }
}

/// Recursively bound long strings and scrub secret values in a JSON value.
fn bound_and_redact(value: &mut Value, secrets: &[SecretString], elided: &mut bool) {
    match value {
        Value::String(text) => {
            redact_secrets(text, secrets);
            if text.len() > MAX_TEXT_CONTENT_BYTES {
                *text = bound_text(text, MAX_TEXT_CONTENT_BYTES);
                *elided = true;
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                bound_and_redact(item, secrets, elided);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                bound_and_redact(item, secrets, elided);
            }
        }
        _ => {}
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{CallToolResult, ContentBlock};

    // A real `McpConnection` owns a spawned child, so the connection-bound paths
    // (`new`/`invoke`) are exercised in the integration test `tests/mcp_tool.rs`.
    // These unit tests cover the pure validation/bounding/redaction helpers and
    // the `CallToolResult` → JSON content mapping `shape_result` relies on.

    #[test]
    fn object_schema_is_accepted() {
        let schema = json!({ "type": "object", "properties": {} });
        let Value::Object(map) = schema else {
            unreachable!()
        };
        assert!(validate_object_schema("mcp.s.t", &map).is_ok());
    }

    #[test]
    fn schema_without_type_is_accepted() {
        let schema = json!({ "properties": {} });
        let Value::Object(map) = schema else {
            unreachable!()
        };
        assert!(validate_object_schema("mcp.s.t", &map).is_ok());
    }

    #[test]
    fn non_object_schema_is_rejected() {
        let schema = json!({ "type": "string" });
        let Value::Object(map) = schema else {
            unreachable!()
        };
        let err = validate_object_schema("mcp.s.t", &map).unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[test]
    fn redaction_scrubs_secret_values() {
        let mut text = "token=supersecret-value rest".to_string();
        redact_secrets(&mut text, &[SecretString::new("supersecret-value")]);
        assert_eq!(text, "token=[REDACTED] rest");
        assert!(!text.contains("supersecret-value"));
    }

    #[test]
    fn redaction_ignores_empty_secrets() {
        let mut text = "unchanged".to_string();
        redact_secrets(&mut text, &[SecretString::new("")]);
        assert_eq!(text, "unchanged");
    }

    #[test]
    fn bound_and_redact_walks_nested_values() {
        let secret = SecretString::new("leaked");
        let mut value = json!({
            "outer": ["leaked here", { "inner": "also leaked" }],
        });
        let mut elided = false;
        bound_and_redact(&mut value, std::slice::from_ref(&secret), &mut elided);
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains("leaked"));
        assert!(!elided, "short strings should not be elided");
    }

    #[test]
    fn long_text_is_bounded_and_flagged_elided() {
        let big = "A".repeat(MAX_TEXT_CONTENT_BYTES * 2);
        let mut value = Value::String(big);
        let mut elided = false;
        bound_and_redact(&mut value, &[], &mut elided);
        let Value::String(text) = &value else {
            unreachable!()
        };
        assert!(text.len() <= MAX_TEXT_CONTENT_BYTES);
        assert!(elided);
    }

    #[test]
    fn text_content_block_maps_to_typed_json() {
        // The content-block → JSON mapping `shape_result` performs per block.
        // `CallToolResult` is `#[non_exhaustive]`; use its public constructor.
        let result = CallToolResult::error(vec![ContentBlock::text("hello")]);
        let mut elided = false;
        let mut content = Vec::new();
        for block in result.content {
            let mut v = serde_json::to_value(&block).unwrap();
            bound_and_redact(&mut v, &[], &mut elided);
            content.push(v);
        }
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hello");
        assert_eq!(result.is_error, Some(true));
    }
}
