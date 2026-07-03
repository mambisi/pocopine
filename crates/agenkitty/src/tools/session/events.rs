use std::sync::Arc;

use agenkitty_core::{SessionEvent, SessionEventKind, SessionEventPolicy};
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{SessionEventFilter, SessionRuntime};

pub const SESSION_EVENTS_TOOL_ID: &str = "session.events";

const DEFAULT_EVENT_LIMIT: usize = 20;
const MAX_EVENT_LIMIT: usize = 100;
const MAX_EVENT_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct SessionEventsTool {
    runtime: Arc<SessionRuntime>,
}

impl SessionEventsTool {
    pub fn new(runtime: Arc<SessionRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: SessionEventsInput) -> AgenkitResult<SessionEventsOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let limit = input
            .limit
            .unwrap_or(DEFAULT_EVENT_LIMIT)
            .clamp(1, MAX_EVENT_LIMIT);
        let kinds = input
            .kinds
            .into_iter()
            .map(SessionEventKind::from)
            .collect();
        let events = self
            .runtime
            .store()
            .list_events(
                &context.identity.thread_id,
                SessionEventFilter {
                    after_seq: input.after_seq,
                    start_seq: input.start_seq,
                    end_seq: input.end_seq,
                    limit,
                    kinds,
                },
            )
            .await?;
        let (events, truncated) = bounded_event_views(events, MAX_EVENT_OUTPUT_BYTES);
        let next_after_seq = events.last().map(|event| event.seq);
        Ok(SessionEventsOutput {
            thread_id: context.identity.thread_id,
            events,
            next_after_seq,
            truncated,
        })
    }
}

impl AiTool for SessionEventsTool {
    const ID: &'static str = SESSION_EVENTS_TOOL_ID;
    type Input = SessionEventsInput;
    type Output = SessionEventsOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SESSION_EVENTS_TOOL_ID,
            "Return a bounded, redacted window of recent events for the current agent session.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input).await })
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct SessionEventsInput {
    #[serde(default)]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub start_seq: Option<u64>,
    #[serde(default)]
    pub end_seq: Option<u64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub kinds: Vec<SessionEventKindFilter>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKindFilter {
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
    Unknown,
}

impl From<SessionEventKindFilter> for SessionEventKind {
    fn from(value: SessionEventKindFilter) -> Self {
        match value {
            SessionEventKindFilter::Started => Self::Started,
            SessionEventKindFilter::AssistantText => Self::AssistantText,
            SessionEventKindFilter::ToolStarted => Self::ToolStarted,
            SessionEventKindFilter::ToolCompleted => Self::ToolCompleted,
            SessionEventKindFilter::ToolFailed => Self::ToolFailed,
            SessionEventKindFilter::ToolBlocked => Self::ToolBlocked,
            SessionEventKindFilter::Compacted => Self::Compacted,
            SessionEventKindFilter::Stopped => Self::Stopped,
            SessionEventKindFilter::Failed => Self::Failed,
            SessionEventKindFilter::NoteCreated => Self::NoteCreated,
            SessionEventKindFilter::SummaryCreated => Self::SummaryCreated,
            SessionEventKindFilter::CheckpointCreated => Self::CheckpointCreated,
            SessionEventKindFilter::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SessionEventsOutput {
    pub thread_id: String,
    pub events: Vec<SessionEventView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_seq: Option<u64>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SessionEventView {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
}

impl From<SessionEvent> for SessionEventView {
    fn from(event: SessionEvent) -> Self {
        Self {
            seq: event.seq,
            timestamp_ms: event.timestamp_ms,
            kind: kind_name(event.kind).to_string(),
            message: event.message,
            tool: event.tool,
            payload: event.payload,
            policy: event.policy.map(|policy| policy_name(policy).to_string()),
        }
    }
}

/// Render events to views, stopping once the serialized output would exceed
/// `max_bytes` (always keeping at least one). Shared by `session.events` and
/// `session.search`.
pub(super) fn bounded_event_views(
    events: Vec<SessionEvent>,
    max_bytes: usize,
) -> (Vec<SessionEventView>, bool) {
    let mut views = Vec::new();
    let mut bytes = 0usize;
    for event in events {
        let view = SessionEventView::from(event);
        let estimate = serde_json::to_vec(&view).map_or(0, |value| value.len());
        if !views.is_empty() && bytes.saturating_add(estimate) > max_bytes {
            return (views, true);
        }
        bytes = bytes.saturating_add(estimate);
        views.push(view);
    }
    (views, false)
}

fn kind_name(kind: SessionEventKind) -> &'static str {
    match kind {
        SessionEventKind::Started => "started",
        SessionEventKind::AssistantText => "assistant_text",
        SessionEventKind::ToolStarted => "tool_started",
        SessionEventKind::ToolCompleted => "tool_completed",
        SessionEventKind::ToolFailed => "tool_failed",
        SessionEventKind::ToolBlocked => "tool_blocked",
        SessionEventKind::Compacted => "compacted",
        SessionEventKind::Stopped => "stopped",
        SessionEventKind::Failed => "failed",
        SessionEventKind::NoteCreated => "note_created",
        SessionEventKind::SummaryCreated => "summary_created",
        SessionEventKind::CheckpointCreated => "checkpoint_created",
        SessionEventKind::Unknown => "unknown",
    }
}

fn policy_name(policy: SessionEventPolicy) -> &'static str {
    match policy {
        SessionEventPolicy::ReadOnly => "read_only",
        SessionEventPolicy::SideEffecting => "side_effecting",
        SessionEventPolicy::Blocked => "blocked",
        SessionEventPolicy::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session::common::{
        CurrentSessionContext, InMemorySessionMetadataStore, SessionMetadataStore,
    };
    use agenkitty_core::{SessionIdentity, SessionStoreKind};

    fn identity() -> SessionIdentity {
        SessionIdentity {
            thread_id: "thread-1".to_string(),
            agent_id: "agent".to_string(),
            model: "local/default".to_string(),
            run_id: None,
            turn_id: None,
            principal_key: None,
            tool_ids: vec![SESSION_EVENTS_TOOL_ID.to_string()],
            max_steps_per_turn: 8,
            capture_policy: "full".to_string(),
            transcript_store: SessionStoreKind::InMemory,
            metadata_store: SessionStoreKind::InMemory,
            created_at_ms: 1,
            last_active_at_ms: 1,
            project_id: None,
        }
    }

    #[tokio::test]
    async fn session_events_returns_bounded_window() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        let runtime = Arc::new(SessionRuntime::new(store.clone()));
        store
            .append_event("thread-1", SessionEvent::new(SessionEventKind::Started, 1))
            .await
            .unwrap();
        store
            .append_event(
                "thread-1",
                SessionEvent::new(SessionEventKind::ToolCompleted, 2).with_tool("fs.read"),
            )
            .await
            .unwrap();
        let args = runtime
            .inject_context_args(
                &serde_json::json!({"limit": 1, "after_seq": 1}),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let input: SessionEventsInput = serde_json::from_value(args).unwrap();
        let output = SessionEventsTool::new(runtime).run(input).await.unwrap();

        assert_eq!(output.events.len(), 1);
        assert_eq!(output.events[0].seq, 2);
        assert_eq!(output.events[0].tool.as_deref(), Some("fs.read"));
    }

    #[tokio::test]
    async fn session_events_filters_inclusive_sequence_range() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        let runtime = Arc::new(SessionRuntime::new(store.clone()));
        for kind in [
            SessionEventKind::Started,
            SessionEventKind::AssistantText,
            SessionEventKind::ToolCompleted,
            SessionEventKind::Stopped,
        ] {
            store
                .append_event("thread-1", SessionEvent::new(kind, 1))
                .await
                .unwrap();
        }
        let args = runtime
            .inject_context_args(
                &serde_json::json!({"start_seq": 2, "end_seq": 3, "limit": 10}),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let input: SessionEventsInput = serde_json::from_value(args).unwrap();
        let output = SessionEventsTool::new(runtime).run(input).await.unwrap();

        assert_eq!(
            output
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[tokio::test]
    async fn session_events_rejects_missing_context() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let err = SessionEventsTool::new(runtime)
            .run(SessionEventsInput::default())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }
}
