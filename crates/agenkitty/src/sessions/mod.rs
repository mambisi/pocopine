use std::sync::Arc;

pub use agenkitty_core::sessions::*;
use pocopine_agenkit::server::AgentSession;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};

use crate::tools::session::{
    SessionEventFilter, SessionMetadataStore, current_time_ms, redact_artifact_link,
    redact_closure, redact_json_value, redact_text_to_limit,
};

pub const DEFAULT_SESSION_EXPORT_EVENT_LIMIT: usize = 1_000;

#[derive(Clone)]
pub struct SessionHost {
    store: Arc<dyn SessionMetadataStore>,
    max_export_events: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionListFilter {
    pub principal_key: Option<String>,
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionListEntry {
    pub identity: SessionIdentity,
    pub event_count: u64,
    pub closed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionExportRedaction {
    #[default]
    Redacted,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionExportOptions {
    pub redaction: SessionExportRedaction,
    pub max_events: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCloseResult {
    pub closure: SessionClosure,
    pub already_closed: bool,
}

impl Default for SessionExportOptions {
    fn default() -> Self {
        Self {
            redaction: SessionExportRedaction::Redacted,
            max_events: DEFAULT_SESSION_EXPORT_EVENT_LIMIT,
        }
    }
}

impl SessionHost {
    pub fn new(store: Arc<dyn SessionMetadataStore>) -> Self {
        Self {
            store,
            max_export_events: DEFAULT_SESSION_EXPORT_EVENT_LIMIT,
        }
    }

    pub fn with_max_export_events(mut self, max_export_events: usize) -> Self {
        self.max_export_events = max_export_events.max(1);
        self
    }

    pub fn store(&self) -> &Arc<dyn SessionMetadataStore> {
        &self.store
    }

    pub async fn list(&self, filter: SessionListFilter) -> AgenkitResult<Vec<SessionListEntry>> {
        let mut identities = self.store.list_identities().await?;
        identities.retain(|identity| {
            filter
                .principal_key
                .as_ref()
                .is_none_or(|principal| identity.principal_key.as_ref() == Some(principal))
                && filter
                    .project_id
                    .as_ref()
                    .is_none_or(|project| identity.project_id.as_ref() == Some(project))
        });
        identities.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));

        let mut entries = Vec::with_capacity(identities.len());
        for identity in identities {
            let event_count = self.store.event_count(&identity.thread_id).await?;
            let closed = self.store.closure(&identity.thread_id).await?.is_some();
            entries.push(SessionListEntry {
                identity,
                event_count,
                closed,
            });
        }
        Ok(entries)
    }

    pub async fn open(&self, thread_id: &str) -> AgenkitResult<Option<SessionIdentity>> {
        self.store.identity(thread_id).await
    }

    pub async fn export(
        &self,
        thread_id: &str,
        options: SessionExportOptions,
    ) -> AgenkitResult<SessionExport> {
        if options.max_events == 0 {
            return Err(AgenkitError::validation(
                "session export max_events must be greater than zero",
            ));
        }
        let identity = self
            .open(thread_id)
            .await?
            .ok_or_else(|| AgenkitError::not_found(format!("session `{thread_id}` not found")))?;
        let total_events = self.store.event_count(thread_id).await?;
        let limit = options.max_events.min(self.max_export_events);
        let start_seq = if total_events > limit as u64 {
            Some(total_events - limit as u64 + 1)
        } else {
            None
        };
        let mut events = self
            .store
            .list_events(
                thread_id,
                SessionEventFilter {
                    after_seq: None,
                    start_seq,
                    end_seq: None,
                    limit,
                    kinds: Vec::new(),
                },
            )
            .await?;
        let mut notes = self.store.list_notes(thread_id).await?;
        let mut summaries = self.store.list_summaries(thread_id).await?;
        let mut checkpoints = self.store.list_checkpoints(thread_id).await?;
        let mut artifact_links = self.store.list_artifact_links(thread_id).await?;
        let mut closure = self.store.closure(thread_id).await?;
        let identity = match options.redaction {
            SessionExportRedaction::Redacted => {
                events = events.into_iter().map(redact_event).collect();
                notes = notes.into_iter().map(redact_note).collect();
                summaries = summaries.into_iter().map(redact_summary).collect();
                checkpoints = checkpoints.into_iter().map(redact_checkpoint).collect();
                artifact_links = artifact_links
                    .into_iter()
                    .map(redact_artifact_link)
                    .collect();
                closure = closure.map(redact_closure);
                redact_identity(identity)
            }
            SessionExportRedaction::Full => identity,
        };

        Ok(SessionExport {
            identity: Some(identity),
            total_events,
            events_truncated: total_events > events.len() as u64,
            events,
            notes,
            summaries,
            checkpoints,
            artifact_links,
            closure,
        })
    }

    pub async fn close(
        &self,
        thread_id: &str,
        reason: Option<String>,
        source_refs: Vec<SessionSourceRef>,
    ) -> AgenkitResult<SessionCloseResult> {
        self.open(thread_id)
            .await?
            .ok_or_else(|| AgenkitError::not_found(format!("session `{thread_id}` not found")))?;
        let closed = self
            .store
            .close_session(
                thread_id,
                SessionClosure {
                    closed_at_ms: current_time_ms(),
                    reason: reason.map(|reason| redact_text_to_limit(&reason, 1024)),
                    source_refs,
                },
            )
            .await?;
        Ok(SessionCloseResult {
            closure: closed.closure,
            already_closed: closed.already_closed,
        })
    }

    pub async fn fork_live_session(
        &self,
        session: &AgentSession,
        source_identity: &SessionIdentity,
    ) -> AgenkitResult<Option<SessionIdentity>> {
        let Some(forked) = session.fork().await? else {
            return Ok(None);
        };
        let now = current_time_ms();
        let mut identity = source_identity.clone();
        identity.thread_id = forked.id().as_str().to_string();
        identity.run_id = None;
        identity.turn_id = None;
        identity.metadata_store = self.store.kind();
        identity.created_at_ms = now;
        identity.last_active_at_ms = now;
        self.store
            .append_event(
                &identity.thread_id,
                SessionEvent::new(SessionEventKind::Started, now)
                    .with_message(format!("forked from {}", source_identity.thread_id))
                    .with_source_ref(SessionSourceRef::Thread {
                        thread_id: source_identity.thread_id.clone(),
                    }),
            )
            .await?;
        identity = self.store.upsert_identity(identity).await?;
        Ok(Some(identity))
    }
}

fn redact_identity(mut identity: SessionIdentity) -> SessionIdentity {
    if identity.principal_key.is_some() {
        identity.principal_key = Some("[redacted]".to_string());
    }
    identity
}

fn redact_event(mut event: SessionEvent) -> SessionEvent {
    event.message = event
        .message
        .map(|message| redact_text_to_limit(&message, 4096));
    event.payload = event
        .payload
        .as_ref()
        .map(|payload| redact_json_value(payload, 2048));
    event
}

fn redact_note(mut note: SessionNote) -> SessionNote {
    note.title = redact_text_to_limit(&note.title, 512);
    note.body = redact_text_to_limit(&note.body, 4096);
    note.reason = redact_text_to_limit(&note.reason, 1024);
    note.tags = note
        .tags
        .into_iter()
        .map(|tag| redact_text_to_limit(&tag, 128))
        .collect();
    note
}

fn redact_summary(mut summary: SessionSummary) -> SessionSummary {
    summary.title = redact_text_to_limit(&summary.title, 512);
    summary.body = redact_text_to_limit(&summary.body, 8192);
    summary.source_marker = summary
        .source_marker
        .map(|marker| redact_text_to_limit(&marker, 512));
    summary
}

fn redact_checkpoint(mut checkpoint: SessionCheckpoint) -> SessionCheckpoint {
    checkpoint.name = checkpoint.name.map(|name| redact_text_to_limit(&name, 512));
    checkpoint.summary = checkpoint
        .summary
        .map(|summary| redact_text_to_limit(&summary, 4096));
    checkpoint
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session::InMemorySessionMetadataStore;
    use agenkitty_core::SessionArtifactLink;
    use futures::StreamExt;
    use pocopine_agenkit::prelude::{Agenkit, ModelRef};
    use pocopine_agenkit::server::{AgentConfig, AuthUser, MockProvider, Principal};

    fn identity(thread_id: &str, principal_key: &str, project_id: Option<&str>) -> SessionIdentity {
        SessionIdentity {
            thread_id: thread_id.to_string(),
            agent_id: "agent".to_string(),
            model: "local/default".to_string(),
            run_id: None,
            turn_id: None,
            principal_key: Some(principal_key.to_string()),
            tool_ids: vec!["session.info".to_string()],
            max_steps_per_turn: 8,
            capture_policy: "full".to_string(),
            transcript_store: SessionStoreKind::InMemory,
            metadata_store: SessionStoreKind::InMemory,
            created_at_ms: 1,
            last_active_at_ms: 1,
            project_id: project_id.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn host_list_respects_owner_and_project_filters() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        store
            .upsert_identity(identity("thread-1", "alice", Some("project-a")))
            .await
            .unwrap();
        store
            .upsert_identity(identity("thread-2", "bob", Some("project-a")))
            .await
            .unwrap();
        store
            .upsert_identity(identity("thread-3", "alice", Some("project-b")))
            .await
            .unwrap();
        store
            .append_event("thread-1", SessionEvent::new(SessionEventKind::Started, 1))
            .await
            .unwrap();

        let host = SessionHost::new(store);
        let entries = host
            .list(SessionListFilter {
                principal_key: Some("alice".to_string()),
                project_id: Some("project-a".to_string()),
            })
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].identity.thread_id, "thread-1");
        assert_eq!(entries[0].event_count, 1);
        assert!(!entries[0].closed);
    }

    #[tokio::test]
    async fn host_export_is_bounded_and_redacted_by_default() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        store
            .upsert_identity(identity("thread-1", "alice", None))
            .await
            .unwrap();
        store
            .append_event(
                "thread-1",
                SessionEvent::new(SessionEventKind::AssistantText, 1)
                    .with_message("api_key=secret")
                    .with_payload(serde_json::json!({"password": "secret", "safe": "ok"})),
            )
            .await
            .unwrap();
        store
            .append_event("thread-1", SessionEvent::new(SessionEventKind::Stopped, 2))
            .await
            .unwrap();
        store
            .link_artifact(
                "thread-1",
                SessionArtifactLink {
                    artifact_id: "api_key=artifact-secret".to_string(),
                    source_refs: vec![SessionSourceRef::Path {
                        path: "src/secrets/config.rs".to_string(),
                    }],
                    promotion_policy: Some("token: promote-secret".to_string()),
                    created_at_ms: 3,
                },
            )
            .await
            .unwrap();
        store
            .close_session(
                "thread-1",
                SessionClosure {
                    closed_at_ms: 5,
                    reason: Some("authorization: secret".to_string()),
                    source_refs: Vec::new(),
                },
            )
            .await
            .unwrap();

        let host = SessionHost::new(store);
        let export = host
            .export("thread-1", SessionExportOptions::default())
            .await
            .unwrap();

        assert_eq!(export.total_events, 2);
        assert!(!export.events_truncated);
        assert_eq!(export.events.len(), 2);
        assert_eq!(
            export.identity.unwrap().principal_key.as_deref(),
            Some("[redacted]")
        );
        assert_eq!(export.events[0].message.as_deref(), Some("[redacted]"));
        assert_eq!(
            export.events[0].payload.as_ref().unwrap()["password"],
            "[redacted]"
        );
        assert_eq!(export.artifact_links[0].artifact_id, "[redacted]");
        assert_eq!(
            export.artifact_links[0].promotion_policy.as_deref(),
            Some("[redacted]")
        );
        assert_eq!(
            export.artifact_links[0].source_refs[0],
            SessionSourceRef::Path {
                path: "src/secrets/config.rs".to_string()
            }
        );
        assert_eq!(
            export.closure.unwrap().reason.as_deref(),
            Some("[redacted]")
        );
    }

    #[tokio::test]
    async fn host_export_returns_newest_events_with_truncation_signal() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        store
            .upsert_identity(identity("thread-1", "alice", None))
            .await
            .unwrap();
        for seq in 1..=3 {
            store
                .append_event(
                    "thread-1",
                    SessionEvent::new(SessionEventKind::AssistantText, seq)
                        .with_message(format!("event {seq}")),
                )
                .await
                .unwrap();
        }

        let export = SessionHost::new(store)
            .with_max_export_events(2)
            .export("thread-1", SessionExportOptions::default())
            .await
            .unwrap();

        assert_eq!(export.total_events, 3);
        assert!(export.events_truncated);
        assert_eq!(
            export
                .events
                .iter()
                .map(|event| event.message.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("event 2"), Some("event 3")]
        );
    }

    #[tokio::test]
    async fn host_close_is_idempotent() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        store
            .upsert_identity(identity("thread-1", "alice", None))
            .await
            .unwrap();
        let host = SessionHost::new(store);

        let first = host
            .close("thread-1", Some("done".to_string()), Vec::new())
            .await
            .unwrap();
        let second = host
            .close("thread-1", Some("different".to_string()), Vec::new())
            .await
            .unwrap();

        assert!(!first.already_closed);
        assert!(second.already_closed);
        assert_eq!(first.closure, second.closure);
    }

    #[tokio::test]
    async fn host_concurrent_close_reports_one_new_closure() {
        let store = Arc::new(InMemorySessionMetadataStore::new());
        store
            .upsert_identity(identity("thread-1", "alice", None))
            .await
            .unwrap();
        let host = SessionHost::new(store);
        let first_host = host.clone();
        let second_host = host.clone();

        let (first, second) = tokio::join!(
            first_host.close("thread-1", Some("first".to_string()), Vec::new()),
            second_host.close("thread-1", Some("second".to_string()), Vec::new())
        );
        let first = first.unwrap();
        let second = second.unwrap();

        assert_eq!(
            [first.already_closed, second.already_closed]
                .into_iter()
                .filter(|already_closed| !already_closed)
                .count(),
            1
        );
        assert_eq!(first.closure, second.closure);
    }

    #[tokio::test]
    async fn host_fork_live_session_creates_child_metadata() {
        let agenkit = Agenkit::builder()
            .provider(MockProvider::new("local").default_text("hello"))
            .default_model(ModelRef::new("local/default"))
            .build()
            .unwrap();
        let session = AgentSession::builder(&agenkit)
            .agent_id("agent")
            .principal(Principal::from_user(AuthUser::new("alice")))
            .config(
                AgentConfig::new()
                    .model(ModelRef::new("local/default"))
                    .system("Answer briefly."),
            )
            .open(None)
            .await
            .unwrap();
        let _ = session.prompt("first").collect::<Vec<_>>().await;
        let store = Arc::new(InMemorySessionMetadataStore::new());
        let source_identity = identity(session.id().as_str(), "alice", None);
        store
            .upsert_identity(source_identity.clone())
            .await
            .unwrap();
        let host = SessionHost::new(store.clone());

        let forked = host
            .fork_live_session(&session, &source_identity)
            .await
            .unwrap()
            .expect("default store supports forks");

        assert_ne!(forked.thread_id, source_identity.thread_id);
        assert_eq!(forked.metadata_store, SessionStoreKind::InMemory);
        assert_eq!(
            store
                .identity(&forked.thread_id)
                .await
                .unwrap()
                .unwrap()
                .agent_id,
            "agent"
        );
        let events = store
            .list_events(
                &forked.thread_id,
                SessionEventFilter {
                    after_seq: None,
                    start_seq: None,
                    end_seq: None,
                    limit: 10,
                    kinds: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].source_refs.contains(&SessionSourceRef::Thread {
            thread_id: source_identity.thread_id
        }));
        assert!(events[0].source_refs.contains(&SessionSourceRef::Thread {
            thread_id: forked.thread_id
        }));
    }
}
