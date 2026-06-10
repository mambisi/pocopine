//! Agent thread descriptors and messages (RFC-093 Phase 1.4, §D4, §D10).
//!
//! A thread is optional conversation/session state. Ids exposed to clients are
//! opaque Pocopine ids, never provider-backed thread ids; messages and
//! checkpoints are privacy-labeled, and raw prompts/tool args/provider
//! payloads are not stored unless capture policy enables it (§D10).

use crate::{AgentThreadId, Content, Role, ThreadRetention};

/// The framework-visible description of an agent thread.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentThreadDescriptor {
    /// The opaque thread id.
    pub id: AgentThreadId,
    /// The agent this thread belongs to.
    pub agent_id: String,
    /// How long the thread is retained.
    pub retention: ThreadRetention,
}

impl AgentThreadDescriptor {
    /// A thread descriptor.
    pub fn new(id: AgentThreadId, agent_id: impl Into<String>, retention: ThreadRetention) -> Self {
        Self {
            id,
            agent_id: agent_id.into(),
            retention,
        }
    }
}

/// One stored message in a thread's history.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadMessage {
    /// Who authored the message.
    pub role: Role,
    /// The (privacy-labeled) message body.
    pub content: Content,
    /// Wall-clock timestamp in epoch millis, when known. Supplied by the host
    /// (core has no clock).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<u64>,
}

impl ThreadMessage {
    /// A thread message.
    pub fn new(role: Role, content: impl Into<Content>) -> Self {
        Self {
            role,
            content: content.into(),
            ts_ms: None,
        }
    }

    /// Stamp a timestamp.
    pub fn at(mut self, ts_ms: u64) -> Self {
        self.ts_ms = Some(ts_ms);
        self
    }
}

/// A resumable checkpoint within a thread.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadCheckpoint {
    /// Opaque checkpoint id.
    pub id: String,
    /// An optional safe summary of the thread state at this checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ThreadCheckpoint {
    /// A checkpoint with an optional summary.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            summary: None,
        }
    }

    /// Attach a safe summary.
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_descriptor_and_message_round_trip() {
        let descriptor = AgentThreadDescriptor::new(
            AgentThreadId::new("th-1"),
            "debugger_agent",
            ThreadRetention::Session,
        );
        let back: AgentThreadDescriptor =
            serde_json::from_str(&serde_json::to_string(&descriptor).unwrap()).unwrap();
        assert_eq!(descriptor, back);

        let msg = ThreadMessage::new(Role::User, "why did the upload fail?").at(1_700_000_000_000);
        assert_eq!(msg.ts_ms, Some(1_700_000_000_000));
        let back: ThreadMessage =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(msg, back);
    }
}
