use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use agenkitty_core::{
    SessionArtifactLink, SessionCheckpoint, SessionClosure, SessionEvent, SessionIdentity,
    SessionNote, SessionSourceRef, SessionStoreKind, SessionSummary,
};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use pocopine_crypto::sha256_hex;
use serde::{Deserialize, Serialize};

use super::common::{
    SessionEventFilter, SessionMetadataCloseResult, SessionMetadataFuture, SessionMetadataStore,
    redact_artifact_link, redact_checkpoint, redact_closure, redact_note, redact_summary,
};

#[derive(Debug)]
pub struct LocalJsonlSessionMetadataStore {
    root: PathBuf,
    io_lock: Arc<Mutex<()>>,
    sessions: Mutex<HashMap<String, StoredSession>>,
}

#[derive(Clone, Debug, Default)]
struct StoredSession {
    identity: Option<SessionIdentity>,
    events: Vec<SessionEvent>,
    notes: Vec<SessionNote>,
    summaries: Vec<SessionSummary>,
    checkpoints: Vec<SessionCheckpoint>,
    artifact_links: Vec<SessionArtifactLink>,
    closure: Option<SessionClosure>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LocalMetadataRecord {
    Identity { identity: SessionIdentity },
    Event { event: SessionEvent },
    Note { note: SessionNote },
    Summary { summary: SessionSummary },
    Checkpoint { checkpoint: SessionCheckpoint },
    ArtifactLink { link: SessionArtifactLink },
    Closure { closure: SessionClosure },
}

impl LocalJsonlSessionMetadataStore {
    pub fn open(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        let root = root.as_ref();
        fs::create_dir_all(root).map_err(io_err)?;
        let root = fs::canonicalize(root).map_err(io_err)?;
        if fs::symlink_metadata(&root)
            .map_err(io_err)?
            .file_type()
            .is_symlink()
        {
            return Err(AgenkitError::tool_policy(format!(
                "session metadata root `{}` is a symlink",
                root.display()
            )));
        }
        Ok(Self {
            io_lock: root_lock(&root)?,
            root,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn load_session(&self, thread_id: &str) -> AgenkitResult<Option<StoredSession>> {
        let path = self.log_path(thread_id)?;
        self.load_session_path(&path)
    }

    fn with_loaded_session<T>(
        &self,
        thread_id: &str,
        f: impl FnOnce(Option<&StoredSession>) -> T,
    ) -> AgenkitResult<T> {
        let _io_lock = self.io_lock.lock().map_err(lock_err)?;
        let mut sessions = self.sessions.lock().map_err(lock_err)?;
        let loaded = self.load_session(thread_id)?;
        match loaded {
            Some(session) => {
                sessions.insert(thread_id.to_string(), session);
            }
            None => {
                sessions.remove(thread_id);
            }
        }
        Ok(f(sessions.get(thread_id)))
    }

    fn load_all_sessions(&self) -> AgenkitResult<()> {
        let _io_lock = self.io_lock.lock().map_err(lock_err)?;
        let mut thread_ids = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(io_err)?;
            if metadata.file_type().is_symlink() {
                return Err(AgenkitError::tool_policy(format!(
                    "session metadata log `{}` is a symlink",
                    path.display()
                )));
            }
            if !metadata.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
            {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if !is_safe_log_filename(stem) {
                continue;
            }
            thread_ids.push(stem.to_string());
        }

        let mut sessions = self.sessions.lock().map_err(lock_err)?;
        for stem in thread_ids {
            let path = self.root.join(format!("{stem}.jsonl"));
            let Ok(Some(session)) = self.load_session_path(&path) else {
                continue;
            };
            if let Some(identity) = &session.identity {
                sessions.insert(identity.thread_id.clone(), session);
            }
        }
        Ok(())
    }

    fn with_loaded_session_mut<T>(
        &self,
        thread_id: &str,
        f: impl FnOnce(&mut StoredSession) -> AgenkitResult<(T, LocalMetadataRecord)>,
    ) -> AgenkitResult<T> {
        let _io_lock = self.io_lock.lock().map_err(lock_err)?;
        let mut sessions = self.sessions.lock().map_err(lock_err)?;
        let loaded = self.load_session(thread_id)?.unwrap_or_default();
        sessions.insert(thread_id.to_string(), loaded);
        let session = sessions
            .get_mut(thread_id)
            .expect("session inserted before mutation");
        let (value, record) = f(session)?;
        self.append_record(thread_id, &record)?;
        apply_record(session, record);
        Ok(value)
    }

    fn append_record(&self, thread_id: &str, record: &LocalMetadataRecord) -> AgenkitResult<()> {
        let path = self.log_path(thread_id)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AgenkitError::tool_policy(format!(
                    "session metadata log `{}` is a symlink",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err(err)),
        }
        let mut line = serde_json::to_string(record).map_err(json_err)?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io_err)?;
        file.write_all(line.as_bytes()).map_err(io_err)?;
        file.flush().map_err(io_err)?;
        Ok(())
    }

    fn log_path(&self, thread_id: &str) -> AgenkitResult<PathBuf> {
        if thread_id.is_empty() {
            return Err(AgenkitError::validation("empty session thread id"));
        }
        let stem = log_stem(thread_id);
        let path = self.root.join(format!("{stem}.jsonl"));
        ensure_under_root(&self.root, &path)?;
        Ok(path)
    }

    fn load_session_path(&self, path: &Path) -> AgenkitResult<Option<StoredSession>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AgenkitError::tool_policy(format!(
                    "session metadata log `{}` is a symlink",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(io_err(err)),
        }

        let body = fs::read_to_string(path).map_err(io_err)?;
        let ended_with_newline = body.ends_with('\n');
        let lines = body.lines().collect::<Vec<_>>();
        let mut session = StoredSession::default();
        for (index, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: LocalMetadataRecord = match serde_json::from_str(line) {
                Ok(record) => record,
                Err(_) if !ended_with_newline && index + 1 == lines.len() => break,
                Err(err) => {
                    return Err(AgenkitError::Json {
                        message: format!(
                            "session metadata log `{}` line {} is corrupt: {err}",
                            path.display(),
                            index + 1
                        ),
                    });
                }
            };
            apply_record(&mut session, record);
        }
        Ok(Some(session))
    }
}

impl SessionMetadataStore for LocalJsonlSessionMetadataStore {
    fn upsert_identity<'a>(
        &'a self,
        identity: SessionIdentity,
    ) -> SessionMetadataFuture<'a, SessionIdentity> {
        Box::pin(async move {
            let thread_id = identity.thread_id.clone();
            self.with_loaded_session_mut(&thread_id, |_| {
                Ok((
                    identity.clone(),
                    LocalMetadataRecord::Identity {
                        identity: identity.clone(),
                    },
                ))
            })
        })
    }

    fn identity<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionIdentity>> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                session.and_then(|session| session.identity.clone())
            })
        })
    }

    fn list_identities<'a>(&'a self) -> SessionMetadataFuture<'a, Vec<SessionIdentity>> {
        Box::pin(async move {
            self.load_all_sessions()?;
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
            self.with_loaded_session_mut(thread_id, |session| {
                event.seq = session.events.last().map_or(1, |last| last.seq + 1);
                ensure_thread_ref(thread_id, &mut event.source_refs);
                Ok((
                    event.clone(),
                    LocalMetadataRecord::Event {
                        event: event.clone(),
                    },
                ))
            })
        })
    }

    fn list_events<'a>(
        &'a self,
        thread_id: &'a str,
        filter: SessionEventFilter,
    ) -> SessionMetadataFuture<'a, Vec<SessionEvent>> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                let Some(session) = session else {
                    return Vec::new();
                };
                // `limit == 0` means "no cap" — matching InMemorySessionMetadataStore.
                // (A caller like session.search relies on this to scan every event.)
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
                events
            })
        })
    }

    fn event_count<'a>(&'a self, thread_id: &'a str) -> SessionMetadataFuture<'a, u64> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                session.map_or(0, |session| session.events.len() as u64)
            })
        })
    }

    fn append_note<'a>(
        &'a self,
        thread_id: &'a str,
        mut note: SessionNote,
    ) -> SessionMetadataFuture<'a, SessionNote> {
        Box::pin(async move {
            self.with_loaded_session_mut(thread_id, |session| {
                note = redact_note(note.clone());
                if note.id.is_empty() {
                    note.id = format!("note-{}", session.notes.len().saturating_add(1));
                }
                if session.notes.iter().any(|existing| existing.id == note.id) {
                    return Err(AgenkitError::validation(format!(
                        "session note `{}` already exists",
                        note.id
                    )));
                }
                ensure_thread_ref(thread_id, &mut note.source_refs);
                Ok((
                    note.clone(),
                    LocalMetadataRecord::Note { note: note.clone() },
                ))
            })
        })
    }

    fn read_note<'a>(
        &'a self,
        thread_id: &'a str,
        note_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionNote>> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                session.and_then(|session| {
                    session
                        .notes
                        .iter()
                        .find(|note| note.id == note_id)
                        .cloned()
                })
            })
        })
    }

    fn list_notes<'a>(&'a self, thread_id: &'a str) -> SessionMetadataFuture<'a, Vec<SessionNote>> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                session.map_or_else(Vec::new, |session| session.notes.clone())
            })
        })
    }

    fn write_summary<'a>(
        &'a self,
        thread_id: &'a str,
        mut summary: SessionSummary,
    ) -> SessionMetadataFuture<'a, SessionSummary> {
        Box::pin(async move {
            self.with_loaded_session_mut(thread_id, |session| {
                summary = redact_summary(summary.clone());
                if summary.id.is_empty() {
                    summary.id = format!("summary-{}", session.summaries.len().saturating_add(1));
                }
                ensure_thread_ref(thread_id, &mut summary.source_refs);
                Ok((
                    summary.clone(),
                    LocalMetadataRecord::Summary {
                        summary: summary.clone(),
                    },
                ))
            })
        })
    }

    fn read_summary<'a>(
        &'a self,
        thread_id: &'a str,
        summary_id: &'a str,
    ) -> SessionMetadataFuture<'a, Option<SessionSummary>> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                session.and_then(|session| {
                    session
                        .summaries
                        .iter()
                        .find(|summary| summary.id == summary_id)
                        .cloned()
                })
            })
        })
    }

    fn list_summaries<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionSummary>> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                session.map_or_else(Vec::new, |session| session.summaries.clone())
            })
        })
    }

    fn write_checkpoint<'a>(
        &'a self,
        thread_id: &'a str,
        mut checkpoint: SessionCheckpoint,
    ) -> SessionMetadataFuture<'a, SessionCheckpoint> {
        Box::pin(async move {
            self.with_loaded_session_mut(thread_id, |session| {
                checkpoint = redact_checkpoint(checkpoint.clone());
                if checkpoint.id.is_empty() {
                    checkpoint.id =
                        format!("checkpoint-{}", session.checkpoints.len().saturating_add(1));
                }
                ensure_thread_ref(thread_id, &mut checkpoint.source_refs);
                Ok((
                    checkpoint.clone(),
                    LocalMetadataRecord::Checkpoint {
                        checkpoint: checkpoint.clone(),
                    },
                ))
            })
        })
    }

    fn list_checkpoints<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionCheckpoint>> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                session.map_or_else(Vec::new, |session| session.checkpoints.clone())
            })
        })
    }

    fn link_artifact<'a>(
        &'a self,
        thread_id: &'a str,
        mut link: SessionArtifactLink,
    ) -> SessionMetadataFuture<'a, SessionArtifactLink> {
        Box::pin(async move {
            self.with_loaded_session_mut(thread_id, |_session| {
                link = redact_artifact_link(link.clone());
                ensure_thread_ref(thread_id, &mut link.source_refs);
                Ok((
                    link.clone(),
                    LocalMetadataRecord::ArtifactLink { link: link.clone() },
                ))
            })
        })
    }

    fn list_artifact_links<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> SessionMetadataFuture<'a, Vec<SessionArtifactLink>> {
        Box::pin(async move {
            self.with_loaded_session(thread_id, |session| {
                session.map_or_else(Vec::new, |session| session.artifact_links.clone())
            })
        })
    }

    fn close_session<'a>(
        &'a self,
        thread_id: &'a str,
        mut closure: SessionClosure,
    ) -> SessionMetadataFuture<'a, SessionMetadataCloseResult> {
        Box::pin(async move {
            let _io_lock = self.io_lock.lock().map_err(lock_err)?;
            let mut sessions = self.sessions.lock().map_err(lock_err)?;
            let session = self.load_session(thread_id)?.unwrap_or_default();
            sessions.insert(thread_id.to_string(), session);
            let session = sessions
                .get_mut(thread_id)
                .expect("session inserted before mutation");
            if let Some(existing) = &session.closure {
                return Ok(SessionMetadataCloseResult {
                    closure: existing.clone(),
                    already_closed: true,
                });
            }
            closure = redact_closure(closure);
            ensure_thread_ref(thread_id, &mut closure.source_refs);
            let record = LocalMetadataRecord::Closure {
                closure: closure.clone(),
            };
            self.append_record(thread_id, &record)?;
            apply_record(session, record);
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
            self.with_loaded_session(thread_id, |session| {
                session.and_then(|session| session.closure.clone())
            })
        })
    }

    fn kind(&self) -> SessionStoreKind {
        SessionStoreKind::LocalJsonl
    }
}

fn apply_record(session: &mut StoredSession, record: LocalMetadataRecord) {
    match record {
        LocalMetadataRecord::Identity { identity } => session.identity = Some(identity),
        LocalMetadataRecord::Event { event } => session.events.push(event),
        LocalMetadataRecord::Note { note } => session.notes.push(note),
        LocalMetadataRecord::Summary { summary } => {
            if let Some(position) = session
                .summaries
                .iter()
                .position(|existing| existing.id == summary.id)
            {
                session.summaries[position] = summary;
            } else {
                session.summaries.push(summary);
            }
        }
        LocalMetadataRecord::Checkpoint { checkpoint } => {
            if let Some(position) = session
                .checkpoints
                .iter()
                .position(|existing| existing.id == checkpoint.id)
            {
                session.checkpoints[position] = checkpoint;
            } else {
                session.checkpoints.push(checkpoint);
            }
        }
        LocalMetadataRecord::ArtifactLink { link } => {
            if let Some(position) = session
                .artifact_links
                .iter()
                .position(|existing| existing.artifact_id == link.artifact_id)
            {
                session.artifact_links[position] = link;
            } else {
                session.artifact_links.push(link);
            }
        }
        LocalMetadataRecord::Closure { closure } => {
            if session.closure.is_none() {
                session.closure = Some(closure);
            }
        }
    }
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

fn root_lock(root: &Path) -> AgenkitResult<Arc<Mutex<()>>> {
    static ROOT_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();

    let locks = ROOT_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().map_err(lock_err)?;
    if let Some(lock) = locks.get(root).and_then(Weak::upgrade) {
        return Ok(lock);
    }

    let lock = Arc::new(Mutex::new(()));
    locks.insert(root.to_path_buf(), Arc::downgrade(&lock));
    Ok(lock)
}

fn log_stem(thread_id: &str) -> String {
    format!("session-{}", sha256_hex(thread_id.as_bytes()))
}

fn is_safe_log_filename(stem: &str) -> bool {
    stem.strip_prefix("session-").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .chars()
                .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
    })
}

fn ensure_under_root(root: &Path, path: &Path) -> AgenkitResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AgenkitError::validation("session metadata path has no parent"))?;
    let parent = if parent.exists() {
        fs::canonicalize(parent).map_err(io_err)?
    } else {
        parent.to_path_buf()
    };
    if parent == root {
        Ok(())
    } else {
        Err(AgenkitError::tool_policy(format!(
            "session metadata path `{}` escapes `{}`",
            path.display(),
            root.display()
        )))
    }
}

fn io_err(err: std::io::Error) -> AgenkitError {
    AgenkitError::internal(err.to_string())
}

fn json_err(err: serde_json::Error) -> AgenkitError {
    AgenkitError::Json {
        message: err.to_string(),
    }
}

fn lock_err<T>(_: std::sync::PoisonError<T>) -> AgenkitError {
    AgenkitError::internal("session metadata store lock poisoned")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use agenkitty_core::{
        SessionArtifactLink, SessionCheckpointKind, SessionClosure, SessionEventKind,
        SessionEventRange, SessionStoreKind,
    };

    fn identity(thread_id: &str) -> SessionIdentity {
        SessionIdentity {
            thread_id: thread_id.to_string(),
            agent_id: "agent".to_string(),
            model: "local/default".to_string(),
            run_id: None,
            turn_id: None,
            principal_key: Some("local".to_string()),
            tool_ids: vec!["session.info".to_string()],
            max_steps_per_turn: 8,
            capture_policy: "full".to_string(),
            transcript_store: SessionStoreKind::LocalJsonl,
            metadata_store: SessionStoreKind::LocalJsonl,
            created_at_ms: 1,
            last_active_at_ms: 1,
            project_id: None,
        }
    }

    #[tokio::test]
    async fn local_store_reloads_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        store.upsert_identity(identity("thread-1")).await.unwrap();
        let first = store
            .append_event("thread-1", SessionEvent::new(SessionEventKind::Started, 1))
            .await
            .unwrap();
        assert_eq!(first.seq, 1);

        let reopened = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        assert_eq!(reopened.kind(), SessionStoreKind::LocalJsonl);
        assert_eq!(reopened.event_count("thread-1").await.unwrap(), 1);
        assert_eq!(
            reopened
                .identity("thread-1")
                .await
                .unwrap()
                .unwrap()
                .agent_id,
            "agent"
        );
        let second = reopened
            .append_event("thread-1", SessionEvent::new(SessionEventKind::Stopped, 2))
            .await
            .unwrap();
        assert_eq!(second.seq, 2);
        let ranged = reopened
            .list_events(
                "thread-1",
                SessionEventFilter {
                    after_seq: None,
                    start_seq: Some(2),
                    end_seq: Some(2),
                    limit: 10,
                    kinds: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(ranged.len(), 1);
        assert_eq!(ranged[0].seq, 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn local_store_serializes_concurrent_event_appends() {
        let dir = tempfile::tempdir().unwrap();
        let first = Arc::new(LocalJsonlSessionMetadataStore::open(dir.path()).unwrap());
        let second = Arc::new(LocalJsonlSessionMetadataStore::open(dir.path()).unwrap());
        first.upsert_identity(identity("thread-1")).await.unwrap();

        let mut tasks = Vec::new();
        for index in 0..64 {
            let store = if index % 2 == 0 {
                Arc::clone(&first)
            } else {
                Arc::clone(&second)
            };
            tasks.push(tokio::spawn(async move {
                store
                    .append_event(
                        "thread-1",
                        SessionEvent::new(SessionEventKind::ToolCompleted, 1),
                    )
                    .await
                    .unwrap()
                    .seq
            }));
        }

        let mut assigned = Vec::new();
        for task in tasks {
            assigned.push(task.await.unwrap());
        }
        let unique = assigned.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), assigned.len());
        assert_eq!(
            unique.into_iter().collect::<Vec<_>>(),
            (1..=64).collect::<Vec<_>>()
        );

        assert_eq!(first.event_count("thread-1").await.unwrap(), 64);
        assert_eq!(second.event_count("thread-1").await.unwrap(), 64);
    }

    #[tokio::test]
    async fn local_store_reloads_notes_summaries_and_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        store
            .append_note(
                "thread-1",
                SessionNote {
                    id: String::new(),
                    title: "Decision".to_string(),
                    body: "Persist metadata.".to_string(),
                    tags: Vec::new(),
                    reason: "Resume needs it.".to_string(),
                    source_refs: vec![SessionSourceRef::Event { seq: 1 }],
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            )
            .await
            .unwrap();
        store
            .write_summary(
                "thread-1",
                SessionSummary {
                    id: String::new(),
                    title: "Setup".to_string(),
                    body: "Metadata store implemented.".to_string(),
                    covered_events: Some(SessionEventRange {
                        start_seq: 1,
                        end_seq: 2,
                    }),
                    covered_records: None,
                    source_refs: Vec::new(),
                    produced_by: Some("test".to_string()),
                    source_marker: None,
                },
            )
            .await
            .unwrap();
        store
            .write_checkpoint(
                "thread-1",
                SessionCheckpoint {
                    id: String::new(),
                    name: Some("before resume".to_string()),
                    kind: SessionCheckpointKind::Manual,
                    covered_events: Some(SessionEventRange {
                        start_seq: 1,
                        end_seq: 2,
                    }),
                    covered_records: None,
                    summary_id: None,
                    summary: None,
                    source_refs: Vec::new(),
                    created_at_ms: 2,
                },
            )
            .await
            .unwrap();

        let reopened = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened
                .read_note("thread-1", "note-1")
                .await
                .unwrap()
                .unwrap()
                .title,
            "Decision"
        );
        assert_eq!(
            reopened
                .read_summary("thread-1", "summary-1")
                .await
                .unwrap()
                .unwrap()
                .covered_events
                .unwrap()
                .end_seq,
            2
        );
        assert_eq!(
            reopened
                .list_checkpoints("thread-1")
                .await
                .unwrap()
                .first()
                .unwrap()
                .name
                .as_deref(),
            Some("before resume")
        );
    }

    #[tokio::test]
    async fn local_store_reloads_artifact_links_closure_and_identities() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        store.upsert_identity(identity("thread-1")).await.unwrap();
        store.upsert_identity(identity("thread-2")).await.unwrap();
        store
            .link_artifact(
                "thread-1",
                SessionArtifactLink {
                    artifact_id: "artifact-1".to_string(),
                    source_refs: vec![SessionSourceRef::Event { seq: 1 }],
                    promotion_policy: Some("manual".to_string()),
                    created_at_ms: 3,
                },
            )
            .await
            .unwrap();
        let closed = store
            .close_session(
                "thread-1",
                SessionClosure {
                    closed_at_ms: 5,
                    reason: Some("done".to_string()),
                    source_refs: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert!(!closed.already_closed);

        let reopened = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        let identities = reopened.list_identities().await.unwrap();
        assert_eq!(
            identities
                .iter()
                .map(|identity| identity.thread_id.as_str())
                .collect::<Vec<_>>(),
            vec!["thread-1", "thread-2"]
        );
        assert_eq!(
            reopened
                .list_artifact_links("thread-1")
                .await
                .unwrap()
                .first()
                .unwrap()
                .artifact_id,
            "artifact-1"
        );
        assert_eq!(
            reopened
                .closure("thread-1")
                .await
                .unwrap()
                .unwrap()
                .reason
                .as_deref(),
            Some("done")
        );
    }

    #[test]
    fn local_store_hashes_thread_ids_to_collision_resistant_log_paths() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        let upper = store.log_path("Thread-A").unwrap();
        let lower = store.log_path("thread-a").unwrap();
        let escaped = store.log_path("../secret").unwrap();

        assert_ne!(upper, lower);
        assert!(upper.starts_with(store.root()));
        assert!(lower.starts_with(store.root()));
        assert!(escaped.starts_with(store.root()));
        assert!(
            upper
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("session-")
        );
        assert!(
            upper
                .file_name()
                .unwrap()
                .to_string_lossy()
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '.')
        );
    }

    #[test]
    fn local_store_rejects_empty_thread_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        let err = store.log_path("").unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[test]
    fn local_store_reports_corrupt_log() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        fs::write(store.log_path("thread-1").unwrap(), "{not json\n").unwrap();
        let err = store.load_session("thread-1").unwrap_err();
        assert_eq!(err.kind(), "json");
    }

    #[tokio::test]
    async fn local_store_tolerates_torn_trailing_line_when_listing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        store.upsert_identity(identity("thread-1")).await.unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.log_path("thread-1").unwrap())
            .unwrap();
        file.write_all(b"{\"kind\":\"event\"").unwrap();
        file.flush().unwrap();

        let reopened = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        let identities = reopened.list_identities().await.unwrap();

        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].thread_id, "thread-1");
    }

    #[tokio::test]
    async fn local_store_listing_skips_one_corrupt_log() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        store.upsert_identity(identity("thread-1")).await.unwrap();
        fs::write(store.log_path("thread-bad").unwrap(), "{not json\n").unwrap();

        let identities = store.list_identities().await.unwrap();

        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].thread_id, "thread-1");
    }

    #[tokio::test]
    async fn local_store_reloads_external_changes_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let first = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        let second = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        first.upsert_identity(identity("thread-1")).await.unwrap();
        assert_eq!(first.event_count("thread-1").await.unwrap(), 0);
        second
            .append_event("thread-1", SessionEvent::new(SessionEventKind::Started, 1))
            .await
            .unwrap();

        assert_eq!(first.event_count("thread-1").await.unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn local_store_rejects_symlink_log() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let store = LocalJsonlSessionMetadataStore::open(dir.path()).unwrap();
        symlink(outside.path(), store.log_path("thread-1").unwrap()).unwrap();
        let err = store.load_session("thread-1").unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }
}
