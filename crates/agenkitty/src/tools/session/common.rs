use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use agenkitty_core::{
    FrameworkEvent, FrameworkEventKind, RunStatus, SessionArtifactLink, SessionCheckpoint,
    SessionClosure, SessionEvent, SessionEventKind, SessionEventPolicy, SessionIdentity,
    SessionNote, SessionSourceRef, SessionStoreKind, SessionSummary,
};
use pocopine_agenkit::server::session::ThreadId;
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use serde_json::{Map, Value};

pub type SessionMetadataFuture<'a, T> = Pin<Box<dyn Future<Output = AgenkitResult<T>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMetadataCloseResult {
    pub closure: SessionClosure,
    pub already_closed: bool,
}

#[derive(Clone, Debug)]
pub struct CurrentSessionContext {
    pub identity: SessionIdentity,
}

impl CurrentSessionContext {
    pub fn thread_ref(&self) -> SessionSourceRef {
        SessionSourceRef::Thread {
            thread_id: self.identity.thread_id.clone(),
        }
    }

    pub fn event_ref(&self, seq: u64) -> SessionSourceRef {
        SessionSourceRef::Event { seq }
    }

    pub fn event_range_ref(&self, start_seq: u64, end_seq: u64) -> SessionSourceRef {
        SessionSourceRef::EventRange { start_seq, end_seq }
    }

    pub fn record_ref(&self, seq: u64) -> SessionSourceRef {
        SessionSourceRef::Record {
            thread_id: self.identity.thread_id.clone(),
            seq,
        }
    }

    pub fn record_range_ref(&self, start_seq: u64, end_seq: u64) -> SessionSourceRef {
        SessionSourceRef::RecordRange {
            thread_id: self.identity.thread_id.clone(),
            start_seq,
            end_seq,
        }
    }

    pub fn tool_ref(&self, tool_id: impl Into<String>) -> SessionSourceRef {
        SessionSourceRef::Tool {
            tool_id: tool_id.into(),
        }
    }

    pub fn tool_call_ref(
        &self,
        call_id: impl Into<String>,
        tool_id: impl Into<String>,
    ) -> SessionSourceRef {
        SessionSourceRef::ToolCall {
            call_id: call_id.into(),
            tool_id: tool_id.into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionEventFilter {
    pub after_seq: Option<u64>,
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
    pub limit: usize,
    pub kinds: Vec<SessionEventKind>,
}

pub trait SessionMetadataStore: Send + Sync + 'static {
    fn upsert_identity<'a>(
        &'a self,
        identity: SessionIdentity,
    ) -> SessionMetadataFuture<'a, SessionIdentity>;

    fn identity<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionIdentity>>;

    fn list_identities<'a>(&'a self) -> SessionMetadataFuture<'a, Vec<SessionIdentity>>;

    fn append_event<'a>(
        &'a self,
        thread_id: &'a str,
        event: SessionEvent,
    ) -> SessionMetadataFuture<'a, SessionEvent>;

    fn list_events<'a>(
        &'a self,
        thread_id: &'a str,
        filter: SessionEventFilter,
    ) -> SessionMetadataFuture<'a, Vec<SessionEvent>>;

    fn event_count<'a>(&'a self, thread_id: &'a str) -> SessionMetadataFuture<'a, u64>;

    fn append_note<'a>(
        &'a self,
        thread_id: &'a str,
        note: SessionNote,
    ) -> SessionMetadataFuture<'a, SessionNote>;

    fn read_note<'a>(
        &'a self,
        thread_id: &'a str,
        note_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionNote>>;

    fn list_notes<'a>(&'a self, thread_id: &'a str) -> SessionMetadataFuture<'a, Vec<SessionNote>>;

    fn write_summary<'a>(
        &'a self,
        thread_id: &'a str,
        summary: SessionSummary,
    ) -> SessionMetadataFuture<'a, SessionSummary>;

    fn read_summary<'a>(
        &'a self,
        thread_id: &'a str,
        summary_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionSummary>>;

    fn list_summaries<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionSummary>>;

    fn write_checkpoint<'a>(
        &'a self,
        thread_id: &'a str,
        checkpoint: SessionCheckpoint,
    ) -> SessionMetadataFuture<'a, SessionCheckpoint>;

    fn list_checkpoints<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionCheckpoint>>;

    fn link_artifact<'a>(
        &'a self,
        thread_id: &'a str,
        link: SessionArtifactLink,
    ) -> SessionMetadataFuture<'a, SessionArtifactLink>;

    fn list_artifact_links<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionArtifactLink>>;

    fn close_session<'a>(
        &'a self,
        thread_id: &'a str,
        closure: SessionClosure,
    ) -> SessionMetadataFuture<'a, SessionMetadataCloseResult>;

    fn closure<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionClosure>>;

    fn kind(&self) -> SessionStoreKind;
}

#[derive(Default)]
pub struct InMemorySessionMetadataStore {
    sessions: Mutex<HashMap<String, StoredSession>>,
}

#[derive(Default)]
struct StoredSession {
    identity: Option<SessionIdentity>,
    events: Vec<SessionEvent>,
    notes: Vec<SessionNote>,
    next_note_seq: u64,
    summaries: Vec<SessionSummary>,
    next_summary_seq: u64,
    checkpoints: Vec<SessionCheckpoint>,
    next_checkpoint_seq: u64,
    artifact_links: Vec<SessionArtifactLink>,
    closure: Option<SessionClosure>,
}

impl InMemorySessionMetadataStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionMetadataStore for InMemorySessionMetadataStore {
    fn upsert_identity<'a>(
        &'a self,
        identity: SessionIdentity,
    ) -> SessionMetadataFuture<'a, SessionIdentity> {
        Box::pin(async move {
            let mut sessions = self.sessions.lock().map_err(lock_err)?;
            let entry = sessions.entry(identity.thread_id.clone()).or_default();
            entry.identity = Some(identity.clone());
            Ok(identity)
        })
    }

    fn identity<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionIdentity>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions
                .get(thread_id)
                .and_then(|session| session.identity.clone()))
        })
    }

    fn list_identities<'a>(&'a self) -> SessionMetadataFuture<'a, Vec<SessionIdentity>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            let mut identities = sessions
                .values()
                .filter_map(|session| session.identity.clone())
                .collect::<Vec<_>>();
            identities.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
            Ok(identities)
        })
    }

    fn append_event<'a>(
        &'a self,
        thread_id: &'a str,
        mut event: SessionEvent,
    ) -> SessionMetadataFuture<'a, SessionEvent> {
        Box::pin(async move {
            let mut sessions = self.sessions.lock().map_err(lock_err)?;
            let entry = sessions.entry(thread_id.to_string()).or_default();
            event.seq = entry.events.last().map_or(1, |last| last.seq + 1);
            if !event.source_refs.iter().any(|source| {
                matches!(
                    source,
                    SessionSourceRef::Thread { thread_id: existing } if existing == thread_id
                )
            }) {
                event.source_refs.push(SessionSourceRef::Thread {
                    thread_id: thread_id.to_string(),
                });
            }
            entry.events.push(event.clone());
            Ok(event)
        })
    }

    fn list_events<'a>(
        &'a self,
        thread_id: &'a str,
        filter: SessionEventFilter,
    ) -> SessionMetadataFuture<'a, Vec<SessionEvent>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            let Some(session) = sessions.get(thread_id) else {
                return Ok(Vec::new());
            };
            let mut events = Vec::new();
            for event in &session.events {
                if filter.after_seq.is_some_and(|after| event.seq <= after) {
                    continue;
                }
                if filter.start_seq.is_some_and(|start| event.seq < start) {
                    continue;
                }
                if filter.end_seq.is_some_and(|end| event.seq > end) {
                    continue;
                }
                if !filter.kinds.is_empty() && !filter.kinds.contains(&event.kind) {
                    continue;
                }
                events.push(event.clone());
                if filter.limit > 0 && events.len() >= filter.limit {
                    break;
                }
            }
            Ok(events)
        })
    }

    fn event_count<'a>(&'a self, thread_id: &'a str) -> SessionMetadataFuture<'a, u64> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions
                .get(thread_id)
                .map_or(0, |session| session.events.len() as u64))
        })
    }

    fn append_note<'a>(
        &'a self,
        thread_id: &'a str,
        mut note: SessionNote,
    ) -> SessionMetadataFuture<'a, SessionNote> {
        Box::pin(async move {
            let mut sessions = self.sessions.lock().map_err(lock_err)?;
            let entry = sessions.entry(thread_id.to_string()).or_default();
            note = redact_note(note);
            if note.id.is_empty() {
                entry.next_note_seq = entry.next_note_seq.saturating_add(1);
                note.id = format!("note-{}", entry.next_note_seq);
            }
            if entry.notes.iter().any(|existing| existing.id == note.id) {
                return Err(AgenkitError::validation(format!(
                    "session note `{}` already exists",
                    note.id
                )));
            }
            if !note.source_refs.iter().any(|source| {
                matches!(
                    source,
                    SessionSourceRef::Thread { thread_id: existing } if existing == thread_id
                )
            }) {
                note.source_refs.push(SessionSourceRef::Thread {
                    thread_id: thread_id.to_string(),
                });
            }
            entry.notes.push(note.clone());
            Ok(note)
        })
    }

    fn read_note<'a>(
        &'a self,
        thread_id: &'a str,
        note_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionNote>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions.get(thread_id).and_then(|session| {
                session
                    .notes
                    .iter()
                    .find(|note| note.id == note_id)
                    .cloned()
            }))
        })
    }

    fn list_notes<'a>(&'a self, thread_id: &'a str) -> SessionMetadataFuture<'a, Vec<SessionNote>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions
                .get(thread_id)
                .map_or_else(Vec::new, |session| session.notes.clone()))
        })
    }

    fn write_summary<'a>(
        &'a self,
        thread_id: &'a str,
        mut summary: SessionSummary,
    ) -> SessionMetadataFuture<'a, SessionSummary> {
        Box::pin(async move {
            let mut sessions = self.sessions.lock().map_err(lock_err)?;
            let entry = sessions.entry(thread_id.to_string()).or_default();
            summary = redact_summary(summary);
            if summary.id.is_empty() {
                entry.next_summary_seq = entry.next_summary_seq.saturating_add(1);
                summary.id = format!("summary-{}", entry.next_summary_seq);
            }
            if let Some(position) = entry
                .summaries
                .iter()
                .position(|existing| existing.id == summary.id)
            {
                entry.summaries[position] = summary.clone();
            } else {
                entry.summaries.push(summary.clone());
            }
            if !summary.source_refs.iter().any(|source| {
                matches!(
                    source,
                    SessionSourceRef::Thread { thread_id: existing } if existing == thread_id
                )
            }) {
                summary.source_refs.push(SessionSourceRef::Thread {
                    thread_id: thread_id.to_string(),
                });
                if let Some(stored) = entry
                    .summaries
                    .iter_mut()
                    .find(|stored| stored.id == summary.id)
                {
                    stored.source_refs = summary.source_refs.clone();
                }
            }
            Ok(summary)
        })
    }

    fn read_summary<'a>(
        &'a self,
        thread_id: &'a str,
        summary_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionSummary>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions.get(thread_id).and_then(|session| {
                session
                    .summaries
                    .iter()
                    .find(|summary| summary.id == summary_id)
                    .cloned()
            }))
        })
    }

    fn list_summaries<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionSummary>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions
                .get(thread_id)
                .map_or_else(Vec::new, |session| session.summaries.clone()))
        })
    }

    fn write_checkpoint<'a>(
        &'a self,
        thread_id: &'a str,
        mut checkpoint: SessionCheckpoint,
    ) -> SessionMetadataFuture<'a, SessionCheckpoint> {
        Box::pin(async move {
            let mut sessions = self.sessions.lock().map_err(lock_err)?;
            let entry = sessions.entry(thread_id.to_string()).or_default();
            checkpoint = redact_checkpoint(checkpoint);
            if checkpoint.id.is_empty() {
                entry.next_checkpoint_seq = entry.next_checkpoint_seq.saturating_add(1);
                checkpoint.id = format!("checkpoint-{}", entry.next_checkpoint_seq);
            }
            if !checkpoint.source_refs.iter().any(|source| {
                matches!(
                    source,
                    SessionSourceRef::Thread { thread_id: existing } if existing == thread_id
                )
            }) {
                checkpoint.source_refs.push(SessionSourceRef::Thread {
                    thread_id: thread_id.to_string(),
                });
            }
            if let Some(position) = entry
                .checkpoints
                .iter()
                .position(|existing| existing.id == checkpoint.id)
            {
                entry.checkpoints[position] = checkpoint.clone();
            } else {
                entry.checkpoints.push(checkpoint.clone());
            }
            Ok(checkpoint)
        })
    }

    fn list_checkpoints<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionCheckpoint>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions
                .get(thread_id)
                .map_or_else(Vec::new, |session| session.checkpoints.clone()))
        })
    }

    fn link_artifact<'a>(
        &'a self,
        thread_id: &'a str,
        mut link: SessionArtifactLink,
    ) -> SessionMetadataFuture<'a, SessionArtifactLink> {
        Box::pin(async move {
            let mut sessions = self.sessions.lock().map_err(lock_err)?;
            let entry = sessions.entry(thread_id.to_string()).or_default();
            link = redact_artifact_link(link);
            ensure_thread_ref(thread_id, &mut link.source_refs);
            if let Some(position) = entry
                .artifact_links
                .iter()
                .position(|existing| existing.artifact_id == link.artifact_id)
            {
                entry.artifact_links[position] = link.clone();
            } else {
                entry.artifact_links.push(link.clone());
            }
            Ok(link)
        })
    }

    fn list_artifact_links<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionArtifactLink>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions
                .get(thread_id)
                .map_or_else(Vec::new, |session| session.artifact_links.clone()))
        })
    }

    fn close_session<'a>(
        &'a self,
        thread_id: &'a str,
        mut closure: SessionClosure,
    ) -> SessionMetadataFuture<'a, SessionMetadataCloseResult> {
        Box::pin(async move {
            let mut sessions = self.sessions.lock().map_err(lock_err)?;
            let entry = sessions.entry(thread_id.to_string()).or_default();
            if let Some(existing) = &entry.closure {
                return Ok(SessionMetadataCloseResult {
                    closure: existing.clone(),
                    already_closed: true,
                });
            }
            closure = redact_closure(closure);
            ensure_thread_ref(thread_id, &mut closure.source_refs);
            entry.closure = Some(closure.clone());
            Ok(SessionMetadataCloseResult {
                closure,
                already_closed: false,
            })
        })
    }

    fn closure<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionClosure>> {
        Box::pin(async move {
            let sessions = self.sessions.lock().map_err(lock_err)?;
            Ok(sessions
                .get(thread_id)
                .and_then(|session| session.closure.clone()))
        })
    }

    fn kind(&self) -> SessionStoreKind {
        SessionStoreKind::InMemory
    }
}

#[derive(Clone)]
pub struct SessionRuntime {
    store: Arc<dyn SessionMetadataStore>,
    contexts: Arc<Mutex<HashMap<String, CurrentSessionContext>>>,
    token_seq: Arc<AtomicU64>,
}

impl SessionRuntime {
    pub fn new(store: Arc<dyn SessionMetadataStore>) -> Self {
        Self {
            store,
            contexts: Arc::new(Mutex::new(HashMap::new())),
            token_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn in_memory() -> Self {
        Self::new(Arc::new(InMemorySessionMetadataStore::new()))
    }

    pub fn store(&self) -> &Arc<dyn SessionMetadataStore> {
        &self.store
    }

    pub fn inject_context_args(
        &self,
        args: &Value,
        context: CurrentSessionContext,
    ) -> Result<Value, String> {
        let mut object = match args {
            Value::Null => Map::new(),
            Value::Object(object) => object.clone(),
            _ => {
                return Err("session tool arguments must be a JSON object".to_string());
            }
        };
        let token = self.issue_context_token(context)?;
        object.insert("context_token".to_string(), Value::String(token));
        Ok(Value::Object(object))
    }

    #[cfg(test)]
    fn context_count(&self) -> usize {
        self.contexts
            .lock()
            .map(|contexts| contexts.len())
            .unwrap_or_default()
    }

    pub fn take_context(&self, token: &str) -> AgenkitResult<CurrentSessionContext> {
        let mut contexts = self.contexts.lock().map_err(lock_err)?;
        contexts.remove(token).ok_or_else(|| {
            AgenkitError::validation("session tool called without an active session context")
        })
    }

    fn issue_context_token(&self, context: CurrentSessionContext) -> Result<String, String> {
        let random = ThreadId::mint().map_err(|err| err.to_string())?;
        let seq = self.token_seq.fetch_add(1, Ordering::Relaxed);
        let token = format!("{}-{seq}", random.as_str());
        let mut contexts = self.contexts.lock().map_err(|err| err.to_string())?;
        contexts.insert(token.clone(), context);
        Ok(token)
    }
}

pub fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn session_event_from_framework(event: &FrameworkEvent, timestamp_ms: u64) -> SessionEvent {
    let kind = match &event.kind {
        FrameworkEventKind::Started => SessionEventKind::Started,
        FrameworkEventKind::AssistantText => SessionEventKind::AssistantText,
        FrameworkEventKind::ToolStarted => SessionEventKind::ToolStarted,
        FrameworkEventKind::ToolCompleted => SessionEventKind::ToolCompleted,
        FrameworkEventKind::ToolFailed => SessionEventKind::ToolFailed,
        FrameworkEventKind::ToolBlocked => SessionEventKind::ToolBlocked,
        FrameworkEventKind::Compacted => SessionEventKind::Compacted,
        FrameworkEventKind::Stopped { .. } => SessionEventKind::Stopped,
        FrameworkEventKind::Failed => SessionEventKind::Failed,
        FrameworkEventKind::Unknown => SessionEventKind::Unknown,
    };

    let mut session_event = SessionEvent::new(kind, timestamp_ms);
    if let Some(message) = &event.message {
        session_event.message = Some(redact_text(message));
    }
    if let Some(tool) = &event.tool {
        session_event.tool = Some(tool.clone());
        session_event.source_refs.push(SessionSourceRef::Tool {
            tool_id: tool.clone(),
        });
    }
    for source_ref in &event.source_refs {
        if !session_event.source_refs.contains(source_ref) {
            session_event.source_refs.push(source_ref.clone());
        }
    }
    session_event.policy = match &event.kind {
        FrameworkEventKind::ToolStarted
        | FrameworkEventKind::ToolCompleted
        | FrameworkEventKind::Compacted => Some(SessionEventPolicy::SideEffecting),
        FrameworkEventKind::ToolFailed | FrameworkEventKind::Failed => {
            Some(SessionEventPolicy::Failed)
        }
        FrameworkEventKind::ToolBlocked => Some(SessionEventPolicy::Blocked),
        _ => Some(SessionEventPolicy::ReadOnly),
    };
    if let Some(payload) = &event.payload {
        session_event.payload = Some(redact_json_value(payload, 2048));
    }
    if let FrameworkEventKind::Stopped { status } = &event.kind {
        session_event.payload = Some(serde_json::json!({ "status": status_name(*status) }));
    } else if matches!(&event.kind, FrameworkEventKind::Compacted)
        && session_event.payload.is_none()
        && let Some(folded) = event.message.as_deref().and_then(parse_folded_count)
    {
        session_event.payload = Some(serde_json::json!({ "folded": folded }));
    }
    session_event
}

fn parse_folded_count(message: &str) -> Option<u64> {
    let mut parts = message.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("folded"), Some(value)) => value.parse().ok(),
        _ => None,
    }
}

fn status_name(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "idle",
        RunStatus::MaxSteps => "max_steps",
        RunStatus::Aborted => "aborted",
        RunStatus::Unknown => "unknown",
    }
}

pub(super) fn redact_text(text: &str) -> String {
    redact_text_to_limit(text, 4096)
}

pub(crate) fn redact_text_to_limit(text: &str, max_bytes: usize) -> String {
    if text_needs_full_redaction(text) {
        return "[redacted]".to_string();
    }
    truncate_utf8(text, max_bytes)
}

pub(crate) fn redact_json_value(value: &Value, max_string_bytes: usize) -> Value {
    redact_json_value_at_depth(value, max_string_bytes, 0)
}

fn redact_json_value_at_depth(value: &Value, max_string_bytes: usize, depth: usize) -> Value {
    const MAX_JSON_REDACTION_DEPTH: usize = 64;
    if depth > MAX_JSON_REDACTION_DEPTH {
        return Value::String("[redacted: too deep]".to_string());
    }
    match value {
        Value::String(text) => Value::String(redact_text_to_limit(text, max_string_bytes)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_json_value_at_depth(item, max_string_bytes, depth + 1))
                .collect(),
        ),
        Value::Object(object) => {
            let mut redacted = Map::new();
            for (key, value) in object {
                let value = if is_sensitive_key(key) {
                    Value::String("[redacted]".to_string())
                } else {
                    redact_json_value_at_depth(value, max_string_bytes, depth + 1)
                };
                redacted.insert(key.clone(), value);
            }
            Value::Object(redacted)
        }
        _ => value.clone(),
    }
}

pub(crate) fn redact_artifact_link(mut link: SessionArtifactLink) -> SessionArtifactLink {
    link.artifact_id = redact_text_to_limit(&link.artifact_id, 512);
    link.promotion_policy = link
        .promotion_policy
        .map(|policy| redact_text_to_limit(&policy, 512));
    link.source_refs = redact_source_refs(link.source_refs);
    link
}

pub(crate) fn redact_note(mut note: SessionNote) -> SessionNote {
    note.title = redact_text_to_limit(&note.title, 512);
    note.body = redact_text_to_limit(&note.body, 16 * 1024);
    note.tags = note
        .tags
        .into_iter()
        .map(|tag| redact_text_to_limit(&tag, 256))
        .collect();
    note.reason = redact_text_to_limit(&note.reason, 1024);
    note.source_refs = redact_source_refs(note.source_refs);
    note
}

pub(crate) fn redact_summary(mut summary: SessionSummary) -> SessionSummary {
    summary.title = redact_text_to_limit(&summary.title, 512);
    summary.body = redact_text_to_limit(&summary.body, 16 * 1024);
    summary.produced_by = summary
        .produced_by
        .map(|value| redact_text_to_limit(&value, 512));
    summary.source_marker = summary
        .source_marker
        .map(|value| redact_text_to_limit(&value, 512));
    summary.source_refs = redact_source_refs(summary.source_refs);
    summary
}

pub(crate) fn redact_checkpoint(mut checkpoint: SessionCheckpoint) -> SessionCheckpoint {
    checkpoint.name = checkpoint
        .name
        .map(|value| redact_text_to_limit(&value, 512));
    checkpoint.summary_id = checkpoint
        .summary_id
        .map(|value| redact_text_to_limit(&value, 512));
    checkpoint.summary = checkpoint
        .summary
        .map(|value| redact_text_to_limit(&value, 16 * 1024));
    checkpoint.source_refs = redact_source_refs(checkpoint.source_refs);
    checkpoint
}

pub(crate) fn redact_closure(mut closure: SessionClosure) -> SessionClosure {
    closure.reason = closure
        .reason
        .map(|reason| redact_text_to_limit(&reason, 1024));
    closure.source_refs = redact_source_refs(closure.source_refs);
    closure
}

pub(crate) fn redact_source_refs(source_refs: Vec<SessionSourceRef>) -> Vec<SessionSourceRef> {
    source_refs
        .into_iter()
        .map(|source_ref| match source_ref {
            SessionSourceRef::Thread { thread_id } => SessionSourceRef::Thread {
                thread_id: redact_text_to_limit(&thread_id, 512),
            },
            SessionSourceRef::SessionThread { session_thread_id } => {
                SessionSourceRef::SessionThread {
                    session_thread_id: redact_text_to_limit(&session_thread_id, 512),
                }
            }
            SessionSourceRef::Record { thread_id, seq } => SessionSourceRef::Record {
                thread_id: redact_text_to_limit(&thread_id, 512),
                seq,
            },
            SessionSourceRef::RecordRange {
                thread_id,
                start_seq,
                end_seq,
            } => SessionSourceRef::RecordRange {
                thread_id: redact_text_to_limit(&thread_id, 512),
                start_seq,
                end_seq,
            },
            SessionSourceRef::Event { seq } => SessionSourceRef::Event { seq },
            SessionSourceRef::EventRange { start_seq, end_seq } => {
                SessionSourceRef::EventRange { start_seq, end_seq }
            }
            SessionSourceRef::Step { step_id } => SessionSourceRef::Step {
                step_id: redact_text_to_limit(&step_id, 512),
            },
            SessionSourceRef::ToolCall { call_id, tool_id } => SessionSourceRef::ToolCall {
                call_id: redact_text_to_limit(&call_id, 512),
                tool_id: redact_text_to_limit(&tool_id, 512),
            },
            SessionSourceRef::Tool { tool_id } => SessionSourceRef::Tool {
                tool_id: redact_text_to_limit(&tool_id, 512),
            },
            SessionSourceRef::Artifact { artifact_id } => SessionSourceRef::Artifact {
                artifact_id: redact_text_to_limit(&artifact_id, 512),
            },
            SessionSourceRef::Checkpoint { checkpoint_id } => SessionSourceRef::Checkpoint {
                checkpoint_id: redact_text_to_limit(&checkpoint_id, 512),
            },
            SessionSourceRef::Path { path } => SessionSourceRef::Path {
                path: redact_text_to_limit(&path, 1024),
            },
        })
        .collect()
}

fn text_needs_full_redaction(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("bearer ")
        || lower.contains("private key")
        || lower.contains("-----begin ")
        || contains_sensitive_assignment(&lower)
}

fn contains_sensitive_assignment(lower_text: &str) -> bool {
    for key in [
        "api_key",
        "apikey",
        "api-key",
        "x-api-key",
        "access_token",
        "refresh_token",
        "id_token",
        "token",
        "authorization",
        "password",
        "secret",
        "client_secret",
        "secret_key",
        "credential",
        "credentials",
        "private_key",
    ] {
        let mut offset = 0;
        while let Some(position) = lower_text[offset..].find(key) {
            let start = offset + position;
            let end = start + key.len();
            if is_assignment_boundary(lower_text, start, end) {
                return true;
            }
            offset = end;
        }
    }
    false
}

fn is_assignment_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    if before.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
        return false;
    }
    let after = text[end..].trim_start();
    after.starts_with('=') || after.starts_with(':')
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "apikey"
            | "xapikey"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "authorization"
            | "auth"
            | "password"
            | "passwd"
            | "pwd"
            | "secret"
            | "clientsecret"
            | "secretkey"
            | "privatekey"
            | "credential"
            | "credentials"
    ) || normalized.ends_with("apikey")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.ends_with("password")
}

pub fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn lock_err<T>(_: std::sync::PoisonError<T>) -> AgenkitError {
    AgenkitError::internal("session metadata store lock poisoned")
}

fn ensure_thread_ref(thread_id: &str, refs: &mut Vec<SessionSourceRef>) {
    if !refs.iter().any(|source| {
        matches!(
            source,
            SessionSourceRef::Thread { thread_id: existing } if existing == thread_id
        )
    }) {
        refs.push(SessionSourceRef::Thread {
            thread_id: thread_id.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> SessionIdentity {
        SessionIdentity {
            thread_id: "thread-1".to_string(),
            agent_id: "agent".to_string(),
            model: "local/default".to_string(),
            run_id: None,
            turn_id: None,
            principal_key: Some("user-1".to_string()),
            tool_ids: vec!["session.info".to_string()],
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
    async fn in_memory_store_assigns_event_sequences() {
        let store = InMemorySessionMetadataStore::new();
        store.upsert_identity(identity()).await.unwrap();
        let first = store
            .append_event("thread-1", SessionEvent::new(SessionEventKind::Started, 1))
            .await
            .unwrap();
        let second = store
            .append_event("thread-1", SessionEvent::new(SessionEventKind::Stopped, 2))
            .await
            .unwrap();

        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
        assert_eq!(store.event_count("thread-1").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn in_memory_store_filters_after_sequence() {
        let store = InMemorySessionMetadataStore::new();
        for kind in [
            SessionEventKind::Started,
            SessionEventKind::AssistantText,
            SessionEventKind::Stopped,
        ] {
            store
                .append_event("thread-1", SessionEvent::new(kind, 1))
                .await
                .unwrap();
        }

        let events = store
            .list_events(
                "thread-1",
                SessionEventFilter {
                    after_seq: Some(1),
                    start_seq: None,
                    end_seq: None,
                    limit: 10,
                    kinds: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 2);
    }

    #[tokio::test]
    async fn in_memory_store_filters_event_ranges() {
        let store = InMemorySessionMetadataStore::new();
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

        let events = store
            .list_events(
                "thread-1",
                SessionEventFilter {
                    after_seq: None,
                    start_seq: Some(2),
                    end_seq: Some(3),
                    limit: 10,
                    kinds: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            events.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[tokio::test]
    async fn in_memory_store_missing_session_is_empty() {
        let store = InMemorySessionMetadataStore::new();
        assert!(store.identity("missing").await.unwrap().is_none());
        assert_eq!(store.event_count("missing").await.unwrap(), 0);
        assert!(
            store
                .list_events("missing", SessionEventFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .read_note("missing", "note-1")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .read_summary("missing", "summary-1")
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.list_checkpoints("missing").await.unwrap().is_empty());
        assert!(
            store
                .list_artifact_links("missing")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(store.closure("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn in_memory_store_assigns_note_ids_and_thread_refs() {
        let store = InMemorySessionMetadataStore::new();
        let note = store
            .append_note(
                "thread-1",
                SessionNote {
                    id: String::new(),
                    title: "Decision".to_string(),
                    body: "Keep session metadata beside the transcript.".to_string(),
                    tags: vec!["architecture".to_string()],
                    reason: "Needed before memory promotion.".to_string(),
                    source_refs: vec![SessionSourceRef::Event { seq: 1 }],
                    created_at_ms: 10,
                    updated_at_ms: 10,
                },
            )
            .await
            .unwrap();

        assert_eq!(note.id, "note-1");
        assert!(note.source_refs.contains(&SessionSourceRef::Thread {
            thread_id: "thread-1".to_string()
        }));
        assert_eq!(
            store
                .read_note("thread-1", "note-1")
                .await
                .unwrap()
                .unwrap()
                .title,
            "Decision"
        );
    }

    #[tokio::test]
    async fn in_memory_store_assigns_summary_ids_and_thread_refs() {
        let store = InMemorySessionMetadataStore::new();
        let summary = store
            .write_summary(
                "thread-1",
                SessionSummary {
                    id: String::new(),
                    title: "Range summary".to_string(),
                    body: "Events 1 through 3 covered the setup.".to_string(),
                    covered_events: Some(agenkitty_core::SessionEventRange {
                        start_seq: 1,
                        end_seq: 3,
                    }),
                    covered_records: None,
                    source_refs: vec![SessionSourceRef::EventRange {
                        start_seq: 1,
                        end_seq: 3,
                    }],
                    produced_by: Some("session.summary".to_string()),
                    source_marker: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(summary.id, "summary-1");
        assert!(summary.source_refs.contains(&SessionSourceRef::Thread {
            thread_id: "thread-1".to_string()
        }));
        assert_eq!(
            store
                .read_summary("thread-1", "summary-1")
                .await
                .unwrap()
                .unwrap()
                .covered_events
                .unwrap()
                .end_seq,
            3
        );
    }

    #[tokio::test]
    async fn in_memory_store_orders_named_checkpoints() {
        let store = InMemorySessionMetadataStore::new();
        for name in ["before edit", "after edit"] {
            store
                .write_checkpoint(
                    "thread-1",
                    SessionCheckpoint {
                        id: String::new(),
                        name: Some(name.to_string()),
                        kind: agenkitty_core::SessionCheckpointKind::Manual,
                        covered_events: Some(agenkitty_core::SessionEventRange {
                            start_seq: 1,
                            end_seq: 1,
                        }),
                        covered_records: None,
                        summary_id: None,
                        summary: None,
                        source_refs: vec![SessionSourceRef::Event { seq: 1 }],
                        created_at_ms: 10,
                    },
                )
                .await
                .unwrap();
        }

        let checkpoints = store.list_checkpoints("thread-1").await.unwrap();
        assert_eq!(checkpoints.len(), 2);
        assert_eq!(checkpoints[0].id, "checkpoint-1");
        assert_eq!(checkpoints[1].name.as_deref(), Some("after edit"));
        assert!(
            checkpoints[0]
                .source_refs
                .contains(&SessionSourceRef::Thread {
                    thread_id: "thread-1".to_string()
                })
        );
    }

    #[tokio::test]
    async fn in_memory_store_links_artifacts_and_closes_once() {
        let store = InMemorySessionMetadataStore::new();
        let artifact = store
            .link_artifact(
                "thread-1",
                SessionArtifactLink {
                    artifact_id: "artifact-1".to_string(),
                    source_refs: vec![SessionSourceRef::Event { seq: 1 }],
                    promotion_policy: Some("manual".to_string()),
                    created_at_ms: 10,
                },
            )
            .await
            .unwrap();
        let closure = store
            .close_session(
                "thread-1",
                SessionClosure {
                    closed_at_ms: 12,
                    reason: Some("done".to_string()),
                    source_refs: Vec::new(),
                },
            )
            .await
            .unwrap();
        let second = store
            .close_session(
                "thread-1",
                SessionClosure {
                    closed_at_ms: 13,
                    reason: Some("different".to_string()),
                    source_refs: Vec::new(),
                },
            )
            .await
            .unwrap();

        assert!(artifact.source_refs.contains(&SessionSourceRef::Thread {
            thread_id: "thread-1".to_string()
        }));
        assert!(
            closure
                .closure
                .source_refs
                .contains(&SessionSourceRef::Thread {
                    thread_id: "thread-1".to_string()
                })
        );
        assert!(!closure.already_closed);
        assert!(second.already_closed);
        assert_eq!(closure.closure, second.closure);
        assert_eq!(
            store.list_artifact_links("thread-1").await.unwrap().len(),
            1
        );
    }

    #[test]
    fn current_session_context_builds_source_refs() {
        let context = CurrentSessionContext {
            identity: identity(),
        };

        assert_eq!(
            context.record_range_ref(2, 4),
            SessionSourceRef::RecordRange {
                thread_id: "thread-1".to_string(),
                start_seq: 2,
                end_seq: 4
            }
        );
        assert_eq!(
            context.tool_call_ref("call-1", "fs.read"),
            SessionSourceRef::ToolCall {
                call_id: "call-1".to_string(),
                tool_id: "fs.read".to_string()
            }
        );
    }

    #[test]
    fn runtime_injects_one_time_context_token() {
        let runtime = SessionRuntime::in_memory();
        let args = runtime
            .inject_context_args(
                &serde_json::json!({"limit": 2}),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let token = args["context_token"].as_str().unwrap();
        assert_eq!(
            runtime.take_context(token).unwrap().identity.thread_id,
            "thread-1"
        );
        assert!(runtime.take_context(token).is_err());
    }

    #[test]
    fn runtime_does_not_leak_context_token_for_invalid_args() {
        let runtime = SessionRuntime::in_memory();
        let result = runtime.inject_context_args(
            &serde_json::json!(["not", "object"]),
            CurrentSessionContext {
                identity: identity(),
            },
        );

        assert!(result.is_err());
        assert_eq!(runtime.context_count(), 0);
    }

    #[test]
    fn runtime_context_tokens_are_isolated() {
        let runtime = SessionRuntime::in_memory();
        let mut second = identity();
        second.thread_id = "thread-2".to_string();
        let first_args = runtime
            .inject_context_args(
                &serde_json::json!({}),
                CurrentSessionContext {
                    identity: identity(),
                },
            )
            .unwrap();
        let second_args = runtime
            .inject_context_args(
                &serde_json::json!({}),
                CurrentSessionContext { identity: second },
            )
            .unwrap();
        let first_token = first_args["context_token"].as_str().unwrap();
        let second_token = second_args["context_token"].as_str().unwrap();

        assert_eq!(
            runtime
                .take_context(second_token)
                .unwrap()
                .identity
                .thread_id,
            "thread-2"
        );
        assert_eq!(
            runtime
                .take_context(first_token)
                .unwrap()
                .identity
                .thread_id,
            "thread-1"
        );
    }

    #[test]
    fn framework_event_redacts_secret_like_messages() {
        let event = FrameworkEvent::failed("api_key=abc");
        let session_event = session_event_from_framework(&event, 1);
        assert_eq!(session_event.message.as_deref(), Some("[redacted]"));
        assert_eq!(session_event.policy, Some(SessionEventPolicy::Failed));
    }

    #[test]
    fn framework_event_carries_tool_call_refs_and_redacts_payloads() {
        let event = FrameworkEvent::tool_started("fs.read")
            .with_source_ref(SessionSourceRef::ToolCall {
                call_id: "call-1".to_string(),
                tool_id: "fs.read".to_string(),
            })
            .with_payload(serde_json::json!({
                "args": {
                    "path": "Cargo.toml",
                    "password": "secret"
                }
            }));

        let session_event = session_event_from_framework(&event, 1);

        assert!(session_event.source_refs.contains(&SessionSourceRef::Tool {
            tool_id: "fs.read".to_string()
        }));
        assert!(
            session_event
                .source_refs
                .contains(&SessionSourceRef::ToolCall {
                    call_id: "call-1".to_string(),
                    tool_id: "fs.read".to_string()
                })
        );
        assert_eq!(
            session_event.payload.as_ref().unwrap()["args"]["password"],
            "[redacted]"
        );
    }

    #[test]
    fn compaction_events_include_folded_count() {
        let event = FrameworkEvent::compacted(4);

        let session_event = session_event_from_framework(&event, 1);

        assert_eq!(session_event.payload.as_ref().unwrap()["folded"], 4);
        assert_eq!(
            session_event.policy,
            Some(SessionEventPolicy::SideEffecting)
        );
    }

    #[test]
    fn redaction_covers_common_secret_keys_without_destroying_benign_text() {
        let redacted = redact_json_value(
            &serde_json::json!({
                "x-api-key": "sk-live",
                "authorization": "Basic abc",
                "token": "ghp_secret",
                "access_token": "access",
                "body": "the secret to performance is caching",
                "path": "src/secrets/config.rs",
                "help": "reset your password"
            }),
            2048,
        );

        assert_eq!(redacted["x-api-key"], "[redacted]");
        assert_eq!(redacted["authorization"], "[redacted]");
        assert_eq!(redacted["token"], "[redacted]");
        assert_eq!(redacted["access_token"], "[redacted]");
        assert_eq!(redacted["body"], "the secret to performance is caching");
        assert_eq!(redacted["path"], "src/secrets/config.rs");
        assert_eq!(redacted["help"], "reset your password");
        assert_eq!(redact_text_to_limit("api_key=abc", 128), "[redacted]");
    }

    #[tokio::test]
    async fn store_redacts_notes_summaries_and_checkpoints() {
        let store = InMemorySessionMetadataStore::new();

        let note = store
            .append_note(
                "thread-1",
                SessionNote {
                    id: String::new(),
                    title: "api_key=abc".to_string(),
                    body: "bearer token should not persist".to_string(),
                    tags: vec!["token: abc".to_string()],
                    reason: "password: abc".to_string(),
                    source_refs: vec![SessionSourceRef::Path {
                        path: "api_key=abc".to_string(),
                    }],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            )
            .await
            .unwrap();
        let summary = store
            .write_summary(
                "thread-1",
                SessionSummary {
                    id: String::new(),
                    title: "authorization: abc".to_string(),
                    body: "refresh_token=abc".to_string(),
                    covered_events: None,
                    covered_records: None,
                    source_refs: vec![SessionSourceRef::ToolCall {
                        call_id: "secret=abc".to_string(),
                        tool_id: "session.summary".to_string(),
                    }],
                    produced_by: Some("client_secret=abc".to_string()),
                    source_marker: Some("private_key: abc".to_string()),
                },
            )
            .await
            .unwrap();
        let checkpoint = store
            .write_checkpoint(
                "thread-1",
                SessionCheckpoint {
                    id: String::new(),
                    name: Some("x-api-key: abc".to_string()),
                    kind: agenkitty_core::SessionCheckpointKind::Manual,
                    covered_events: None,
                    covered_records: None,
                    summary_id: Some("token=abc".to_string()),
                    summary: Some("password=abc".to_string()),
                    source_refs: vec![SessionSourceRef::Checkpoint {
                        checkpoint_id: "api_key=abc".to_string(),
                    }],
                    created_at_ms: 1,
                },
            )
            .await
            .unwrap();

        assert_eq!(note.title, "[redacted]");
        assert_eq!(note.body, "[redacted]");
        assert_eq!(note.tags, vec!["[redacted]"]);
        assert_eq!(note.reason, "[redacted]");
        assert_eq!(summary.title, "[redacted]");
        assert_eq!(summary.body, "[redacted]");
        assert_eq!(summary.produced_by.as_deref(), Some("[redacted]"));
        assert_eq!(summary.source_marker.as_deref(), Some("[redacted]"));
        assert_eq!(checkpoint.name.as_deref(), Some("[redacted]"));
        assert_eq!(checkpoint.summary_id.as_deref(), Some("[redacted]"));
        assert_eq!(checkpoint.summary.as_deref(), Some("[redacted]"));
        assert!(!format!("{note:?}{summary:?}{checkpoint:?}").contains("abc"));
    }

    #[test]
    fn redaction_caps_deep_json() {
        let mut value = serde_json::json!("leaf");
        for _ in 0..70 {
            value = serde_json::json!([value]);
        }

        let redacted = redact_json_value(&value, 2048);

        assert!(format!("{redacted}").contains("[redacted: too deep]"));
    }

    #[tokio::test]
    async fn default_event_filter_returns_all_events() {
        let store = InMemorySessionMetadataStore::new();
        for kind in [
            SessionEventKind::Started,
            SessionEventKind::AssistantText,
            SessionEventKind::Stopped,
        ] {
            store
                .append_event("thread-1", SessionEvent::new(kind, 1))
                .await
                .unwrap();
        }

        let events = store
            .list_events("thread-1", SessionEventFilter::default())
            .await
            .unwrap();

        assert_eq!(events.len(), 3);
    }
}
