use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sessions::SessionSourceRef;

/// CLI/API-friendly framework event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FrameworkEvent {
    pub kind: FrameworkEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The per-invocation tool-call id (the same id the agenkit loop hands the
    /// `before_tool_call` hook), so policy decisions and events correlate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<SessionSourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl FrameworkEvent {
    pub fn started(message: impl Into<String>) -> Self {
        Self {
            kind: FrameworkEventKind::Started,
            message: Some(message.into()),
            tool: None,
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            kind: FrameworkEventKind::AssistantText,
            message: Some(text.into()),
            tool: None,
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn stopped(status: RunStatus) -> Self {
        Self {
            kind: FrameworkEventKind::Stopped { status },
            message: None,
            tool: None,
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            kind: FrameworkEventKind::Failed,
            message: Some(error.into()),
            tool: None,
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn tool_started(tool: impl Into<String>) -> Self {
        Self {
            kind: FrameworkEventKind::ToolStarted,
            message: None,
            tool: Some(tool.into()),
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn tool_completed(tool: impl Into<String>) -> Self {
        Self {
            kind: FrameworkEventKind::ToolCompleted,
            message: None,
            tool: Some(tool.into()),
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn tool_failed(tool: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            kind: FrameworkEventKind::ToolFailed,
            message: Some(error.into()),
            tool: Some(tool.into()),
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn tool_blocked(tool: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: FrameworkEventKind::ToolBlocked,
            message: Some(reason.into()),
            tool: Some(tool.into()),
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn compacted(folded: u64) -> Self {
        Self {
            kind: FrameworkEventKind::Compacted,
            message: Some(format!("folded {folded} messages")),
            tool: None,
            call_id: None,
            source_refs: Vec::new(),
            payload: Some(serde_json::json!({ "folded": folded })),
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            kind: FrameworkEventKind::Unknown,
            message: Some(message.into()),
            tool: None,
            call_id: None,
            source_refs: Vec::new(),
            payload: None,
        }
    }

    pub fn with_source_ref(mut self, source_ref: SessionSourceRef) -> Self {
        self.source_refs.push(source_ref);
        self
    }

    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = Some(payload);
        self
    }

    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum FrameworkEventKind {
    Started,
    AssistantText,
    ToolStarted,
    ToolCompleted,
    ToolFailed,
    ToolBlocked,
    Compacted,
    Stopped { status: RunStatus },
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Idle,
    MaxSteps,
    Aborted,
    Unknown,
}
