use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a host approver is asked to decide on. One shape serves every Ask
/// gate: the central dispatch gate (carries the invocation id + full
/// arguments), the secret-grant gate, and the MCP admission gate (carries the
/// T1 approval payload as `detail`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The tool (or subsystem verb) the request is about.
    pub tool_id: String,
    /// Why approval is required — shown to the operator verbatim.
    pub reason: String,
    /// The per-invocation call id when the gate sits at agent-loop dispatch;
    /// subsystem gates (secrets, MCP admission) have no agenkit call id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Gate-specific payload the operator needs to judge the request: the
    /// call arguments at the dispatch gate, the pinned definition summary at
    /// the MCP gate. Hosts must render it bounded + redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl ApprovalRequest {
    pub fn new(tool_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            tool_id: tool_id.into(),
            reason: reason.into(),
            call_id: None,
            detail: None,
        }
    }

    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// The operator's answer to an [`ApprovalRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Denied { reason: String },
}

impl ApprovalDecision {
    pub fn denied(reason: impl Into<String>) -> Self {
        Self::Denied {
            reason: reason.into(),
        }
    }

    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved)
    }
}
