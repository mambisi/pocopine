use std::sync::Arc;

use agenkitty_core::{
    SessionEvent, SessionEventKind, SessionEventPolicy, SessionNote, SessionSourceRef,
};
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{SessionRuntime, current_time_ms, redact_text_to_limit};
use super::source_ref::{SessionSourceRefSpec, validate_source_refs};

pub const SESSION_NOTE_TOOL_ID: &str = "session.note";

const MAX_NOTE_TITLE_BYTES: usize = 256;
const MAX_NOTE_BODY_BYTES: usize = 16 * 1024;
const MAX_NOTE_REASON_BYTES: usize = 1024;
const MAX_NOTE_TAG_BYTES: usize = 64;
const MAX_NOTE_TAGS: usize = 16;

#[derive(Clone)]
pub struct SessionNoteTool {
    runtime: Arc<SessionRuntime>,
}

impl SessionNoteTool {
    pub fn new(runtime: Arc<SessionRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: SessionNoteInput) -> AgenkitResult<SessionNoteOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let title = required_text("session.note title", input.title, MAX_NOTE_TITLE_BYTES)?;
        let body = required_text("session.note body", input.body, MAX_NOTE_BODY_BYTES)?;
        let reason = required_text("session.note reason", input.reason, MAX_NOTE_REASON_BYTES)?;
        let source_refs = validate_source_refs(SESSION_NOTE_TOOL_ID, input.source_refs)?;
        let tags = normalize_tags(input.tags)?;
        let timestamp_ms = current_time_ms();

        let note = self
            .runtime
            .store()
            .append_note(
                &context.identity.thread_id,
                SessionNote {
                    id: String::new(),
                    title,
                    body,
                    tags,
                    reason,
                    source_refs,
                    created_at_ms: timestamp_ms,
                    updated_at_ms: timestamp_ms,
                },
            )
            .await?;

        let mut event = SessionEvent::new(SessionEventKind::NoteCreated, timestamp_ms)
            .with_message(format!("session note created: {}", note.title))
            .with_tool(SESSION_NOTE_TOOL_ID)
            .with_policy(SessionEventPolicy::SideEffecting)
            .with_payload(serde_json::json!({
                "note_id": note.id,
                "title": note.title,
                "tags": note.tags,
            }))
            .with_source_ref(SessionSourceRef::Tool {
                tool_id: SESSION_NOTE_TOOL_ID.to_string(),
            });
        for source_ref in note.source_refs.clone() {
            event = event.with_source_ref(source_ref);
        }
        let event = self
            .runtime
            .store()
            .append_event(&context.identity.thread_id, event)
            .await?;

        Ok(SessionNoteOutput {
            thread_id: context.identity.thread_id,
            note_id: note.id,
            title: note.title,
            body: note.body,
            tags: note.tags,
            reason: note.reason,
            source_refs: note.source_refs.into_iter().map(Into::into).collect(),
            event_seq: event.seq,
            created_at_ms: note.created_at_ms,
        })
    }
}

impl AiTool for SessionNoteTool {
    const ID: &'static str = SESSION_NOTE_TOOL_ID;
    type Input = SessionNoteInput;
    type Output = SessionNoteOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SESSION_NOTE_TOOL_ID,
            "Attach a bounded, session-scoped note with a reason and explicit source references.",
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
pub struct SessionNoteInput {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub reason: String,
    #[serde(default)]
    pub source_refs: Vec<SessionSourceRefSpec>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SessionNoteOutput {
    pub thread_id: String,
    pub note_id: String,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<SessionSourceRefSpec>,
    pub event_seq: u64,
    pub created_at_ms: u64,
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

fn normalize_tags(tags: Vec<String>) -> AgenkitResult<Vec<String>> {
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        if tag.len() > MAX_NOTE_TAG_BYTES {
            return Err(AgenkitError::validation(format!(
                "session.note tag must be at most {MAX_NOTE_TAG_BYTES} bytes"
            )));
        }
        if !normalized.iter().any(|existing| existing == tag) {
            normalized.push(tag.to_string());
        }
        if normalized.len() > MAX_NOTE_TAGS {
            return Err(AgenkitError::validation(format!(
                "session.note accepts at most {MAX_NOTE_TAGS} tags"
            )));
        }
    }
    Ok(normalized)
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
            tool_ids: vec![SESSION_NOTE_TOOL_ID.to_string()],
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
    async fn session_note_appends_note_and_event() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        let runtime = Arc::new(SessionRuntime::new(store.clone()));
        store.upsert_identity(identity()).await.unwrap();
        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "title": "Architecture decision",
                    "body": "Build session source refs before memory.",
                    "tags": ["memory", "memory"],
                    "reason": "This decision affects the next tool.",
                    "source_refs": [{"kind": "event", "seq": 1}]
                }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();

        let input: SessionNoteInput = serde_json::from_value(args).unwrap();
        let output = SessionNoteTool::new(runtime).run(input).await.unwrap();

        assert_eq!(output.note_id, "note-1");
        assert_eq!(output.tags, vec!["memory".to_string()]);
        assert_eq!(output.event_seq, 1);
        assert!(output.source_refs.contains(&SessionSourceRefSpec::Thread {
            thread_id: "thread-1".to_string()
        }));
        assert_eq!(
            store
                .read_note("thread-1", "note-1")
                .await
                .unwrap()
                .unwrap()
                .body,
            "Build session source refs before memory."
        );
        let events = store
            .list_events(
                "thread-1",
                SessionEventFilter {
                    after_seq: None,
                    start_seq: None,
                    end_seq: None,
                    limit: 10,
                    kinds: vec![SessionEventKind::NoteCreated],
                },
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].policy, Some(SessionEventPolicy::SideEffecting));
        assert_eq!(
            events[0].payload.as_ref().unwrap()["note_id"],
            serde_json::json!("note-1")
        );
    }

    #[tokio::test]
    async fn session_note_requires_body_reason_and_source_refs() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "title": "Decision",
                    "body": "",
                    "reason": "Needed for memory.",
                    "source_refs": [{"kind": "event", "seq": 1}]
                }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let err = SessionNoteTool::new(runtime.clone())
            .run(serde_json::from_value(args).unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");

        let args = runtime
            .inject_context_args(
                &serde_json::json!({
                    "title": "Decision",
                    "body": "Body",
                    "reason": "Needed for memory.",
                    "source_refs": []
                }),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let err = SessionNoteTool::new(runtime)
            .run(serde_json::from_value(args).unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[tokio::test]
    async fn session_note_rejects_missing_context() {
        let runtime = Arc::new(SessionRuntime::in_memory());
        let err = SessionNoteTool::new(runtime)
            .run(SessionNoteInput {
                title: "Decision".to_string(),
                body: "Body".to_string(),
                tags: Vec::new(),
                reason: "Reason".to_string(),
                source_refs: vec![SessionSourceRefSpec::Event { seq: 1 }],
                context_token: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[test]
    fn session_note_descriptor_is_side_effecting() {
        assert_eq!(
            SessionNoteTool::descriptor().side_effect,
            ToolSideEffectPolicy::SideEffecting
        );
    }
}
