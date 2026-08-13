use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable identity for an Agenkitty session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIdentity {
    pub thread_id: String,
    pub agent_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_key: Option<String>,
    #[serde(default)]
    pub tool_ids: Vec<String>,
    pub max_steps_per_turn: u32,
    pub capture_policy: String,
    pub transcript_store: SessionStoreKind,
    pub metadata_store: SessionStoreKind,
    pub created_at_ms: u64,
    pub last_active_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// Storage durability class visible to the model and CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStoreKind {
    InMemory,
    LocalJsonl,
    Sqlite,
    HostProvided,
    Unknown,
}

/// Stable references from session metadata into transcript, tool, artifact, or
/// workspace state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionSourceRef {
    Thread {
        thread_id: String,
    },
    SessionThread {
        session_thread_id: String,
    },
    Record {
        thread_id: String,
        seq: u64,
    },
    RecordRange {
        thread_id: String,
        start_seq: u64,
        end_seq: u64,
    },
    Event {
        seq: u64,
    },
    EventRange {
        start_seq: u64,
        end_seq: u64,
    },
    Step {
        step_id: String,
    },
    ToolCall {
        call_id: String,
        tool_id: String,
    },
    Tool {
        tool_id: String,
    },
    Artifact {
        artifact_id: String,
    },
    Checkpoint {
        checkpoint_id: String,
    },
    Path {
        path: String,
    },
}

/// Model-safe session event kind.
///
/// Deserialization is **forward compatible**: a kind this build doesn't know
/// decodes to [`SessionEventKind::Unknown`] rather than failing, so an older
/// reader can still open a log written by a newer one. (`#[serde(other)]` only
/// applies to internally/adjacently tagged enums, so the fallback is
/// hand-written below.)
///
/// Both halves are hand-written and both go through [`as_str`], so the encoding
/// is the same snake_case string in every format — including binary ones, where
/// a derived `Serialize` would have written a variant index that the
/// string-reading `Deserialize` could not decode. The JSON form is unchanged
/// from the derived `rename_all = "snake_case"` one.
///
/// [`as_str`]: SessionEventKind::as_str
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEventKind {
    Started,
    AssistantText,
    ToolStarted,
    ToolCompleted,
    ToolFailed,
    ToolBlocked,
    Compacted,
    Stopped,
    Failed,
    NoteCreated,
    SummaryCreated,
    CheckpointCreated,
    /// A subagent was spawned by this session. Carries the child's thread id as
    /// a [`SessionSourceRef::Thread`] so the delegation tree is walkable from
    /// the parent; the child records the inverse edge on its own `Started`.
    SubagentStarted,
    /// A subagent this session spawned reached a terminal state (its status and
    /// usage rollup ride in the payload).
    SubagentFinished,
    Unknown,
}

impl SessionEventKind {
    /// The wire string for this kind (the `rename_all = "snake_case"` form).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::AssistantText => "assistant_text",
            Self::ToolStarted => "tool_started",
            Self::ToolCompleted => "tool_completed",
            Self::ToolFailed => "tool_failed",
            Self::ToolBlocked => "tool_blocked",
            Self::Compacted => "compacted",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::NoteCreated => "note_created",
            Self::SummaryCreated => "summary_created",
            Self::CheckpointCreated => "checkpoint_created",
            Self::SubagentStarted => "subagent_started",
            Self::SubagentFinished => "subagent_finished",
            Self::Unknown => "unknown",
        }
    }
}

impl Serialize for SessionEventKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Unconditionally a string — no `is_human_readable` branch — so this
        // cannot drift from the string-reading `Deserialize`. A derived impl
        // would emit the variant *index* to a binary format, which that
        // `Deserialize` could not read back.
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionEventKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Unrecognized kinds degrade to `Unknown` instead of erroring: an event
        // log is append-only and long-lived, so a reader must survive kinds
        // added after it was built.
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "started" => Self::Started,
            "assistant_text" => Self::AssistantText,
            "tool_started" => Self::ToolStarted,
            "tool_completed" => Self::ToolCompleted,
            "tool_failed" => Self::ToolFailed,
            "tool_blocked" => Self::ToolBlocked,
            "compacted" => Self::Compacted,
            "stopped" => Self::Stopped,
            "failed" => Self::Failed,
            "note_created" => Self::NoteCreated,
            "summary_created" => Self::SummaryCreated,
            "checkpoint_created" => Self::CheckpointCreated,
            "subagent_started" => Self::SubagentStarted,
            "subagent_finished" => Self::SubagentFinished,
            _ => Self::Unknown,
        })
    }
}

/// Coarse classification for model-visible session event policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventPolicy {
    ReadOnly,
    SideEffecting,
    Blocked,
    Failed,
}

/// A bounded, redacted event in the Agenkitty session metadata stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub kind: SessionEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<SessionSourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<SessionEventPolicy>,
}

impl SessionEvent {
    pub fn new(kind: SessionEventKind, timestamp_ms: u64) -> Self {
        Self {
            seq: 0,
            timestamp_ms,
            kind,
            message: None,
            tool: None,
            source_refs: Vec::new(),
            payload: None,
            policy: None,
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = Some(payload);
        self
    }

    pub fn with_policy(mut self, policy: SessionEventPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    pub fn with_source_ref(mut self, source_ref: SessionSourceRef) -> Self {
        self.source_refs.push(source_ref);
        self
    }
}

/// Cursor for bounded session event reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
}

/// Inclusive event sequence range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventRange {
    pub start_seq: u64,
    pub end_seq: u64,
}

/// Session-scoped note. Promotion to memory is separate policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionNote {
    pub id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub reason: String,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRef>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Compact summary of a bounded session range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_events: Option<SessionEventRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_records: Option<SessionEventRange>,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_marker: Option<String>,
}

/// Session checkpoint metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: SessionCheckpointKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_events: Option<SessionEventRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_records: Option<SessionEventRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRef>,
    pub created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCheckpointKind {
    AutoCompaction,
    Manual,
    PreTool,
    PostTool,
    HostImport,
    WorkspaceMarker,
}

/// Link from a session to an artifact produced or referenced during the run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionArtifactLink {
    pub artifact_id: String,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion_policy: Option<String>,
    pub created_at_ms: u64,
}

/// Terminal metadata for a closed session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClosure {
    pub closed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRef>,
}

/// Portable session export payload used by host APIs and local tooling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionExport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<SessionIdentity>,
    #[serde(default)]
    pub total_events: u64,
    #[serde(default)]
    pub events_truncated: bool,
    #[serde(default)]
    pub events: Vec<SessionEvent>,
    #[serde(default)]
    pub notes: Vec<SessionNote>,
    #[serde(default)]
    pub summaries: Vec<SessionSummary>,
    #[serde(default)]
    pub checkpoints: Vec<SessionCheckpoint>,
    #[serde(default)]
    pub artifact_links: Vec<SessionArtifactLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure: Option<SessionClosure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_refs_round_trip() {
        let refs = vec![
            SessionSourceRef::Thread {
                thread_id: "t-1".to_string(),
            },
            SessionSourceRef::RecordRange {
                thread_id: "t-1".to_string(),
                start_seq: 1,
                end_seq: 3,
            },
            SessionSourceRef::ToolCall {
                call_id: "call-1".to_string(),
                tool_id: "fs.read".to_string(),
            },
        ];

        let json = serde_json::to_string(&refs).unwrap();
        let back: Vec<SessionSourceRef> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, refs);
        assert!(json.contains("\"kind\":\"record_range\""));
    }

    #[test]
    fn event_kind_uses_snake_case() {
        let value = serde_json::to_value(SessionEventKind::ToolCompleted).unwrap();
        assert_eq!(value, serde_json::json!("tool_completed"));
    }

    #[test]
    fn every_event_kind_round_trips_and_as_str_matches_the_wire() {
        // `as_str` is hand-written next to a derived Serialize and a hand-written
        // Deserialize; this pins all three to the same string.
        for kind in [
            SessionEventKind::Started,
            SessionEventKind::AssistantText,
            SessionEventKind::ToolStarted,
            SessionEventKind::ToolCompleted,
            SessionEventKind::ToolFailed,
            SessionEventKind::ToolBlocked,
            SessionEventKind::Compacted,
            SessionEventKind::Stopped,
            SessionEventKind::Failed,
            SessionEventKind::NoteCreated,
            SessionEventKind::SummaryCreated,
            SessionEventKind::CheckpointCreated,
            SessionEventKind::SubagentStarted,
            SessionEventKind::SubagentFinished,
            SessionEventKind::Unknown,
        ] {
            let value = serde_json::to_value(kind).unwrap();
            assert_eq!(value, serde_json::json!(kind.as_str()), "{kind:?}");
            let back: SessionEventKind = serde_json::from_value(value).unwrap();
            assert_eq!(back, kind);
        }
        assert_eq!(
            serde_json::to_value(SessionEventKind::SubagentStarted).unwrap(),
            serde_json::json!("subagent_started")
        );
    }

    #[test]
    fn an_unrecognized_event_kind_decodes_to_unknown() {
        // Forward compatibility: an older reader must survive a log written by a
        // newer build rather than failing the whole record.
        let kind: SessionEventKind =
            serde_json::from_value(serde_json::json!("teleported")).unwrap();
        assert_eq!(kind, SessionEventKind::Unknown);

        let event: SessionEvent = serde_json::from_value(serde_json::json!({
            "seq": 1,
            "timestamp_ms": 7,
            "kind": "invented_in_a_later_release",
            "message": "still readable",
        }))
        .unwrap();
        assert_eq!(event.kind, SessionEventKind::Unknown);
        assert_eq!(event.message.as_deref(), Some("still readable"));
    }

    #[test]
    fn session_export_round_trips_links_and_closure() {
        let export = SessionExport {
            identity: None,
            total_events: 0,
            events_truncated: false,
            events: Vec::new(),
            notes: Vec::new(),
            summaries: Vec::new(),
            checkpoints: Vec::new(),
            artifact_links: vec![SessionArtifactLink {
                artifact_id: "artifact-1".to_string(),
                source_refs: vec![SessionSourceRef::Event { seq: 1 }],
                promotion_policy: Some("manual".to_string()),
                created_at_ms: 10,
            }],
            closure: Some(SessionClosure {
                closed_at_ms: 12,
                reason: Some("done".to_string()),
                source_refs: Vec::new(),
            }),
        };

        let json = serde_json::to_string(&export).unwrap();
        assert!(json.contains("\"artifact_id\":\"artifact-1\""));
        assert!(json.contains("\"reason\":\"done\""));
        let back: SessionExport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, export);
    }
}
