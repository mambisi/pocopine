use std::sync::Arc;

use agenkitty_core::{
    SessionEvent, SessionEventKind, SessionEventPolicy, SessionEventRange, SessionSourceRef,
    SessionSummary,
};
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{SessionRuntime, current_time_ms, redact_text_to_limit};
use super::source_ref::{
    SessionEventRangeSpec, SessionSourceRefSpec, validate_event_range, validate_source_refs,
};

pub const SESSION_SUMMARY_TOOL_ID: &str = "session.summary";

const MAX_SUMMARY_TITLE_BYTES: usize = 256;
const MAX_SUMMARY_BODY_BYTES: usize = 32 * 1024;
const MAX_SUMMARY_MARKER_BYTES: usize = 512;

#[derive(Clone)]
pub struct SessionSummaryTool {
    runtime: Arc<SessionRuntime>,
}

impl SessionSummaryTool {
    pub fn new(runtime: Arc<SessionRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: SessionSummaryInput) -> AgenkitResult<SessionSummaryOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let title = required_text(
            "session.summary title",
            input.title,
            MAX_SUMMARY_TITLE_BYTES,
        )?;
        let body = required_text("session.summary body", input.body, MAX_SUMMARY_BODY_BYTES)?;
        let covered_events = input
            .covered_events
            .map(|range| validate_event_range(SESSION_SUMMARY_TOOL_ID, "covered_events", range))
            .transpose()?;
        let covered_records = input
            .covered_records
            .map(|range| validate_event_range(SESSION_SUMMARY_TOOL_ID, "covered_records", range))
            .transpose()?;
        if covered_events.is_none() && covered_records.is_none() {
            return Err(AgenkitError::validation(
                "session.summary requires covered_events or covered_records",
            ));
        }
        let produced_by = match input.produced_by {
            Some(value) => Some(required_text(
                "session.summary produced_by",
                value,
                MAX_SUMMARY_MARKER_BYTES,
            )?),
            None => Some(format!("agent:{}", context.identity.agent_id)),
        };
        let source_marker = input
            .source_marker
            .map(|marker| {
                required_text(
                    "session.summary source_marker",
                    marker,
                    MAX_SUMMARY_MARKER_BYTES,
                )
            })
            .transpose()?;
        let mut source_refs = if input.source_refs.is_empty() {
            Vec::new()
        } else {
            validate_source_refs(SESSION_SUMMARY_TOOL_ID, input.source_refs)?
        };
        add_range_source_refs(
            &mut source_refs,
            &context.identity.thread_id,
            covered_events.as_ref(),
            covered_records.as_ref(),
        );
        let timestamp_ms = current_time_ms();

        let summary = self
            .runtime
            .store()
            .write_summary(
                &context.identity.thread_id,
                SessionSummary {
                    id: String::new(),
                    title,
                    body,
                    covered_events: covered_events.clone(),
                    covered_records: covered_records.clone(),
                    source_refs,
                    produced_by,
                    source_marker,
                },
            )
            .await?;

        let mut event = SessionEvent::new(SessionEventKind::SummaryCreated, timestamp_ms)
            .with_message(format!("session summary created: {}", summary.title))
            .with_tool(SESSION_SUMMARY_TOOL_ID)
            .with_policy(SessionEventPolicy::SideEffecting)
            .with_payload(serde_json::json!({
                "summary_id": summary.id,
                "title": summary.title,
                "covered_events": summary.covered_events,
                "covered_records": summary.covered_records,
            }))
            .with_source_ref(SessionSourceRef::Tool {
                tool_id: SESSION_SUMMARY_TOOL_ID.to_string(),
            });
        for source_ref in summary.source_refs.clone() {
            event = event.with_source_ref(source_ref);
        }
        let event = self
            .runtime
            .store()
            .append_event(&context.identity.thread_id, event)
            .await?;

        Ok(SessionSummaryOutput {
            thread_id: context.identity.thread_id,
            summary_id: summary.id,
            title: summary.title,
            body: summary.body,
            covered_events: summary.covered_events.map(Into::into),
            covered_records: summary.covered_records.map(Into::into),
            source_refs: summary.source_refs.into_iter().map(Into::into).collect(),
            produced_by: summary.produced_by,
            source_marker: summary.source_marker,
            event_seq: event.seq,
        })
    }
}

impl AiTool for SessionSummaryTool {
    const ID: &'static str = SESSION_SUMMARY_TOOL_ID;
    type Input = SessionSummaryInput;
    type Output = SessionSummaryOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SESSION_SUMMARY_TOOL_ID,
            "Persist a bounded session summary for an explicit event or record range.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input).await })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct SessionSummaryInput {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub covered_events: Option<SessionEventRangeSpec>,
    #[serde(default)]
    pub covered_records: Option<SessionEventRangeSpec>,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRefSpec>,
    #[serde(default)]
    pub produced_by: Option<String>,
    #[serde(default)]
    pub source_marker: Option<String>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SessionSummaryOutput {
    pub thread_id: String,
    pub summary_id: String,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_events: Option<SessionEventRangeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_records: Option<SessionEventRangeSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<SessionSourceRefSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_marker: Option<String>,
    pub event_seq: u64,
}

fn required_text(field: &str, value: String, max_bytes: usize) -> AgenkitResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AgenkitError::validation(format!("{field} is required")));
    }
    if trimmed.len() > max_bytes {
        return Err(AgenkitError::validation(format!(
            "{field} must be at most {max_bytes} bytes"
        )));
    }
    Ok(redact_text_to_limit(trimmed, max_bytes))
}

fn add_range_source_refs(
    source_refs: &mut Vec<SessionSourceRef>,
    thread_id: &str,
    covered_events: Option<&SessionEventRange>,
    covered_records: Option<&SessionEventRange>,
) {
    if let Some(range) = covered_events {
        push_unique_ref(
            source_refs,
            SessionSourceRef::EventRange {
                start_seq: range.start_seq,
                end_seq: range.end_seq,
            },
        );
    }
    if let Some(range) = covered_records {
        push_unique_ref(
            source_refs,
            SessionSourceRef::RecordRange {
                thread_id: thread_id.to_string(),
                start_seq: range.start_seq,
                end_seq: range.end_seq,
            },
        );
    }
}

fn push_unique_ref(source_refs: &mut Vec<SessionSourceRef>, source_ref: SessionSourceRef) {
    if !source_refs.iter().any(|existing| existing == &source_ref) {
        source_refs.push(source_ref);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session::common::{
        CurrentSessionContext, InMemorySessionMetadataStore, SessionEventFilter,
        SessionMetadataStore,
    };
    use agenkitty_core::{SessionIdentity, SessionStoreKind};
    use pocopine_agenkit_core::ToolSideEffectPolicy;

    fn identity() -> SessionIdentity {
        SessionIdentity {
            thread_id: "thread-1".to_string(),
            agent_id: "agent".to_string(),
            model: "local/default".to_string(),
            run_id: Some("run-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            principal_key: Some("local".to_string()),
            tool_ids: vec![SESSION_SUMMARY_TOOL_ID.to_string()],
            max_steps_per_turn: 8,
            capture_policy: "full".to_string(),
            transcript_store: SessionStoreKind::InMemory,
            metadata_store: SessionStoreKind::InMemory,
            created_at_ms: 1,
            last_active_at_ms: 1,
            project_id: Some("project".to_string()),
        }
    }

    #[tokio::test]
    async fn session_summary_persists_summary_and_event() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        let runtime = Arc::new(SessionRuntime::new(store.clone()));
        store.upsert_identity(identity()).await.unwrap();
        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "title": "Setup summary",
                    "body": "The session defined the source-ref spine.",
                    "covered_events": {"start_seq": 1, "end_seq": 3},
                    "source_refs": [{"kind": "path", "path": "src/tools/session/README.md"}]
                }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();

        let input: SessionSummaryInput = serde_json::from_value(args).unwrap();
        let output = SessionSummaryTool::new(runtime).run(input).await.unwrap();

        assert_eq!(output.summary_id, "summary-1");
        assert_eq!(output.event_seq, 1);
        assert_eq!(output.covered_events.unwrap().end_seq, 3);
        assert!(
            output
                .source_refs
                .contains(&SessionSourceRefSpec::EventRange {
                    start_seq: 1,
                    end_seq: 3
                })
        );
        assert_eq!(
            store
                .read_summary("thread-1", "summary-1")
                .await
                .unwrap()
                .unwrap()
                .body,
            "The session defined the source-ref spine."
        );
        let events = store
            .list_events(
                "thread-1",
                SessionEventFilter {
                    after_seq: None,
                    start_seq: None,
                    end_seq: None,
                    limit: 10,
                    kinds: vec![SessionEventKind::SummaryCreated],
                },
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].policy, Some(SessionEventPolicy::SideEffecting));
        assert_eq!(
            events[0].payload.as_ref().unwrap()["summary_id"],
            serde_json::json!("summary-1")
        );
    }

    #[tokio::test]
    async fn session_summary_requires_a_covered_range() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "title": "Setup summary",
                    "body": "Body"
                }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let err = SessionSummaryTool::new(runtime)
            .run(serde_json::from_value(args).unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[tokio::test]
    async fn session_summary_rejects_missing_context() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let err = SessionSummaryTool::new(runtime)
            .run(SessionSummaryInput {
                title: "Setup summary".to_string(),
                body: "Body".to_string(),
                covered_events: Some(SessionEventRangeSpec {
                    start_seq: 1,
                    end_seq: 2,
                }),
                covered_records: None,
                source_refs: Vec::new(),
                produced_by: None,
                source_marker: None,
                context_token: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[test]
    fn session_summary_descriptor_is_side_effecting() {
        assert_eq!(
            SessionSummaryTool::descriptor().side_effect,
            ToolSideEffectPolicy::SideEffecting
        );
    }
}
