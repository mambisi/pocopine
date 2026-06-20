//! Durable, branchable **agent conversation sessions** (roadmap W7) — host-only.
//!
//! A session is the transcript an agent loop runs on. It is **single-writer by
//! construction**: the runtime that owns a run is the only writer; a sub-agent or
//! a parallel branch gets its *own* session, linked by reference. So this is a
//! **forest of single-writer append-only logs**, not a collaborative document —
//! no CRDT, no collab store. (Co-editing a shared *artifact* is a separate
//! concern, reached through a tool; it never backs the session.)
//!
//! The model, validated against open-source agents (OpenAI Codex, pi):
//! - a **thread** is an append-only log of [`Record`]s, totally ordered by `seq`;
//! - a **fork** is a *new* thread with a [`ParentLink`] (parent + split seq) — not
//!   a copy, not an in-place mutation; the parent is never touched;
//! - **resume / undo** fall out of the log (replay; move a leaf back);
//! - **compaction** appends a [`RecordKind::Checkpoint`] whose payload summarizes
//!   everything before it — [`Session::active_context`] starts from the last
//!   checkpoint, while [`Session::history`] keeps the full log (the checkpoint
//!   *replaces* in the active context, it does not *duplicate* prior history).
//!
//! Three [`SessionStore`] impls ship: [`MemorySessionStore`] (tests/dev),
//! [`JsonlSessionStore`] (a cat-able JSONL log per thread + a meta sidecar;
//! `children`/`last_seq` scan files), and [`SqliteSessionStore`] (the indexed
//! production store — `children`/`last_seq` are indexed queries, `append` is one
//! transactional `INSERT`). `delete_thread` refuses to orphan a fork (returns
//! [`SessionError::HasChildren`]); delete the children first.
//!
//! [`ExternalizingSessionStore`] wraps any of them to keep large tool outputs
//! **out-of-line** (anti-bloat rule #2): a record over a size threshold is
//! written to a [`BlobStore`] (content-addressed, so identical payloads dedup)
//! and the log keeps only a small reference, rehydrated transparently on read.
//!
//! ```no_run
//! # async fn demo() -> pocopine_agenkit::server::session::SessionResult<()> {
//! use std::sync::Arc;
//! use pocopine_agenkit::server::session::{MemorySessionStore, Session, SessionStore};
//!
//! let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
//! let chat = Session::create(store.clone(), None).await?;
//! chat.append_message(serde_json::json!({"role": "user", "text": "hi"})).await?;
//! chat.append_message(serde_json::json!({"role": "assistant", "text": "hello"})).await?;
//!
//! // Fork from after the first message — a new branch; the original is untouched.
//! let branch = chat.fork(1).await?;
//! branch.append_message(serde_json::json!({"role": "user", "text": "different"})).await?;
//! assert_eq!(chat.history().await?.len(), 2);    // unchanged
//! assert_eq!(branch.history().await?.len(), 2);  // [msg0 from parent] + [its own]
//! # Ok(()) }
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

mod blob;
mod jsonl;
mod memory;
mod sqlite;

pub use blob::{
    BlobStore, DEFAULT_BLOB_THRESHOLD, ExternalizingSessionStore, FsBlobStore, MemoryBlobStore,
};
pub use jsonl::JsonlSessionStore;
pub use memory::MemorySessionStore;
pub use sqlite::SqliteSessionStore;

/// An error from a session store.
#[derive(Debug)]
pub enum SessionError {
    /// The thread id does not exist.
    NotFound,
    /// The backing store failed (I/O, lock poisoned, …).
    Backend(String),
    /// Stored data could not be parsed (a corrupt log line / meta file).
    Corrupt(String),
    /// The thread can't be deleted because forks still point at it; removing it
    /// would orphan them (their materialized history would fail to resolve).
    /// Delete the children first.
    HasChildren,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::NotFound => f.write_str("session thread not found"),
            SessionError::Backend(m) => write!(f, "session backend: {m}"),
            SessionError::Corrupt(m) => write!(f, "session data corrupt: {m}"),
            SessionError::HasChildren => f.write_str("session thread has forks; delete them first"),
        }
    }
}

impl std::error::Error for SessionError {}

/// The result type for session operations.
pub type SessionResult<T> = Result<T, SessionError>;

/// The object-safe return shape for async [`SessionStore`] methods (host store
/// convention: a `Send` boxed future — see `AGENTS.md`).
pub type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = SessionResult<T>> + Send + 'a>>;

/// An opaque thread id (filename-safe). Mint one with [`ThreadId::mint`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadId(String);

impl ThreadId {
    /// Wrap an existing id.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Mint a fresh random id (base64url of 12 CSPRNG bytes — url/filename-safe).
    pub fn mint() -> SessionResult<Self> {
        let mut bytes = [0u8; 12];
        getrandom::getrandom(&mut bytes)
            .map_err(|e| SessionError::Backend(format!("no secure RNG: {e}")))?;
        Ok(Self(format!(
            "t-{}",
            pocopine_codec::base64url_encode(&bytes)
        )))
    }

    /// Borrow the id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an appended [`Record`] is. The session layer treats the payload as
/// opaque JSON — the agent owns its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A conversation message (user/assistant/tool — the agent's shape).
    Message,
    /// A change to the run's model / thinking level / tool-set (provenance).
    StateChange,
    /// A compaction checkpoint: the payload summarizes everything before it.
    Checkpoint,
}

/// One entry in a thread's append-only log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Position within this thread (0-based, monotonic). Total order *within* a
    /// branch; after a fork is materialized, ancestor records keep their own
    /// local seqs (order is positional).
    pub seq: u64,
    /// When the record was appended (Unix ms).
    pub timestamp_ms: u64,
    /// The kind of entry.
    pub kind: RecordKind,
    /// The opaque payload.
    pub data: serde_json::Value,
}

/// A child thread's link to where it forked from its parent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentLink {
    /// The parent thread.
    pub parent: ThreadId,
    /// The parent-local seq the fork split at: the child inherits parent records
    /// `[0, fork_at_seq)`.
    pub fork_at_seq: u64,
}

/// A thread's metadata (everything but its records).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadMeta {
    /// The thread id.
    pub id: ThreadId,
    /// Where this thread forked from, or `None` for a root thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ParentLink>,
    /// When the thread was created (Unix ms).
    pub created_ms: u64,
    /// Count of this thread's *local* records (the next append's seq).
    pub last_seq: u64,
    /// Opaque metadata the creator attached (e.g. the owning principal, agent
    /// id) — the session layer persists it verbatim and never interprets it. A
    /// fork inherits its parent's attributes.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub attributes: serde_json::Value,
}

/// Durable storage for session threads. The store is the source of truth; a
/// thread is a single-writer append-only log of [`Record`]s plus a [`ThreadMeta`].
pub trait SessionStore: Send + Sync {
    /// Create a new thread, optionally forked from a parent, with opaque
    /// `attributes` persisted on its [`ThreadMeta`]. Returns its meta.
    fn create_thread<'a>(
        &'a self,
        parent: Option<ParentLink>,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ThreadMeta>;

    /// Load a thread's metadata, or `None` if it doesn't exist.
    fn meta<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Option<ThreadMeta>>;

    /// Append a record to a thread; returns the stored record (with its `seq`).
    fn append<'a>(
        &'a self,
        id: &'a ThreadId,
        kind: RecordKind,
        data: serde_json::Value,
    ) -> SessionFuture<'a, Record>;

    /// This thread's *local* records (not its ancestors').
    fn records<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<Record>>;

    /// Threads whose parent is `id` (the branching tree).
    fn children<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<ThreadId>>;

    /// Delete a thread (its records + meta). Does not cascade to children;
    /// instead it **refuses** with [`SessionError::HasChildren`] when a fork
    /// still points at this thread, since deleting it would orphan the fork
    /// (its materialized history walks into the missing parent). Delete the
    /// children first. Deleting a missing thread is a no-op.
    fn delete_thread<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, ()>;
}

/// Current Unix time in milliseconds (0 if the clock is before the epoch).
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A backend failure from anything that displays (I/O, lock poison, …).
pub(crate) fn backend(e: impl std::fmt::Display) -> SessionError {
    SessionError::Backend(e.to_string())
}

/// An ergonomic handle to one thread on a store: append, checkpoint, fork, and
/// materialize history.
#[derive(Clone)]
pub struct Session {
    store: Arc<dyn SessionStore>,
    id: ThreadId,
}

impl Session {
    /// Open a handle to an existing thread id (no I/O).
    pub fn open(store: Arc<dyn SessionStore>, id: ThreadId) -> Self {
        Self { store, id }
    }

    /// Create a new root thread (no attributes) and return a handle to it. Use
    /// [`SessionStore::create_thread`] directly to attach metadata.
    pub async fn create(
        store: Arc<dyn SessionStore>,
        parent: Option<ParentLink>,
    ) -> SessionResult<Self> {
        let meta = store.create_thread(parent, serde_json::Value::Null).await?;
        Ok(Self { store, id: meta.id })
    }

    /// This session's thread id.
    pub fn id(&self) -> &ThreadId {
        &self.id
    }

    /// The store behind this session.
    pub fn store(&self) -> &Arc<dyn SessionStore> {
        &self.store
    }

    /// Append a conversation message.
    pub async fn append_message(&self, data: serde_json::Value) -> SessionResult<Record> {
        self.store.append(&self.id, RecordKind::Message, data).await
    }

    /// Append a state-change record (model/thinking/tool-set provenance).
    pub async fn append_state_change(&self, data: serde_json::Value) -> SessionResult<Record> {
        self.store
            .append(&self.id, RecordKind::StateChange, data)
            .await
    }

    /// Append a compaction checkpoint. `summary` is the compacted view of
    /// everything before it; [`Session::active_context`] resumes from here.
    /// The full history stays in the log — the checkpoint *replaces* in the
    /// active context, it does not duplicate prior records.
    pub async fn checkpoint(&self, summary: serde_json::Value) -> SessionResult<Record> {
        self.store
            .append(&self.id, RecordKind::Checkpoint, summary)
            .await
    }

    /// The full materialized history: this thread's records spliced onto its
    /// ancestors' (each ancestor contributes records up to its fork point).
    pub async fn history(&self) -> SessionResult<Vec<Record>> {
        materialize(self.store.as_ref(), &self.id).await
    }

    /// The active context to send the model: the suffix of [`Session::history`]
    /// from the last [`RecordKind::Checkpoint`] (inclusive), or the whole history
    /// when there's no checkpoint.
    ///
    /// Perf (tracked in #228): materializes the **full** history every call, then
    /// keeps only the suffix — so a long compacted thread pays an O(total records)
    /// read per turn for a small live window. Bounding it needs a store-level
    /// "records since the last checkpoint seq" path with fork-aware handling.
    pub async fn active_context(&self) -> SessionResult<Vec<Record>> {
        let full = self.history().await?;
        let start = full
            .iter()
            .rposition(|r| r.kind == RecordKind::Checkpoint)
            .unwrap_or(0);
        Ok(full[start..].to_vec())
    }

    /// Fork a new thread from this one at `at_seq` (clamped to this thread's
    /// local length). The new branch inherits records `[0, at_seq)` and the
    /// parent's attributes (e.g. its owner); this thread is untouched.
    pub async fn fork(&self, at_seq: u64) -> SessionResult<Self> {
        let meta = self
            .store
            .meta(&self.id)
            .await?
            .ok_or(SessionError::NotFound)?;
        let fork_at_seq = at_seq.min(meta.last_seq);
        let child = self
            .store
            .create_thread(
                Some(ParentLink {
                    parent: self.id.clone(),
                    fork_at_seq,
                }),
                meta.attributes,
            )
            .await?;
        Ok(Self {
            store: self.store.clone(),
            id: child.id,
        })
    }
}

/// Walk the parent chain root→leaf and splice each thread's records (ancestors
/// truncated at their fork point) into one ordered history.
async fn materialize(store: &dyn SessionStore, id: &ThreadId) -> SessionResult<Vec<Record>> {
    // Build the chain leaf→root: (thread, local-record limit). The leaf has no
    // limit; each ancestor is bounded by the child's fork point.
    let mut chain: Vec<(ThreadId, Option<u64>)> = Vec::new();
    let mut cur = id.clone();
    let mut limit: Option<u64> = None;
    loop {
        let meta = store.meta(&cur).await?.ok_or(SessionError::NotFound)?;
        chain.push((cur.clone(), limit));
        match meta.parent {
            Some(p) => {
                cur = p.parent;
                limit = Some(p.fork_at_seq);
            }
            None => break,
        }
    }
    chain.reverse(); // root first

    let mut out = Vec::new();
    for (tid, lim) in chain {
        let records = store.records(&tid).await?;
        let take = lim.map_or(records.len(), |l| (l as usize).min(records.len()));
        out.extend(records.into_iter().take(take));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Arc<dyn SessionStore> {
        Arc::new(MemorySessionStore::new())
    }

    #[tokio::test]
    async fn append_and_history_preserve_order() {
        let s = Session::create(mem(), None).await.unwrap();
        for i in 0..3 {
            s.append_message(serde_json::json!({ "i": i }))
                .await
                .unwrap();
        }
        let h = s.history().await.unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(h[1].data, serde_json::json!({ "i": 1 }));
    }

    #[tokio::test]
    async fn fork_inherits_prefix_and_leaves_parent_untouched() {
        let parent = Session::create(mem(), None).await.unwrap();
        parent
            .append_message(serde_json::json!({ "m": 0 }))
            .await
            .unwrap();
        parent
            .append_message(serde_json::json!({ "m": 1 }))
            .await
            .unwrap();
        parent
            .append_message(serde_json::json!({ "m": 2 }))
            .await
            .unwrap();

        // Fork after the first message (inherits [m0]).
        let child = parent.fork(1).await.unwrap();
        child
            .append_message(serde_json::json!({ "c": 0 }))
            .await
            .unwrap();

        let ph = parent.history().await.unwrap();
        let ch = child.history().await.unwrap();
        // Parent unchanged: m0, m1, m2.
        assert_eq!(ph.len(), 3);
        assert_eq!(ph[2].data, serde_json::json!({ "m": 2 }));
        // Child: inherited m0 + its own c0 (NOT m1/m2).
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0].data, serde_json::json!({ "m": 0 }));
        assert_eq!(ch[1].data, serde_json::json!({ "c": 0 }));
    }

    #[tokio::test]
    async fn checkpoint_compacts_active_context_but_keeps_full_history() {
        let s = Session::create(mem(), None).await.unwrap();
        for i in 0..3 {
            s.append_message(serde_json::json!({ "old": i }))
                .await
                .unwrap();
        }
        s.checkpoint(serde_json::json!({ "summary": "3 old turns" }))
            .await
            .unwrap();
        s.append_message(serde_json::json!({ "new": 0 }))
            .await
            .unwrap();

        // Full history retains everything (3 old + checkpoint + 1 new = 5).
        assert_eq!(s.history().await.unwrap().len(), 5);
        // Active context starts at the checkpoint: [checkpoint, new0].
        let active = s.active_context().await.unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].kind, RecordKind::Checkpoint);
        assert_eq!(active[1].data, serde_json::json!({ "new": 0 }));
    }

    #[tokio::test]
    async fn children_lists_forks() {
        let store = mem();
        let parent = Session::create(store.clone(), None).await.unwrap();
        parent.append_message(serde_json::json!({})).await.unwrap();
        let a = parent.fork(1).await.unwrap();
        let b = parent.fork(1).await.unwrap();

        let mut kids = store.children(parent.id()).await.unwrap();
        kids.sort();
        let mut want = vec![a.id().clone(), b.id().clone()];
        want.sort();
        assert_eq!(kids, want);
    }

    #[tokio::test]
    async fn fork_seq_is_clamped_to_thread_length() {
        let parent = Session::create(mem(), None).await.unwrap();
        parent
            .append_message(serde_json::json!({ "m": 0 }))
            .await
            .unwrap();
        // Ask to fork past the end → clamped to 1; inherits the single record.
        let child = parent.fork(999).await.unwrap();
        assert_eq!(child.history().await.unwrap().len(), 1);
    }
}
