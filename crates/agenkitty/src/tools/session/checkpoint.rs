use std::sync::Arc;

use agenkitty_core::{
    SessionCheckpoint, SessionCheckpointKind, SessionEvent, SessionEventKind, SessionEventPolicy,
    SessionEventRange, SessionSourceRef,
};
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{SessionRuntime, current_time_ms, redact_text_to_limit};
use super::source_ref::{
    SessionEventRangeSpec, SessionSourceRefSpec, validate_event_range, validate_source_refs,
};

pub const SESSION_CHECKPOINT_TOOL_ID: &str = "session.checkpoint";

const MAX_CHECKPOINT_NAME_BYTES: usize = 256;
const MAX_CHECKPOINT_SUMMARY_BYTES: usize = 16 * 1024;
const MAX_CHECKPOINT_SUMMARY_ID_BYTES: usize = 256;

#[derive(Clone)]
pub struct SessionCheckpointTool {
    runtime: Arc<SessionRuntime>,
}

impl SessionCheckpointTool {
    pub fn new(runtime: Arc<SessionRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(
        &self,
        input: SessionCheckpointInput,
    ) -> AgenkitResult<SessionCheckpointOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let name = required_text(
            "session.checkpoint name",
            input.name,
            MAX_CHECKPOINT_NAME_BYTES,
        )?;
        let kind = input.kind.unwrap_or_default().into();
        let covered_events = input
            .covered_events
            .map(|range| validate_event_range(SESSION_CHECKPOINT_TOOL_ID, "covered_events", range))
            .transpose()?;
        let covered_records = input
            .covered_records
            .map(|range| validate_event_range(SESSION_CHECKPOINT_TOOL_ID, "covered_records", range))
            .transpose()?;
        if covered_events.is_none() && covered_records.is_none() && input.source_refs.is_empty() {
            return Err(AgenkitError::validation(
                "session.checkpoint requires source_refs, covered_events, or covered_records",
            ));
        }
        let summary_id = input
            .summary_id
            .map(|value| {
                required_text(
                    "session.checkpoint summary_id",
                    value,
                    MAX_CHECKPOINT_SUMMARY_ID_BYTES,
                )
            })
            .transpose()?;
        let summary = input
            .summary
            .map(|value| {
                required_text(
                    "session.checkpoint summary",
                    value,
                    MAX_CHECKPOINT_SUMMARY_BYTES,
                )
            })
            .transpose()?;
        let mut source_refs = if input.source_refs.is_empty() {
            Vec::new()
        } else {
            validate_source_refs(SESSION_CHECKPOINT_TOOL_ID, input.source_refs)?
        };
        add_range_source_refs(
            &mut source_refs,
            &context.identity.thread_id,
            covered_events.as_ref(),
            covered_records.as_ref(),
        );
        let timestamp_ms = current_time_ms();

        let checkpoint = self
            .runtime
            .store()
            .write_checkpoint(
                &context.identity.thread_id,
                SessionCheckpoint {
                    id: String::new(),
                    name: Some(name),
                    kind,
                    covered_events: covered_events.clone(),
                    covered_records: covered_records.clone(),
                    summary_id,
                    summary,
                    source_refs,
                    created_at_ms: timestamp_ms,
                },
            )
            .await?;

        let mut event = SessionEvent::new(SessionEventKind::CheckpointCreated, timestamp_ms)
            .with_message(format!(
                "session checkpoint created: {}",
                checkpoint.name.as_deref().unwrap_or("unnamed")
            ))
            .with_tool(SESSION_CHECKPOINT_TOOL_ID)
            .with_policy(SessionEventPolicy::SideEffecting)
            .with_payload(serde_json::json!({
                "checkpoint_id": checkpoint.id,
                "name": checkpoint.name,
                "kind": checkpoint_kind_name(checkpoint.kind),
                "summary_id": checkpoint.summary_id,
                "filesystem_rollback": false,
            }))
            .with_source_ref(SessionSourceRef::Tool {
                tool_id: SESSION_CHECKPOINT_TOOL_ID.to_string(),
            })
            .with_source_ref(SessionSourceRef::Checkpoint {
                checkpoint_id: checkpoint.id.clone(),
            });
        for source_ref in checkpoint.source_refs.clone() {
            event = event.with_source_ref(source_ref);
        }
        let event = self
            .runtime
            .store()
            .append_event(&context.identity.thread_id, event)
            .await?;

        Ok(SessionCheckpointOutput {
            thread_id: context.identity.thread_id,
            checkpoint_id: checkpoint.id,
            name: checkpoint.name.unwrap_or_default(),
            kind: checkpoint.kind.into(),
            covered_events: checkpoint.covered_events.map(Into::into),
            covered_records: checkpoint.covered_records.map(Into::into),
            summary_id: checkpoint.summary_id,
            summary: checkpoint.summary,
            source_refs: checkpoint.source_refs.into_iter().map(Into::into).collect(),
            event_seq: event.seq,
            created_at_ms: checkpoint.created_at_ms,
            filesystem_rollback: false,
        })
    }
}

impl AiTool for SessionCheckpointTool {
    const ID: &'static str = SESSION_CHECKPOINT_TOOL_ID;
    type Input = SessionCheckpointInput;
    type Output = SessionCheckpointOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SESSION_CHECKPOINT_TOOL_ID,
            "Record a named logical session checkpoint; this does not snapshot or roll back files.",
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
pub struct SessionCheckpointInput {
    pub name: String,
    #[serde(default)]
    pub kind: Option<SessionCheckpointKindSpec>,
    #[serde(default)]
    pub covered_events: Option<SessionEventRangeSpec>,
    #[serde(default)]
    pub covered_records: Option<SessionEventRangeSpec>,
    #[serde(default)]
    pub summary_id: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRefSpec>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SessionCheckpointOutput {
    pub thread_id: String,
    pub checkpoint_id: String,
    pub name: String,
    pub kind: SessionCheckpointKindSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_events: Option<SessionEventRangeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_records: Option<SessionEventRangeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<SessionSourceRefSpec>,
    pub event_seq: u64,
    pub created_at_ms: u64,
    pub filesystem_rollback: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionCheckpointKindSpec {
    AutoCompaction,
    #[default]
    Manual,
    PreTool,
    PostTool,
    HostImport,
    WorkspaceMarker,
}

impl From<SessionCheckpointKindSpec> for SessionCheckpointKind {
    fn from(value: SessionCheckpointKindSpec) -> Self {
        match value {
            SessionCheckpointKindSpec::AutoCompaction => Self::AutoCompaction,
            SessionCheckpointKindSpec::Manual => Self::Manual,
            SessionCheckpointKindSpec::PreTool => Self::PreTool,
            SessionCheckpointKindSpec::PostTool => Self::PostTool,
            SessionCheckpointKindSpec::HostImport => Self::HostImport,
            SessionCheckpointKindSpec::WorkspaceMarker => Self::WorkspaceMarker,
        }
    }
}

impl From<SessionCheckpointKind> for SessionCheckpointKindSpec {
    fn from(value: SessionCheckpointKind) -> Self {
        match value {
            SessionCheckpointKind::AutoCompaction => Self::AutoCompaction,
            SessionCheckpointKind::Manual => Self::Manual,
            SessionCheckpointKind::PreTool => Self::PreTool,
            SessionCheckpointKind::PostTool => Self::PostTool,
            SessionCheckpointKind::HostImport => Self::HostImport,
            SessionCheckpointKind::WorkspaceMarker => Self::WorkspaceMarker,
        }
    }
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

fn checkpoint_kind_name(kind: SessionCheckpointKind) -> &'static str {
    match kind {
        SessionCheckpointKind::AutoCompaction => "auto_compaction",
        SessionCheckpointKind::Manual => "manual",
        SessionCheckpointKind::PreTool => "pre_tool",
        SessionCheckpointKind::PostTool => "post_tool",
        SessionCheckpointKind::HostImport => "host_import",
        SessionCheckpointKind::WorkspaceMarker => "workspace_marker",
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
            tool_ids: vec![SESSION_CHECKPOINT_TOOL_ID.to_string()],
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
    async fn session_checkpoint_records_logical_checkpoint_and_event() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        let runtime = Arc::new(SessionRuntime::new(store.clone()));
        store.upsert_identity(identity()).await.unwrap();
        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "name": "before patch",
                    "kind": "manual",
                    "covered_events": {"start_seq": 1, "end_seq": 2},
                    "summary": "Before applying the patch tool.",
                    "source_refs": [{"kind": "tool", "tool_id": "patch.apply"}]
                }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();

        let input: SessionCheckpointInput = serde_json::from_value(args).unwrap();
        let output = SessionCheckpointTool::new(runtime)
            .run(input)
            .await
            .unwrap();

        assert_eq!(output.checkpoint_id, "checkpoint-1");
        assert_eq!(output.name, "before patch");
        assert_eq!(output.kind, SessionCheckpointKindSpec::Manual);
        assert_eq!(output.event_seq, 1);
        assert!(!output.filesystem_rollback);
        assert!(
            output
                .source_refs
                .contains(&SessionSourceRefSpec::EventRange {
                    start_seq: 1,
                    end_seq: 2
                })
        );
        let checkpoints = store.list_checkpoints("thread-1").await.unwrap();
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(
            checkpoints[0].summary.as_deref(),
            Some("Before applying the patch tool.")
        );
        let events = store
            .list_events(
                "thread-1",
                SessionEventFilter {
                    after_seq: None,
                    start_seq: None,
                    end_seq: None,
                    limit: 10,
                    kinds: vec![SessionEventKind::CheckpointCreated],
                },
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].policy, Some(SessionEventPolicy::SideEffecting));
        assert_eq!(
            events[0].payload.as_ref().unwrap()["filesystem_rollback"],
            serde_json::json!(false)
        );
    }

    #[tokio::test]
    async fn session_checkpoint_requires_name_and_source() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "name": "",
                    "covered_events": {"start_seq": 1, "end_seq": 2}
                }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let err = SessionCheckpointTool::new(runtime.clone())
            .run(serde_json::from_value(args).unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");

        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "name": "empty"
                }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let err = SessionCheckpointTool::new(runtime)
            .run(serde_json::from_value(args).unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[tokio::test]
    async fn session_checkpoint_rejects_missing_context() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let err = SessionCheckpointTool::new(runtime)
            .run(SessionCheckpointInput {
                name: "before patch".to_string(),
                kind: Some(SessionCheckpointKindSpec::Manual),
                covered_events: Some(SessionEventRangeSpec {
                    start_seq: 1,
                    end_seq: 2,
                }),
                covered_records: None,
                summary_id: None,
                summary: None,
                source_refs: Vec::new(),
                context_token: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[test]
    fn session_checkpoint_descriptor_is_side_effecting() {
        assert_eq!(
            SessionCheckpointTool::descriptor().side_effect,
            ToolSideEffectPolicy::SideEffecting
        );
    }
}
