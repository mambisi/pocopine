use serde::{Deserialize, Serialize};

/// Default policy mode for a class of tool calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolMode {
    Allow,
    #[default]
    Ask,
    Deny,
}

impl ToolMode {
    pub fn ask() -> Self {
        Self::Ask
    }
}

/// Framework-level policy result before it is adapted to Agenkit's
/// `ToolDecision`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    Ask { reason: String },
    Deny { reason: String },
    Rewrite { args: serde_json::Value },
}

impl PolicyDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow | Self::Rewrite { .. })
    }
}
