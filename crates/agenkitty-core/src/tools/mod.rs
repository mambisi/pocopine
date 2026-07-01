use pocopine_agenkit_core::{ToolCall, ToolDescriptor, ToolResult, ToolSideEffectPolicy};
use serde::{Deserialize, Serialize};

use crate::policy::{CapabilitySet, PolicyDecision, ToolMode};

/// Where a framework tool comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    BuiltIn,
    Generated,
    SubAgent,
    External,
}

/// Lifecycle state of a tool visible to planning/research.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLifecycle {
    Proposed,
    Available,
    Disabled,
    Deprecated,
}

/// Agenkitty's policy/research wrapper around Agenkit's descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub descriptor: ToolDescriptor,
    pub kind: ToolKind,
    #[serde(default = "default_lifecycle")]
    pub lifecycle: ToolLifecycle,
    #[serde(default)]
    pub capabilities: CapabilitySet,
    #[serde(default)]
    pub mode: ToolMode,
}

impl ToolSpec {
    pub fn built_in(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            descriptor: ToolDescriptor::new(id, description),
            kind: ToolKind::BuiltIn,
            lifecycle: ToolLifecycle::Available,
            capabilities: CapabilitySet::default(),
            mode: ToolMode::Ask,
        }
    }

    pub fn side_effecting(mut self) -> Self {
        self.descriptor.side_effect = ToolSideEffectPolicy::SideEffecting;
        self
    }
}

/// Why a planner selected a tool for a task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSelection {
    pub tool_id: String,
    pub reason: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
}

/// A policy-aware tool invocation request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolUseRequest {
    pub call: ToolCall,
    pub selection: ToolSelection,
    pub mode: ToolMode,
    #[serde(default)]
    pub capabilities: CapabilitySet,
}

impl ToolUseRequest {
    pub fn new(call: ToolCall, selection: ToolSelection, spec: &ToolSpec) -> Self {
        Self {
            call,
            selection,
            mode: spec.mode,
            capabilities: spec.capabilities.clone(),
        }
    }
}

/// Framework-level result of evaluating and/or executing a tool call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolUseOutcome {
    pub status: ToolUseStatus,
    pub decision: PolicyDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ToolResult>,
}

impl ToolUseOutcome {
    pub fn allowed(result: ToolResult) -> Self {
        Self {
            status: ToolUseStatus::Completed,
            decision: PolicyDecision::Allow,
            result: Some(result),
        }
    }

    pub fn blocked(reason: impl Into<String>) -> Self {
        Self {
            status: ToolUseStatus::Blocked,
            decision: PolicyDecision::Deny {
                reason: reason.into(),
            },
            result: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolUseStatus {
    Pending,
    AwaitingApproval,
    Running,
    Completed,
    Failed,
    Blocked,
}

fn default_lifecycle() -> ToolLifecycle {
    ToolLifecycle::Proposed
}

#[cfg(test)]
mod tests {
    use pocopine_agenkit_core::{ToolCall, ToolResult};
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_use_request_carries_policy_context() {
        let spec = ToolSpec::built_in("fs.search", "Search repository");
        let call = ToolCall::new("call-1", "fs.search", json!({"query": "ToolRegistry"}));
        let selection = ToolSelection {
            tool_id: "fs.search".to_string(),
            reason: "Need to inspect existing tool APIs".to_string(),
            evidence: vec!["pocopine-agenkit has ToolRegistry".to_string()],
            confidence: Some(0.9),
        };
        let request = ToolUseRequest::new(call, selection, &spec);
        assert_eq!(request.mode, ToolMode::Ask);
        assert_eq!(request.selection.tool_id, "fs.search");
    }

    #[test]
    fn tool_use_outcome_separates_blocked_from_executed() {
        let blocked = ToolUseOutcome::blocked("write access denied");
        assert_eq!(blocked.status, ToolUseStatus::Blocked);
        assert!(blocked.result.is_none());

        let completed = ToolUseOutcome::allowed(ToolResult::ok("call-1", json!({"ok": true})));
        assert_eq!(completed.status, ToolUseStatus::Completed);
        assert!(completed.result.is_some());
    }
}
