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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, AgentThreadId, StepKind, StepStatus, ThreadMessage,
    ThreadRetention, events,
};
use pocopine_auth::Principal;

use super::provider::BoxFuture;
use super::run::RunState;

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
    ) -> BoxFuture<'_, AgenkitResult<Option<Vec<ThreadMessage>>>>;

    /// Append messages to a thread owned by `owner`.
    fn append(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        messages: Vec<ThreadMessage>,
    ) -> BoxFuture<'_, AgenkitResult<()>>;

    /// Delete a thread and its state if it is owned by `owner`.
    fn delete(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<()>>;
}

/// One stored thread: its owner key plus message history.
struct ThreadEntry {
    owner: Option<String>,
    messages: Vec<ThreadMessage>,
}

/// The default in-memory thread store (tests, examples, single-process apps).
#[derive(Default)]
pub struct InMemoryThreadStore {
    threads: Mutex<HashMap<String, ThreadEntry>>,
    seq: AtomicU64,
}

impl InMemoryThreadStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl AgentThreadStore for InMemoryThreadStore {
    fn create(
        &self,
        agent_id: &str,
        owner: ThreadOwner<'_>,
        _retention: ThreadRetention,
    ) -> BoxFuture<'_, AgenkitResult<AgentThreadId>> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let id = AgentThreadId::new(format!("th-{agent_id}-{seq}"));
        self.threads.lock().unwrap().insert(
            id.as_str().to_string(),
            ThreadEntry {
                owner: owner.key().map(str::to_string),
                messages: Vec::new(),
            },
        );
        Box::pin(async move { Ok(id) })
    }

    fn load(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<Option<Vec<ThreadMessage>>>> {
        // A thread owned by a different principal reads as not-found, so a
        // caller probing ids learns nothing about other principals' threads.
        let result = self
            .threads
            .lock()
            .unwrap()
            .get(id.as_str())
            .filter(|entry| entry.owner.as_deref() == owner.key())
            .map(|entry| entry.messages.clone());
        Box::pin(async move { Ok(result) })
    }

    fn append(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
        messages: Vec<ThreadMessage>,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        let mut threads = self.threads.lock().unwrap();
        let result = match threads.get_mut(id.as_str()) {
            Some(entry) if entry.owner.as_deref() == owner.key() => {
                entry.messages.extend(messages);
                Ok(())
            }
            // Missing, or owned by someone else: refuse without revealing which.
            _ => Err(AgenkitError::not_found("thread not found")),
        };
        Box::pin(async move { result })
    }

    fn delete(
        &self,
        id: &AgentThreadId,
        owner: ThreadOwner<'_>,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        // Idempotent: only the owner can delete; a foreign/missing id is a
        // silent no-op so deletion never doubles as an existence oracle.
        let mut threads = self.threads.lock().unwrap();
        if threads
            .get(id.as_str())
            .is_some_and(|entry| entry.owner.as_deref() == owner.key())
        {
            threads.remove(id.as_str());
        }
        Box::pin(async move { Ok(()) })
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

    /// Load the current message history.
    pub async fn history(&self) -> AgenkitResult<Vec<ThreadMessage>> {
        Ok(self
            .store
            .load(&self.id, self.owner())
            .await?
            .unwrap_or_default())
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
    use pocopine_agenkit_core::Role;
    use pocopine_auth::AuthUser;

    fn alice() -> Principal {
        Principal::from_user(AuthUser::new("alice"))
    }

    fn bob() -> Principal {
        Principal::from_user(AuthUser::new("bob"))
    }

    #[tokio::test]
    async fn in_memory_store_round_trips() {
        let store = InMemoryThreadStore::new();
        let owner = ThreadOwner::anonymous();
        let id = store
            .create("debugger", owner, ThreadRetention::Session)
            .await
            .unwrap();
        store
            .append(
                &id,
                owner,
                vec![ThreadMessage::new(Role::User, "why did it fail?")],
            )
            .await
            .unwrap();
        let history = store.load(&id, owner).await.unwrap().unwrap();
        assert_eq!(history.len(), 1);
        store.delete(&id, owner).await.unwrap();
        assert!(store.load(&id, owner).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn foreign_principal_cannot_read_or_mutate_anothers_thread() {
        let store = InMemoryThreadStore::new();
        let (alice_p, bob_p) = (alice(), bob());
        let alice = ThreadOwner::from_principal(&alice_p);
        let bob = ThreadOwner::from_principal(&bob_p);

        let id = store
            .create("debugger", alice, ThreadRetention::Session)
            .await
            .unwrap();
        store
            .append(&id, alice, vec![ThreadMessage::new(Role::User, "secret")])
            .await
            .unwrap();

        // Bob cannot read Alice's thread — it reads as not-found.
        assert!(store.load(&id, bob).await.unwrap().is_none());
        // Bob cannot append to it.
        assert!(
            store
                .append(&id, bob, vec![ThreadMessage::new(Role::User, "inject")])
                .await
                .is_err()
        );
        // Bob's delete is a silent no-op — Alice's thread survives intact.
        store.delete(&id, bob).await.unwrap();
        let history = store.load(&id, alice).await.unwrap().unwrap();
        assert_eq!(history.len(), 1);
    }

    #[tokio::test]
    async fn anonymous_and_authenticated_owners_are_distinct() {
        let store = InMemoryThreadStore::new();
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
}
