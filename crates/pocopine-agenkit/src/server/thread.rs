//! Agent thread state (RFC-093 Phase 2.6, §D4, §D10).
//!
//! Thread state is opt-in. The [`AgentThreadStore`] trait is behind a runtime
//! seam so v1 ships an in-memory store while a future durable store can drop
//! in. Thread ids exposed to callers are opaque Pocopine ids; provider-backed
//! ids (if any) never leak (§D10).
//!
//! Threads are **owner-scoped**: every store access carries a [`ThreadOwner`]
//! derived from the caller [`Principal`], so resuming a thread can never expose
//! another principal's conversation history. The owner travels through the
//! store seam (not just the resume path) so durable backends enforce the same
//! invariant — and so adding it stays a cheap trait change now rather than a
//! breaking one once durable stores exist (issue #214).

use std::sync::Arc;

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, AgentThreadId, Message, StepKind, StepStatus, ThreadRetention,
    events,
};
use pocopine_auth::Principal;

use super::provider::BoxFuture;
use super::run::RunState;
use super::session::{self, RecordKind, SessionStore};

/// An opaque key identifying the principal that owns a thread.
///
/// Derived from the request [`Principal`]: the authenticated user id, or
/// `None` for anonymous callers. Anonymous callers all share one bucket
/// because there is no identity to scope a thread to — the security property
/// this enforces is "an authenticated user cannot read another principal's
/// thread", which is exactly the cross-user leak issue #214 closes.
///
/// The key is borrowed so passing it through the store seam never allocates;
/// stores convert it to an owned key ([`ThreadOwner::key`]) when persisting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadOwner<'a> {
    key: Option<&'a str>,
}

impl<'a> ThreadOwner<'a> {
    /// The owner key for a request principal.
    pub fn from_principal(principal: &'a Principal) -> Self {
        Self {
            key: principal.user().map(|user| user.id.as_str()),
        }
    }

    /// The anonymous owner (no authenticated user).
    pub fn anonymous() -> Self {
        Self { key: None }
    }

    /// The borrowed owner key, `None` for anonymous callers. Durable stores
    /// persist and compare against this; the in-memory store does too.
    pub fn key(&self) -> Option<&str> {
        self.key
    }

    /// Rebuild an owner from a previously captured key (used by the thread
    /// handle to replay its owner on later reads/appends).
    pub(crate) fn from_opt(key: Option<&'a str>) -> Self {
        Self { key }
    }
}

/// A thread's listing metadata — what an owner-scoped enumeration (e.g. a chat
/// sidebar) needs: identity, parentage (to rebuild the fork tree), creation
/// time, and the creator-attached attributes (the agent id, plus anything set
/// via [`update_attributes`](AgentThreadStore::update_attributes), e.g. a
/// title).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentThreadMeta {
    /// The opaque thread id.
    pub id: AgentThreadId,
    /// The thread this one forked from, `None` for a root thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<AgentThreadId>,
    /// When the thread was created (Unix ms).
    pub created_ms: u64,
    /// The thread's attributes, verbatim.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub attributes: serde_json::Value,
}

/// Durable-or-not storage for agent threads.
///
/// Every method carries the caller's [`ThreadOwner`] so the store can scope
/// access to the principal that created the thread. Implementations MUST treat
/// a thread owned by a different owner as inaccessible: `load` returns `None`
/// (indistinguishable from a missing thread — no existence oracle), and
/// `append`/`delete` must not touch it.
pub trait AgentThreadStore: Send + Sync + 'static {
    /// Create a new thread for `agent_id` owned by `owner`, returning its
    /// opaque id.
    fn create(
        &self,
        agent_id: &str,
        owner: ThreadOwner<'_>,
        retention: ThreadRetention,
    ) -> BoxFuture<'_, AgenkitResult<AgentThreadId>>;

    /// Load a thread's message history if it exists **and** is owned by
    /// `owner`; otherwise `None`.
    fn load(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Option<Vec<Message>>>>;

    /// Append messages to a thread owned by `owner`.
    fn append(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        messages: Vec<Message>,
    ) -> BoxFuture<'_, AgenkitResult<()>>;

    /// Delete a thread and its state if it is owned by `owner`. A missing or
    /// foreign thread is a silent no-op; a thread that still has forks is
    /// **refused** (deleting it would orphan them — delete the forks first).
    fn delete(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<()>>;

    /// Append a compaction checkpoint summarizing the folded history, keeping
    /// `kept` (the most recent messages) verbatim in the active context. A
    /// session-backed store records a real checkpoint so [`active_history`] can
    /// resume from `[summary, ...kept]`; the default just appends `summary` as a
    /// message (no compaction — `kept` is already in the full history, so it is
    /// ignored here).
    ///
    /// [`active_history`]: AgentThreadStore::active_history
    fn checkpoint(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        summary: Message,
        kept: Vec<Message>,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        let _ = kept;
        self.append(id, owner, vec![summary])
    }

    /// The message history to send the model: the **compacted** view (from the
    /// last checkpoint) when the store supports compaction, else the full
    /// history (the default — same as [`AgentThreadStore::load`]).
    fn active_history(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Option<Vec<Message>>>> {
        self.load(id, owner)
    }

    /// Fork a new branch from this thread's current state — a new thread that
    /// inherits the full history and owner. `Ok(None)` when the store doesn't
    /// support branching (the default) or the thread isn't accessible to `owner`.
    fn fork(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Option<AgentThreadId>>> {
        let _ = (id, owner);
        Box::pin(async move { Ok(None) })
    }

    /// The threads owned by `owner`, newest-first (by creation time), with
    /// parent links so a caller can rebuild the fork tree (a chat sidebar's
    /// query). The default is empty — a store that doesn't support enumeration
    /// simply lists nothing (the `fork` → `Ok(None)` convention).
    fn list(&self, owner: ThreadOwner<'_>) -> BoxFuture<'_, AgenkitResult<Vec<AgentThreadMeta>>> {
        let _ = owner;
        Box::pin(async move { Ok(Vec::new()) })
    }

    /// Merge `patch` (a JSON object) into the attributes of a thread owned by
    /// `owner` — how a caller stores mutable thread state like a title or a
    /// `kind`. Shallow merge: each patch key replaces that attribute; a `null`
    /// value **removes** it. The runtime-owned keys (`owner`, `agent_id`) are
    /// reserved — a patch naming them is rejected, so the owner scope can never
    /// be rewritten through this path. A missing/foreign thread is
    /// `not_found` (no existence oracle). The default errors — a store that
    /// doesn't support mutation must not silently drop the update.
    fn update_attributes(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        patch: serde_json::Value,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        let _ = (id, owner, patch);
        Box::pin(async move {
            Err(AgenkitError::config(
                "thread store does not support attribute updates",
            ))
        })
    }

    /// Record a `state-change` provenance entry (model / usage / cost) alongside
    /// the messages, *without* placing it in the conversation history (it never
    /// reaches the model). The default is a no-op — provenance is best-effort, so
    /// a store that doesn't model it simply drops it. A session-backed store writes
    /// a [`RecordKind::StateChange`] record.
    fn append_state_change(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        data: serde_json::Value,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        let _ = (id, owner, data);
        Box::pin(async move { Ok(()) })
    }

    /// The `state-change` provenance entries for a thread, in order (the inverse of
    /// [`append_state_change`](AgentThreadStore::append_state_change)). The default
    /// is empty — a store that doesn't record provenance has none to return.
    fn state_changes(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Vec<serde_json::Value>>> {
        let _ = (id, owner);
        Box::pin(async move { Ok(Vec::new()) })
    }
}

/// The default agent-thread store: agent threads run **on the durable session
/// layer** (roadmap W7). Each agent thread is a [`session`] thread; messages are
/// `Message` records in its log; the owning principal is persisted in the
/// thread's `attributes`, so the owner-scope (issue #214) survives a restart.
///
/// Swap the backing [`SessionStore`] for durability:
/// [`SessionThreadStore::in_memory`] (the default — ephemeral) or
/// `SessionThreadStore::new(Arc::new(JsonlSessionStore::new(dir)))`.
pub struct SessionThreadStore {
    sessions: Arc<dyn SessionStore>,
}

impl SessionThreadStore {
    /// Build over any session store.
    pub fn new(sessions: Arc<dyn SessionStore>) -> Self {
        Self { sessions }
    }

    /// An in-memory store (no durability) — the runtime default.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(session::MemorySessionStore::new()))
    }

    /// The owner string persisted on a thread, if any.
    fn stored_owner(attributes: &serde_json::Value) -> Option<&str> {
        attributes.get("owner").and_then(serde_json::Value::as_str)
    }

    /// Owner-checked read of a thread's messages from the **materialized**
    /// history (so a forked thread includes its inherited prefix). `compacted`
    /// returns the view from the last checkpoint (the checkpoint's summary leads);
    /// otherwise the full message history. `None` = missing or foreign owner.
    fn read(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        compacted: bool,
    ) -> BoxFuture<'_, AgenkitResult<Option<Vec<Message>>>> {
        let sessions = self.sessions.clone();
        let tid = session::ThreadId::new(id.as_str());
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            if owner_checked_meta(sessions.as_ref(), &tid, want.as_deref())
                .await?
                .is_none()
            {
                return Ok(None);
            }
            let handle = session::Session::open(sessions.clone(), tid);
            let records = if compacted {
                handle.active_context().await
            } else {
                handle.history().await
            }
            .map_err(session_err)?;
            let mut messages = Vec::new();
            for record in records {
                match record.kind {
                    // Full view = Message records only.
                    RecordKind::Message => {
                        let message: Message =
                            serde_json::from_value(record.data).map_err(|e| {
                                AgenkitError::internal(format!("thread message decode: {e}"))
                            })?;
                        messages.push(message);
                    }
                    // Compacted view: expand the leading Checkpoint into its
                    // summary followed by the verbatim tail kept live.
                    RecordKind::Checkpoint if compacted => {
                        let payload = CheckpointPayload::decode(record.data)?;
                        messages.push(payload.summary);
                        messages.extend(payload.kept);
                    }
                    RecordKind::Checkpoint | RecordKind::StateChange => {}
                }
            }
            Ok(Some(messages))
        })
    }
}

/// The single owner-scope gate: load a thread's meta only if it exists **and**
/// belongs to `want`, returning `None` for a missing or foreign thread (no
/// existence oracle). Every `SessionThreadStore` method routes through this, so
/// the cross-principal guard (issue #214) can't be forgotten by one method.
async fn owner_checked_meta(
    sessions: &dyn SessionStore,
    tid: &session::ThreadId,
    want: Option<&str>,
) -> AgenkitResult<Option<session::ThreadMeta>> {
    match sessions.meta(tid).await.map_err(session_err)? {
        Some(meta) if SessionThreadStore::stored_owner(&meta.attributes) == want => Ok(Some(meta)),
        _ => Ok(None),
    }
}

/// Map a session-store failure to an opaque internal agenkit error (§D10 — no
/// backend detail crosses to a client through the boundary).
fn session_err(err: session::SessionError) -> AgenkitError {
    AgenkitError::internal(format!("thread store: {err}"))
}

/// The payload of a compaction [`RecordKind::Checkpoint`] in a session-backed
/// thread: the summary of the folded prefix, plus the most recent messages kept
/// verbatim so the model still sees its live context un-paraphrased.
/// [`active_history`](AgentThreadStore::active_history) expands it to
/// `[summary, ...kept]`.
#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointPayload {
    summary: Message,
    #[serde(default)]
    kept: Vec<Message>,
}

impl CheckpointPayload {
    /// Decode a checkpoint record. A failure is a hard error (a corrupt/truncated
    /// checkpoint surfaces, rather than silently degrading to "treat the bad JSON
    /// as a summary" — there is no legacy on-disk format to tolerate).
    fn decode(data: serde_json::Value) -> AgenkitResult<Self> {
        serde_json::from_value(data)
            .map_err(|e| AgenkitError::internal(format!("thread checkpoint decode: {e}")))
    }
}

impl AgentThreadStore for SessionThreadStore {
    fn create(
        &self,
        agent_id: &str,
        owner: ThreadOwner<'_>,
        _retention: ThreadRetention,
    ) -> BoxFuture<'_, AgenkitResult<AgentThreadId>> {
        // The owner is persisted on the thread so the scope survives a restart.
        let sessions = self.sessions.clone();
        let attributes = serde_json::json!({ "owner": owner.key(), "agent_id": agent_id });
        Box::pin(async move {
            let meta = sessions
                .create_thread(None, attributes)
                .await
                .map_err(session_err)?;
            Ok(AgentThreadId::new(meta.id.as_str()))
        })
    }

    fn load(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Option<Vec<Message>>>> {
        // The full message history (a foreign owner reads as not-found).
        self.read(id, owner, false)
    }

    fn active_history(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Option<Vec<Message>>>> {
        // The compacted view: from the last checkpoint (its summary leads).
        self.read(id, owner, true)
    }

    fn append(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        messages: Vec<Message>,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        let sessions = self.sessions.clone();
        let tid = session::ThreadId::new(id.as_str());
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            // Missing, or owned by someone else: refuse without revealing which.
            if owner_checked_meta(sessions.as_ref(), &tid, want.as_deref())
                .await?
                .is_none()
            {
                return Err(AgenkitError::not_found("thread not found"));
            }
            for message in messages {
                let data = serde_json::to_value(&message)
                    .map_err(|e| AgenkitError::internal(format!("thread message encode: {e}")))?;
                sessions
                    .append(&tid, RecordKind::Message, data)
                    .await
                    .map_err(session_err)?;
            }
            Ok(())
        })
    }

    fn delete(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        // Idempotent: only the owner can delete; a foreign/missing id is a
        // silent no-op so deletion never doubles as an existence oracle.
        let sessions = self.sessions.clone();
        let tid = session::ThreadId::new(id.as_str());
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            if owner_checked_meta(sessions.as_ref(), &tid, want.as_deref())
                .await?
                .is_some()
            {
                sessions.delete_thread(&tid).await.map_err(session_err)?;
            }
            Ok(())
        })
    }

    fn checkpoint(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        summary: Message,
        kept: Vec<Message>,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        let sessions = self.sessions.clone();
        let tid = session::ThreadId::new(id.as_str());
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            // Owner-scoped: refuse a missing/foreign thread (no existence oracle).
            if owner_checked_meta(sessions.as_ref(), &tid, want.as_deref())
                .await?
                .is_none()
            {
                return Err(AgenkitError::not_found("thread not found"));
            }
            // A `Checkpoint` record carrying the summary plus the verbatim tail:
            // `active_history` expands it to `[summary, ...kept]`, while the full
            // history (and any fork) still retains every original record. The
            // kept tail lives only inside the payload — it is not re-appended, so
            // the log never duplicates the messages it folds.
            let payload = CheckpointPayload { summary, kept };
            let data = serde_json::to_value(&payload)
                .map_err(|e| AgenkitError::internal(format!("thread checkpoint encode: {e}")))?;
            sessions
                .append(&tid, RecordKind::Checkpoint, data)
                .await
                .map_err(session_err)?;
            Ok(())
        })
    }

    fn fork(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Option<AgentThreadId>>> {
        let sessions = self.sessions.clone();
        let tid = session::ThreadId::new(id.as_str());
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            let Some(meta) = owner_checked_meta(sessions.as_ref(), &tid, want.as_deref()).await?
            else {
                return Ok(None);
            };
            // Fork from the current end: the child inherits the full record range
            // and the parent's attributes (owner); the parent is untouched.
            let child = session::Session::open(sessions.clone(), tid)
                .fork(meta.last_seq)
                .await
                .map_err(session_err)?;
            Ok(Some(AgentThreadId::new(child.id().as_str())))
        })
    }

    fn list(&self, owner: ThreadOwner<'_>) -> BoxFuture<'_, AgenkitResult<Vec<AgentThreadMeta>>> {
        let sessions = self.sessions.clone();
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            let mut metas: Vec<session::ThreadMeta> = sessions
                .threads()
                .await
                .map_err(session_err)?
                .into_iter()
                .filter(|m| Self::stored_owner(&m.attributes) == want.as_deref())
                .collect();
            // Newest-first; the id tiebreak keeps the order deterministic when
            // several threads share a creation millisecond.
            metas.sort_by(|a, b| {
                b.created_ms
                    .cmp(&a.created_ms)
                    .then_with(|| b.id.cmp(&a.id))
            });
            Ok(metas
                .into_iter()
                .map(|m| AgentThreadMeta {
                    id: AgentThreadId::new(m.id.as_str()),
                    parent: m.parent.map(|p| AgentThreadId::new(p.parent.as_str())),
                    created_ms: m.created_ms,
                    attributes: m.attributes,
                })
                .collect())
        })
    }

    fn update_attributes(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        patch: serde_json::Value,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        let sessions = self.sessions.clone();
        let tid = session::ThreadId::new(id.as_str());
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            let Some(object) = patch.as_object() else {
                return Err(AgenkitError::validation(
                    "thread attribute patch must be a JSON object",
                ));
            };
            // The scope/identity keys are runtime-owned: rejecting them here
            // (not silently skipping) keeps a patch's effect exactly what it
            // says, and the owner scope unrewritable through this path.
            for reserved in ["owner", "agent_id"] {
                if object.contains_key(reserved) {
                    return Err(AgenkitError::validation(format!(
                        "thread attribute `{reserved}` is reserved"
                    )));
                }
            }
            // Owner-scoped, like every other mutation (no existence oracle).
            let Some(meta) = owner_checked_meta(sessions.as_ref(), &tid, want.as_deref()).await?
            else {
                return Err(AgenkitError::not_found("thread not found"));
            };
            let mut merged = match meta.attributes {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            for (key, value) in object {
                if value.is_null() {
                    merged.remove(key);
                } else {
                    merged.insert(key.clone(), value.clone());
                }
            }
            sessions
                .set_attributes(&tid, serde_json::Value::Object(merged))
                .await
                .map_err(session_err)?;
            // Audit trail: the patch rides the provenance channel (it never
            // reaches the model), so who-renamed-what is reconstructable from
            // the durable log.
            let audit = serde_json::json!({ "kind": "attributes", "patch": patch });
            sessions
                .append(&tid, RecordKind::StateChange, audit)
                .await
                .map_err(session_err)?;
            Ok(())
        })
    }

    fn append_state_change(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        data: serde_json::Value,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        let sessions = self.sessions.clone();
        let tid = session::ThreadId::new(id.as_str());
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            // Owner-scoped, like every other mutation (no existence oracle).
            if owner_checked_meta(sessions.as_ref(), &tid, want.as_deref())
                .await?
                .is_none()
            {
                return Err(AgenkitError::not_found("thread not found"));
            }
            sessions
                .append(&tid, RecordKind::StateChange, data)
                .await
                .map_err(session_err)?;
            Ok(())
        })
    }

    fn state_changes(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Vec<serde_json::Value>>> {
        let sessions = self.sessions.clone();
        let tid = session::ThreadId::new(id.as_str());
        let want = owner.key().map(str::to_string);
        Box::pin(async move {
            if owner_checked_meta(sessions.as_ref(), &tid, want.as_deref())
                .await?
                .is_none()
            {
                return Ok(Vec::new()); // missing/foreign reads as empty
            }
            // This thread's OWN records, NOT the materialized parent chain. Each
            // turn's provenance is recorded on exactly one thread, so a fork reports
            // only its own usage — summing across the tree never double-counts the
            // shared prefix (a fork inherits the parent's *messages* for replay, but
            // not its already-counted usage). It also avoids materializing the full
            // ancestor log just to keep a handful of provenance rows.
            let records = sessions.records(&tid).await.map_err(session_err)?;
            Ok(records
                .into_iter()
                .filter(|r| r.kind == RecordKind::StateChange)
                .map(|r| r.data)
                .collect())
        })
    }
}

/// A handle to a thread within a flow run: its id, the store to reach it, and
/// the owner key captured when the thread was opened.
#[derive(Clone)]
pub struct AgentThreadHandle {
    pub(crate) id: AgentThreadId,
    pub(crate) store: Arc<dyn AgentThreadStore>,
    /// Owner key captured at create/resume time, replayed on every store
    /// access so later reads/appends stay scoped to the opening principal.
    pub(crate) owner: Option<String>,
}

impl AgentThreadHandle {
    /// The opaque thread id.
    pub fn id(&self) -> &AgentThreadId {
        &self.id
    }

    /// The owner this handle accesses the store under.
    pub(crate) fn owner(&self) -> ThreadOwner<'_> {
        ThreadOwner::from_opt(self.owner.as_deref())
    }

    /// Load the full message history.
    pub async fn history(&self) -> AgenkitResult<Vec<Message>> {
        Ok(self
            .store
            .load(&self.id, self.owner())
            .await?
            .unwrap_or_default())
    }

    /// The compacted history to send the model (from the last checkpoint).
    pub async fn active_history(&self) -> AgenkitResult<Vec<Message>> {
        Ok(self
            .store
            .active_history(&self.id, self.owner())
            .await?
            .unwrap_or_default())
    }

    /// Append a compaction checkpoint: the summary of the folded prefix plus
    /// `kept`, the recent messages retained verbatim in the active context.
    pub async fn checkpoint(&self, summary: Message, kept: Vec<Message>) -> AgenkitResult<()> {
        self.store
            .checkpoint(&self.id, self.owner(), summary, kept)
            .await
    }

    /// Fork a new branch from this thread's current state — a handle to a new
    /// thread that inherits the full history. `None` if the store can't branch.
    pub async fn fork(&self) -> AgenkitResult<Option<AgentThreadHandle>> {
        let owner = self.owner.clone();
        Ok(self
            .store
            .fork(&self.id, self.owner())
            .await?
            .map(|id| AgentThreadHandle {
                id,
                store: self.store.clone(),
                owner,
            }))
    }

    /// Merge `patch` into the thread's attributes (e.g. a title) — see
    /// [`AgentThreadStore::update_attributes`] for the merge semantics and the
    /// reserved keys.
    pub async fn update_attributes(&self, patch: serde_json::Value) -> AgenkitResult<()> {
        self.store
            .update_attributes(&self.id, self.owner(), patch)
            .await
    }

    /// Record a provenance entry (model / usage / cost) outside the conversation
    /// history (see [`AgentThreadStore::append_state_change`]).
    pub async fn append_state_change(&self, data: serde_json::Value) -> AgenkitResult<()> {
        self.store
            .append_state_change(&self.id, self.owner(), data)
            .await
    }

    /// The provenance entries recorded for this thread, in order.
    pub async fn state_changes(&self) -> AgenkitResult<Vec<serde_json::Value>> {
        self.store.state_changes(&self.id, self.owner()).await
    }
}

/// Builder returned by `ctx.thread::<A>()`.
pub struct ThreadBuilder {
    run: Arc<RunState>,
    // `A::ID` is a `&'static str` declared once on the agent — carry it by
    // reference, never an owned/cloned `String`.
    agent_id: &'static str,
}

impl ThreadBuilder {
    pub(crate) fn new(run: Arc<RunState>, agent_id: &'static str) -> Self {
        Self { run, agent_id }
    }

    /// The owner key for this run's caller principal.
    fn owner(&self) -> ThreadOwner<'_> {
        ThreadOwner::from_principal(&self.run.principal)
    }

    /// Create a fresh thread for the agent, owned by the caller principal.
    pub async fn create(&self) -> AgenkitResult<AgentThreadHandle> {
        let store = self.run.inner.thread_store.clone();
        let owner = self.owner();
        let id = store
            .create(self.agent_id, owner, ThreadRetention::Session)
            .await?;
        self.run.emit(
            self.run
                .event(
                    events::AI_THREAD_CREATED,
                    StepKind::Custom,
                    StepStatus::Completed,
                )
                .with_field("thread_id", id.as_str())
                .with_field("agent_id", self.agent_id),
        );
        Ok(AgentThreadHandle {
            id,
            store,
            owner: owner.key().map(str::to_string),
        })
    }

    /// Resume the thread with `id` if it exists **and is owned by the caller**,
    /// otherwise create a new one.
    ///
    /// Resuming a thread that belongs to a different principal is rejected: the
    /// owner-scoped `load` reads it as missing, so the caller falls through to
    /// a fresh thread instead of inheriting another principal's history.
    pub async fn resume_or_create(
        &self,
        id: Option<AgentThreadId>,
    ) -> AgenkitResult<AgentThreadHandle> {
        let store = self.run.inner.thread_store.clone();
        let owner = self.owner();
        if let Some(id) = id
            && store.load(&id, owner).await?.is_some()
        {
            self.run.emit(
                self.run
                    .event(
                        events::AI_THREAD_RESUMED,
                        StepKind::Custom,
                        StepStatus::Completed,
                    )
                    .with_field("thread_id", id.as_str())
                    .with_field("agent_id", self.agent_id),
            );
            return Ok(AgentThreadHandle {
                id,
                store,
                owner: owner.key().map(str::to_string),
            });
        }
        self.create().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_agenkit_core::{Content, ContentPart, Role, ToolCall};
    use pocopine_auth::AuthUser;

    fn alice() -> Principal {
        Principal::from_user(AuthUser::new("alice"))
    }

    fn bob() -> Principal {
        Principal::from_user(AuthUser::new("bob"))
    }

    #[tokio::test]
    async fn session_thread_store_round_trips() {
        let store = SessionThreadStore::in_memory();
        let owner = ThreadOwner::anonymous();
        let id = store
            .create("debugger", owner, ThreadRetention::Session)
            .await
            .unwrap();
        store
            .append(
                &id,
                owner,
                vec![Message::new(Role::User, "why did it fail?")],
            )
            .await
            .unwrap();
        let history = store.load(&id, owner).await.unwrap().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::User);
        store.delete(&id, owner).await.unwrap();
        assert!(store.load(&id, owner).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn full_fidelity_messages_round_trip_through_the_store() {
        // The P4 resume-fidelity guard: a complete tool turn survives a store
        // round trip byte-for-byte — tool_calls, the tool_call_id linkage, AND a
        // reasoning signature (all of which the old lossy `ThreadMessage` dropped).
        let store = SessionThreadStore::in_memory();
        let owner = ThreadOwner::anonymous();
        let id = store
            .create("a", owner, ThreadRetention::Session)
            .await
            .unwrap();

        let original = vec![
            Message::user("weather?"),
            // An assistant tool-call turn carrying reasoning + a provider signature.
            Message::assistant_tool_calls(
                Content::from_parts(vec![
                    ContentPart::thinking("let me check the tool", Some("sig-xyz".to_string())),
                    ContentPart::text("calling echo"),
                ]),
                vec![ToolCall::new(
                    "call-1",
                    "echo",
                    serde_json::json!({ "text": "x" }),
                )],
            ),
            // A tool result, linked to the call by id.
            Message::tool_result("call-1", "{\"echoed\":\"x\"}"),
            Message::assistant("it's sunny"),
        ];
        store.append(&id, owner, original.clone()).await.unwrap();

        // Reload via the resume path → byte-equal to what was stored.
        let reloaded = store.load(&id, owner).await.unwrap().unwrap();
        assert_eq!(reloaded, original);
    }

    #[tokio::test]
    async fn legacy_thread_message_records_decode_into_message() {
        // Forward-compat: records written before this work (old `ThreadMessage`
        // shape — role + content + ts_ms, no tool fields) still decode into
        // `Message`, with the new fields defaulting. No data migration needed.
        let sessions: Arc<dyn SessionStore> = Arc::new(session::MemorySessionStore::new());
        let store = SessionThreadStore::new(sessions.clone());
        let owner = ThreadOwner::anonymous();
        let id = store
            .create("a", owner, ThreadRetention::Session)
            .await
            .unwrap();

        // Append a raw legacy-shaped record straight to the underlying log.
        let tid = session::ThreadId::new(id.as_str());
        sessions
            .append(
                &tid,
                RecordKind::Message,
                serde_json::json!({
                    "role": "user",
                    "content": [{ "type": "text", "text": "legacy hi" }],
                    "ts_ms": 1_700_000_000_000u64,
                }),
            )
            .await
            .unwrap();

        let history = store.load(&id, owner).await.unwrap().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[0].content.as_text(), "legacy hi");
        assert!(history[0].tool_calls.is_empty());
        assert!(history[0].tool_call_id.is_none());
    }

    #[tokio::test]
    async fn foreign_principal_cannot_read_or_mutate_anothers_thread() {
        let store = SessionThreadStore::in_memory();
        let (alice_p, bob_p) = (alice(), bob());
        let alice = ThreadOwner::from_principal(&alice_p);
        let bob = ThreadOwner::from_principal(&bob_p);

        let id = store
            .create("debugger", alice, ThreadRetention::Session)
            .await
            .unwrap();
        store
            .append(&id, alice, vec![Message::new(Role::User, "secret")])
            .await
            .unwrap();

        // Bob cannot read Alice's thread — it reads as not-found.
        assert!(store.load(&id, bob).await.unwrap().is_none());
        // Bob cannot append to it.
        assert!(
            store
                .append(&id, bob, vec![Message::new(Role::User, "inject")])
                .await
                .is_err()
        );
        // Bob's delete is a silent no-op — Alice's thread survives intact.
        store.delete(&id, bob).await.unwrap();
        assert_eq!(store.load(&id, alice).await.unwrap().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn anonymous_and_authenticated_owners_are_distinct() {
        let store = SessionThreadStore::in_memory();
        let alice_p = alice();
        let anon = ThreadOwner::anonymous();
        let alice = ThreadOwner::from_principal(&alice_p);

        let id = store
            .create("debugger", anon, ThreadRetention::Session)
            .await
            .unwrap();
        // An authenticated caller cannot pick up an anonymous thread.
        assert!(store.load(&id, alice).await.unwrap().is_none());
        // But another anonymous caller shares the anonymous bucket.
        assert!(store.load(&id, anon).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn durable_thread_and_owner_scope_survive_a_reopen() {
        // The whole point of graduating onto the session layer: a JSONL-backed
        // thread (and its owner scope) survives a process restart.
        let dir = tempfile::tempdir().unwrap();
        let alice_p = alice();
        let bob_p = bob();
        let alice = ThreadOwner::from_principal(&alice_p);
        let bob = ThreadOwner::from_principal(&bob_p);

        // First "process": Alice creates a thread and appends a message.
        let id = {
            let store =
                SessionThreadStore::new(Arc::new(session::JsonlSessionStore::new(dir.path())));
            let id = store
                .create("debugger", alice, ThreadRetention::Durable)
                .await
                .unwrap();
            store
                .append(&id, alice, vec![Message::new(Role::User, "remembered")])
                .await
                .unwrap();
            id
        };

        // Second "process": a fresh store over the same dir.
        let store = SessionThreadStore::new(Arc::new(session::JsonlSessionStore::new(dir.path())));
        // Alice's history survived the restart.
        let history = store.load(&id, alice).await.unwrap().unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].content.as_text().contains("remembered"));
        // ...and so did the owner scope — Bob still can't read it.
        assert!(store.load(&id, bob).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn checkpoint_compacts_active_history_but_load_keeps_full() {
        let store = SessionThreadStore::in_memory();
        let owner = ThreadOwner::anonymous();
        let id = store
            .create("a", owner, ThreadRetention::Session)
            .await
            .unwrap();
        store
            .append(
                &id,
                owner,
                vec![
                    Message::new(Role::User, "q1"),
                    Message::new(Role::Assistant, "a1"),
                ],
            )
            .await
            .unwrap();
        // Fold q1 into the summary but keep a1 verbatim in the active context.
        store
            .checkpoint(
                &id,
                owner,
                Message::new(Role::System, "summary of q1"),
                vec![Message::new(Role::Assistant, "a1")],
            )
            .await
            .unwrap();
        store
            .append(&id, owner, vec![Message::new(Role::User, "q2")])
            .await
            .unwrap();

        // active_history resumes from the checkpoint, expanding it to its summary
        // plus the verbatim tail: [summary, a1, q2].
        let active = store.active_history(&id, owner).await.unwrap().unwrap();
        assert_eq!(active.len(), 3);
        assert_eq!(active[0].role, Role::System);
        assert_eq!(active[0].content.as_text(), "summary of q1");
        assert_eq!(active[1].content.as_text(), "a1"); // kept verbatim
        assert_eq!(active[2].content.as_text(), "q2");
        // load keeps the FULL message history (q1, a1, q2); the checkpoint record
        // is not itself a message, and the kept tail is NOT duplicated.
        let full = store.load(&id, owner).await.unwrap().unwrap();
        assert_eq!(full.len(), 3);
        assert_eq!(full[0].content.as_text(), "q1");
        assert_eq!(full[1].content.as_text(), "a1");
        assert_eq!(full[2].content.as_text(), "q2");
    }

    #[tokio::test]
    async fn list_returns_only_the_owners_threads_newest_first_with_parents() {
        let store = SessionThreadStore::in_memory();
        let (alice_p, bob_p) = (alice(), bob());
        let alice = ThreadOwner::from_principal(&alice_p);
        let bob = ThreadOwner::from_principal(&bob_p);

        let first = store
            .create("chat", alice, ThreadRetention::Durable)
            .await
            .unwrap();
        // Distinct creation timestamps so newest-first is observable.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let second = store
            .create("chat", alice, ThreadRetention::Durable)
            .await
            .unwrap();
        let foreign = store
            .create("chat", bob, ThreadRetention::Durable)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let branch = store.fork(&first, alice).await.unwrap().unwrap();

        let listed = store.list(alice).await.unwrap();
        let ids: Vec<&str> = listed.iter().map(|m| m.id.as_str()).collect();
        // Alice's three threads, newest-first; Bob's never appears.
        assert_eq!(ids, vec![branch.as_str(), second.as_str(), first.as_str()]);
        assert!(!ids.contains(&foreign.as_str()));
        // Parent links let a caller rebuild the fork tree.
        assert_eq!(listed[0].parent.as_ref().unwrap(), &first);
        assert!(listed[1].parent.is_none());
        // Attributes ride along (the agent id the runtime stored at create).
        assert_eq!(listed[2].attributes["agent_id"], "chat");

        // Bob's view is scoped to his own thread.
        let bobs = store.list(bob).await.unwrap();
        assert_eq!(bobs.len(), 1);
        assert_eq!(bobs[0].id, foreign);
        // Anonymous is its own bucket — none of the above.
        assert!(
            store
                .list(ThreadOwner::anonymous())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn update_attributes_merges_removes_and_guards_reserved_keys() {
        let store = SessionThreadStore::in_memory();
        let (alice_p, bob_p) = (alice(), bob());
        let alice = ThreadOwner::from_principal(&alice_p);
        let bob = ThreadOwner::from_principal(&bob_p);
        let id = store
            .create("chat", alice, ThreadRetention::Durable)
            .await
            .unwrap();

        // Merge in a title + kind; the runtime-owned keys survive untouched.
        store
            .update_attributes(
                &id,
                alice,
                serde_json::json!({ "title": "First chat", "kind": "chat" }),
            )
            .await
            .unwrap();
        let meta = &store.list(alice).await.unwrap()[0];
        assert_eq!(meta.attributes["title"], "First chat");
        assert_eq!(meta.attributes["kind"], "chat");
        assert_eq!(meta.attributes["agent_id"], "chat");

        // A later patch replaces one key and null-removes another.
        store
            .update_attributes(
                &id,
                alice,
                serde_json::json!({ "kind": "notebook", "title": null }),
            )
            .await
            .unwrap();
        let meta = &store.list(alice).await.unwrap()[0];
        assert_eq!(meta.attributes["kind"], "notebook");
        assert!(meta.attributes.get("title").is_none());

        // Reserved keys are rejected — the owner scope can't be rewritten.
        for patch in [
            serde_json::json!({ "owner": "bob" }),
            serde_json::json!({ "agent_id": "other" }),
            serde_json::json!("not an object"),
        ] {
            assert!(store.update_attributes(&id, alice, patch).await.is_err());
        }
        // A foreign principal can't mutate (reads as not-found)...
        assert!(
            store
                .update_attributes(&id, bob, serde_json::json!({ "title": "hijack" }))
                .await
                .is_err()
        );
        // ...and nothing above leaked through.
        let meta = &store.list(alice).await.unwrap()[0];
        assert_eq!(
            SessionThreadStore::stored_owner(&meta.attributes),
            Some("alice")
        );
        assert_eq!(meta.attributes["kind"], "notebook");

        // Each successful update left an audit entry on the provenance channel.
        let audits = store.state_changes(&id, alice).await.unwrap();
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0]["kind"], "attributes");
        assert_eq!(audits[0]["patch"]["title"], "First chat");
    }

    #[cfg(feature = "sqlite-session")]
    #[tokio::test]
    async fn list_and_update_attributes_work_on_the_sqlite_backend() {
        // The same owner-scoped listing + mutation through the indexed store.
        let store = SessionThreadStore::new(Arc::new(
            session::SqliteSessionStore::open_in_memory().unwrap(),
        ));
        let alice_p = alice();
        let alice = ThreadOwner::from_principal(&alice_p);
        let id = store
            .create("chat", alice, ThreadRetention::Durable)
            .await
            .unwrap();
        store
            .update_attributes(&id, alice, serde_json::json!({ "title": "sq" }))
            .await
            .unwrap();
        let listed = store.list(alice).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].attributes["title"], "sq");
    }

    #[tokio::test]
    async fn fork_branches_with_inherited_history_and_owner() {
        let store = SessionThreadStore::in_memory();
        let (alice_p, bob_p) = (alice(), bob());
        let alice = ThreadOwner::from_principal(&alice_p);
        let bob = ThreadOwner::from_principal(&bob_p);

        let id = store
            .create("a", alice, ThreadRetention::Session)
            .await
            .unwrap();
        store
            .append(
                &id,
                alice,
                vec![
                    Message::new(Role::User, "shared"),
                    Message::new(Role::Assistant, "shared-reply"),
                ],
            )
            .await
            .unwrap();

        // Bob cannot fork Alice's thread.
        assert!(store.fork(&id, bob).await.unwrap().is_none());

        // Alice forks: the branch inherits the history and the owner.
        let branch_id = store.fork(&id, alice).await.unwrap().unwrap();
        assert_ne!(branch_id, id);
        store
            .append(
                &branch_id,
                alice,
                vec![Message::new(Role::User, "branch-only")],
            )
            .await
            .unwrap();

        // The branch = inherited [shared, shared-reply] + its own [branch-only].
        let branch = store.load(&branch_id, alice).await.unwrap().unwrap();
        assert_eq!(branch.len(), 3);
        assert_eq!(branch[0].content.as_text(), "shared");
        assert_eq!(branch[2].content.as_text(), "branch-only");
        // Owner inherited — Bob still can't read the branch.
        assert!(store.load(&branch_id, bob).await.unwrap().is_none());
        // Parent untouched (still 2 messages).
        assert_eq!(store.load(&id, alice).await.unwrap().unwrap().len(), 2);
    }
}
