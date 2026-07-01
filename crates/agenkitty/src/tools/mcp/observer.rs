//! MCP eventing + audit (Phase 7).
//!
//! Every MCP **connect** and every remote **call** emits a [`FrameworkEvent`]
//! (`ToolStarted`/`ToolCompleted`/`ToolFailed`/`ToolBlocked`, per RFC-069) and an
//! [`McpAuditEntry`]. The observer additionally records an audit row for every
//! description **approval** and every pin **hash change** (a `tools/list_changed`
//! rug-pull), so the full call/approval/hash-change/connect trail is reconstructible.
//!
//! **Redaction is structural.** The observer never sees a resolved secret value:
//! tool *outputs* are already bounded + secret-scrubbed by the adapter
//! ([`shape_call_result`](super::adapter::shape_call_result)) before they reach
//! the model or any event, and the only free-form text the observer records is an
//! [`AgenkitError`] message (our own diagnostic text), which is additionally run
//! through [`redact_text_to_limit`] (the same heuristic redaction the session
//! event mapper uses) and byte-bounded. No event or audit row carries a server
//! body, a header, an env value, or argument payloads.
//!
//! The logs are **in-memory + session-scoped** (mirroring the pin store): a
//! [`fork`](super::runtime::McpRuntime::fork) starts with an empty observer.
//! Both logs are bounded (oldest entries dropped past the cap) so a long-running
//! session cannot grow them without limit.

use std::sync::Mutex;

use pocopine_agenkit_core::AgenkitError;
use serde::{Deserialize, Serialize};

use crate::events::FrameworkEvent;

use super::pins::PinStatus;

/// Byte cap applied to any free-form detail string recorded in an audit entry or
/// event message (an error message; never a secret or a server body).
const MAX_DETAIL_BYTES: usize = 4096;

/// Cap on the number of retained events / audit entries. Past this the oldest
/// entries are dropped so a long session can't grow the logs without bound.
const MAX_LOG_ENTRIES: usize = 8192;

/// Event/audit prefix used for connect records (the "tool" of a connect event).
const CONNECT_TOOL_PREFIX: &str = "mcp.connect";

/// What kind of MCP action an [`McpAuditEntry`] records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuditKind {
    /// Opening a transport / running the handshake to a server.
    Connect,
    /// Invoking a remote tool / resource / prompt.
    Call,
    /// Recording an operator (or allowlist) approval of a tool definition.
    Approval,
    /// An `Ask` tool was dispatched without an approved pin: the T1 approval
    /// payload (command/origin + description + params) was surfaced for a host to
    /// act on, and the call failed closed pending that decision.
    ApprovalRequested,
    /// Observing a tool definition hash on `tools/list_changed` (rug-pull guard).
    HashChange,
}

/// The outcome recorded for an audited action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuditOutcome {
    /// The action began (paired with a later `Completed`/`Failed`/`Blocked`).
    Started,
    /// The action completed successfully.
    Completed,
    /// The action failed (transport / protocol / runtime error).
    Failed,
    /// The action was refused by policy (capability admission / pin gate).
    Blocked,
    /// A pin observation found the definition unchanged.
    Unchanged,
    /// A pin observation found the definition changed (approval cleared).
    Changed,
}

/// A single audit record. Carries **no secret material** — only ids, server/tool
/// names, and a bounded, redaction-safe detail string (our own status / error
/// text, never a secret value, header, env value, or raw server body).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpAuditEntry {
    /// The action kind.
    pub kind: McpAuditKind,
    /// The action outcome.
    pub outcome: McpAuditOutcome,
    /// The owning server name.
    pub server: String,
    /// The tool / resource / prompt name, when the action targets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// A bounded, redaction-safe detail (error message / status). Never a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// In-memory, session-scoped collector for MCP events + audit entries. Shared
/// (`Arc`) across a runtime's clones and handed to every adapter built off it.
#[derive(Default)]
pub struct McpObserver {
    events: Mutex<Vec<FrameworkEvent>>,
    audit: Mutex<Vec<McpAuditEntry>>,
}

impl std::fmt::Debug for McpObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpObserver")
            .field("events", &self.events.lock().map(|e| e.len()).unwrap_or(0))
            .field("audit", &self.audit.lock().map(|a| a.len()).unwrap_or(0))
            .finish()
    }
}

impl McpObserver {
    /// A fresh, empty observer.
    pub fn new() -> Self {
        Self::default()
    }

    // --- snapshots (tests / host UX) --------------------------------------

    /// A snapshot of the recorded framework events.
    pub fn events(&self) -> Vec<FrameworkEvent> {
        self.events
            .lock()
            .expect("mcp observer events mutex poisoned")
            .clone()
    }

    /// A snapshot of the recorded audit entries.
    pub fn audit(&self) -> Vec<McpAuditEntry> {
        self.audit
            .lock()
            .expect("mcp observer audit mutex poisoned")
            .clone()
    }

    fn push_event(&self, event: FrameworkEvent) {
        let mut events = self
            .events
            .lock()
            .expect("mcp observer events mutex poisoned");
        if events.len() >= MAX_LOG_ENTRIES {
            events.remove(0);
        }
        events.push(event);
    }

    fn push_audit(&self, entry: McpAuditEntry) {
        let mut audit = self
            .audit
            .lock()
            .expect("mcp observer audit mutex poisoned");
        if audit.len() >= MAX_LOG_ENTRIES {
            audit.remove(0);
        }
        audit.push(entry);
    }

    // --- connect -----------------------------------------------------------

    /// Record the start of a connect attempt.
    pub fn connect_started(&self, server: &str) {
        self.push_event(FrameworkEvent::tool_started(format!(
            "{CONNECT_TOOL_PREFIX}.{server}"
        )));
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::Connect,
            outcome: McpAuditOutcome::Started,
            server: server.to_string(),
            tool: None,
            detail: None,
        });
        tracing::debug!(target: "pocopine.log", server, "mcp connect started");
    }

    /// Record a successful connect.
    pub fn connect_completed(
        &self,
        server: &str,
        transport: &str,
        protocol_version: &str,
        tool_count: usize,
    ) {
        self.push_event(FrameworkEvent::tool_completed(format!(
            "{CONNECT_TOOL_PREFIX}.{server}"
        )));
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::Connect,
            outcome: McpAuditOutcome::Completed,
            server: server.to_string(),
            tool: None,
            detail: Some(format!(
                "{transport} protocol={protocol_version} tools={tool_count}"
            )),
        });
        tracing::info!(
            target: "pocopine.log",
            server,
            transport,
            protocol_version,
            tool_count,
            "mcp connect completed"
        );
    }

    /// Record a failed connect (the error message is bounded + redacted).
    pub fn connect_failed(&self, server: &str, error: &AgenkitError) {
        let detail = detail_from_error(error);
        self.push_event(FrameworkEvent::tool_failed(
            format!("{CONNECT_TOOL_PREFIX}.{server}"),
            detail.clone(),
        ));
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::Connect,
            outcome: McpAuditOutcome::Failed,
            server: server.to_string(),
            tool: None,
            detail: Some(detail),
        });
        tracing::warn!(target: "pocopine.log", server, kind = error.kind(), "mcp connect failed");
    }

    // --- call --------------------------------------------------------------

    /// Record the start of a remote call (`id` = the namespaced tool id / verb).
    pub fn call_started(&self, id: &str, server: &str, tool: &str) {
        self.push_event(FrameworkEvent::tool_started(id.to_string()));
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::Call,
            outcome: McpAuditOutcome::Started,
            server: server.to_string(),
            tool: Some(tool.to_string()),
            detail: None,
        });
        tracing::debug!(target: "pocopine.log", tool_id = id, server, tool, "mcp call started");
    }

    /// Record a completed call.
    pub fn call_completed(&self, id: &str, server: &str, tool: &str) {
        self.push_event(FrameworkEvent::tool_completed(id.to_string()));
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::Call,
            outcome: McpAuditOutcome::Completed,
            server: server.to_string(),
            tool: Some(tool.to_string()),
            detail: None,
        });
        tracing::debug!(target: "pocopine.log", tool_id = id, server, tool, "mcp call completed");
    }

    /// Record a failed call (the error message is bounded + redacted).
    pub fn call_failed(&self, id: &str, server: &str, tool: &str, error: &AgenkitError) {
        let detail = detail_from_error(error);
        self.push_event(FrameworkEvent::tool_failed(id.to_string(), detail.clone()));
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::Call,
            outcome: McpAuditOutcome::Failed,
            server: server.to_string(),
            tool: Some(tool.to_string()),
            detail: Some(detail),
        });
        tracing::warn!(
            target: "pocopine.log",
            tool_id = id,
            server,
            tool,
            kind = error.kind(),
            "mcp call failed"
        );
    }

    /// Record a policy-blocked call (capability admission / pin gate). Emits a
    /// [`FrameworkEventKind::ToolBlocked`](crate::events::FrameworkEventKind).
    pub fn call_blocked(&self, id: &str, server: &str, tool: &str, reason: &str) {
        let detail = redact_detail(reason);
        self.push_event(FrameworkEvent::tool_blocked(id.to_string(), detail.clone()));
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::Call,
            outcome: McpAuditOutcome::Blocked,
            server: server.to_string(),
            tool: Some(tool.to_string()),
            detail: Some(detail),
        });
        tracing::warn!(target: "pocopine.log", tool_id = id, server, tool, "mcp call blocked by policy");
    }

    // --- approval / hash change -------------------------------------------

    /// Record that an `Ask` tool was dispatched while unapproved: the T1 approval
    /// request (a secret-free `command/origin + tool` summary) is surfaced here so
    /// a host can present it, while the call itself fails closed (the interactive
    /// approve/deny loop is a deferred host-layer concern, M4). `summary` is the
    /// [`AskPrompt::summary`](super::admission::AskPrompt::summary) string —
    /// secret-free by construction; it is additionally redaction-bounded here.
    pub fn approval_requested(&self, server: &str, tool: &str, summary: &str) {
        let detail = redact_detail(summary);
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::ApprovalRequested,
            outcome: McpAuditOutcome::Started,
            server: server.to_string(),
            tool: Some(tool.to_string()),
            detail: Some(detail),
        });
        tracing::info!(
            target: "pocopine.log",
            server,
            tool,
            "mcp tool dispatched while unapproved; approval requested (awaiting host decision)"
        );
    }

    /// Record that a tool definition was approved (operator allowlist / Allow).
    pub fn approval(&self, server: &str, tool: &str) {
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::Approval,
            outcome: McpAuditOutcome::Completed,
            server: server.to_string(),
            tool: Some(tool.to_string()),
            detail: None,
        });
        tracing::info!(target: "pocopine.log", server, tool, "mcp tool definition approved");
    }

    /// Record a pin observation on re-discovery (`tools/list_changed`): a
    /// `Changed` outcome means the prior approval was cleared (rug-pull guard).
    pub fn hash_change(&self, server: &str, tool: &str, status: PinStatus) {
        let outcome = match status {
            PinStatus::New => McpAuditOutcome::Started,
            PinStatus::Unchanged => McpAuditOutcome::Unchanged,
            PinStatus::Changed => McpAuditOutcome::Changed,
        };
        self.push_audit(McpAuditEntry {
            kind: McpAuditKind::HashChange,
            outcome,
            server: server.to_string(),
            tool: Some(tool.to_string()),
            detail: None,
        });
    }
}

/// Bound + redact an [`AgenkitError`] into a recordable detail string.
fn detail_from_error(error: &AgenkitError) -> String {
    redact_detail(&error.to_string())
}

/// Substrings that, if present in a detail string, fully redact it — a defensive
/// guard so a credential that accidentally lands in an error message never
/// reaches an event/audit row verbatim.
const SENSITIVE_MARKERS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "x-api-key",
    "access_token",
    "refresh_token",
    "id_token",
    "token=",
    "authorization",
    "password",
    "secret",
    "credential",
    "private_key",
    "bearer ",
    "-----begin ",
    "private key",
];

/// Bound + heuristically redact a free-form detail string. If it looks like it
/// carries a credential it is fully replaced; otherwise it is byte-bounded
/// (UTF-8-safe) to [`MAX_DETAIL_BYTES`].
fn redact_detail(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if SENSITIVE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return "[redacted]".to_string();
    }
    if text.len() <= MAX_DETAIL_BYTES {
        return text.to_string();
    }
    let mut end = MAX_DETAIL_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = text[..end].to_string();
    bounded.push_str("…(truncated)");
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::FrameworkEventKind;

    #[test]
    fn connect_records_event_and_audit() {
        let obs = McpObserver::new();
        obs.connect_started("github");
        obs.connect_completed("github", "stdio", "2025-11-25", 3);

        let events = obs.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, FrameworkEventKind::ToolStarted);
        assert_eq!(events[1].kind, FrameworkEventKind::ToolCompleted);
        assert_eq!(events[0].tool.as_deref(), Some("mcp.connect.github"));

        let audit = obs.audit();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[1].kind, McpAuditKind::Connect);
        assert_eq!(audit[1].outcome, McpAuditOutcome::Completed);
    }

    #[test]
    fn call_lifecycle_emits_expected_event_kinds() {
        let obs = McpObserver::new();
        obs.call_started("mcp.s.t", "s", "t");
        obs.call_completed("mcp.s.t", "s", "t");
        obs.call_failed("mcp.s.t", "s", "t", &AgenkitError::internal("boom"));
        obs.call_blocked("mcp.s.t", "s", "t", "denied by policy");

        let kinds: Vec<_> = obs.events().into_iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                FrameworkEventKind::ToolStarted,
                FrameworkEventKind::ToolCompleted,
                FrameworkEventKind::ToolFailed,
                FrameworkEventKind::ToolBlocked,
            ]
        );
    }

    #[test]
    fn blocked_call_emits_tool_blocked_with_reason() {
        let obs = McpObserver::new();
        obs.call_blocked("mcp.s.t", "s", "t", "tool is denied by policy");
        let events = obs.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, FrameworkEventKind::ToolBlocked);
        assert!(events[0].message.as_deref().unwrap().contains("denied"));
        let audit = obs.audit();
        assert_eq!(audit[0].outcome, McpAuditOutcome::Blocked);
    }

    #[test]
    fn error_detail_with_secret_shape_is_redacted() {
        // A key=value-shaped error string is fully redacted by the heuristic
        // redactor, so even an error that accidentally embeds a credential never
        // lands verbatim in an event or audit row.
        let obs = McpObserver::new();
        obs.call_failed(
            "mcp.s.t",
            "s",
            "t",
            &AgenkitError::internal("api_key=sk-supersecret-value"),
        );
        let serialized = serde_json::to_string(&obs.audit()).unwrap();
        assert!(!serialized.contains("sk-supersecret-value"));
        let event_serialized = serde_json::to_string(&obs.events()).unwrap();
        assert!(!event_serialized.contains("sk-supersecret-value"));
    }

    #[test]
    fn hash_change_records_changed_outcome() {
        let obs = McpObserver::new();
        obs.hash_change("s", "t", PinStatus::Changed);
        obs.hash_change("s", "t", PinStatus::Unchanged);
        let audit = obs.audit();
        assert_eq!(audit[0].kind, McpAuditKind::HashChange);
        assert_eq!(audit[0].outcome, McpAuditOutcome::Changed);
        assert_eq!(audit[1].outcome, McpAuditOutcome::Unchanged);
    }

    #[test]
    fn logs_are_bounded() {
        let obs = McpObserver::new();
        for _ in 0..(MAX_LOG_ENTRIES + 16) {
            obs.call_started("mcp.s.t", "s", "t");
        }
        assert_eq!(obs.events().len(), MAX_LOG_ENTRIES);
        assert_eq!(obs.audit().len(), MAX_LOG_ENTRIES);
    }
}
