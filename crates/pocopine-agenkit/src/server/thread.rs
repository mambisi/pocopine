//! Agent thread state (RFC-093 Phase 2.6, §D4, §D10).
//!
//! Thread state is opt-in. The [`AgentThreadStore`] trait is behind a runtime
//! seam so v1 ships an in-memory store while a future durable store can drop
//! in. Thread ids exposed to callers are opaque Pocopine ids; provider-backed
//! ids (if any) never leak (§D10).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pocopine_agenkit_core::{
    AgenkitResult, AgentThreadId, StepKind, StepStatus, ThreadMessage, ThreadRetention, events,
};

use super::provider::BoxFuture;
use super::run::RunState;

/// Durable-or-not storage for agent threads.
pub trait AgentThreadStore: Send + Sync + 'static {
    /// Create a new thread for `agent_id`, returning its opaque id.
    fn create(
        &self,
        agent_id: &str,
        retention: ThreadRetention,
    ) -> BoxFuture<'_, AgenkitResult<AgentThreadId>>;

    /// Load a thread's message history, or `None` if it does not exist.
    fn load(&self, id: &AgentThreadId) -> BoxFuture<'_, AgenkitResult<Option<Vec<ThreadMessage>>>>;

    /// Append messages to a thread.
    fn append(
        &self,
        id: &AgentThreadId,
        messages: Vec<ThreadMessage>,
    ) -> BoxFuture<'_, AgenkitResult<()>>;

    /// Delete a thread and its state.
    fn delete(&self, id: &AgentThreadId) -> BoxFuture<'_, AgenkitResult<()>>;
}

/// The default in-memory thread store (tests, examples, single-process apps).
#[derive(Default)]
pub struct InMemoryThreadStore {
    threads: Mutex<HashMap<String, Vec<ThreadMessage>>>,
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
        _retention: ThreadRetention,
    ) -> BoxFuture<'_, AgenkitResult<AgentThreadId>> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let id = AgentThreadId::new(format!("th-{agent_id}-{seq}"));
        self.threads
            .lock()
            .unwrap()
            .insert(id.as_str().to_string(), Vec::new());
        Box::pin(async move { Ok(id) })
    }

    fn load(&self, id: &AgentThreadId) -> BoxFuture<'_, AgenkitResult<Option<Vec<ThreadMessage>>>> {
        let result = self.threads.lock().unwrap().get(id.as_str()).cloned();
        Box::pin(async move { Ok(result) })
    }

    fn append(
        &self,
        id: &AgentThreadId,
        messages: Vec<ThreadMessage>,
    ) -> BoxFuture<'_, AgenkitResult<()>> {
        self.threads
            .lock()
            .unwrap()
            .entry(id.as_str().to_string())
            .or_default()
            .extend(messages);
        Box::pin(async move { Ok(()) })
    }

    fn delete(&self, id: &AgentThreadId) -> BoxFuture<'_, AgenkitResult<()>> {
        self.threads.lock().unwrap().remove(id.as_str());
        Box::pin(async move { Ok(()) })
    }
}

/// A handle to a thread within a flow run: its id plus the store to reach it.
#[derive(Clone)]
pub struct AgentThreadHandle {
    pub(crate) id: AgentThreadId,
    pub(crate) store: Arc<dyn AgentThreadStore>,
}

impl AgentThreadHandle {
    /// The opaque thread id.
    pub fn id(&self) -> &AgentThreadId {
        &self.id
    }

    /// Load the current message history.
    pub async fn history(&self) -> AgenkitResult<Vec<ThreadMessage>> {
        Ok(self.store.load(&self.id).await?.unwrap_or_default())
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

    /// Create a fresh thread for the agent.
    pub async fn create(&self) -> AgenkitResult<AgentThreadHandle> {
        let store = self.run.inner.thread_store.clone();
        let id = store
            .create(self.agent_id, ThreadRetention::Session)
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
        Ok(AgentThreadHandle { id, store })
    }

    /// Resume the thread with `id` if it exists, otherwise create a new one.
    pub async fn resume_or_create(
        &self,
        id: Option<AgentThreadId>,
    ) -> AgenkitResult<AgentThreadHandle> {
        let store = self.run.inner.thread_store.clone();
        if let Some(id) = id
            && store.load(&id).await?.is_some()
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
            return Ok(AgentThreadHandle { id, store });
        }
        self.create().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_agenkit_core::Role;

    #[tokio::test]
    async fn in_memory_store_round_trips() {
        let store = InMemoryThreadStore::new();
        let id = store
            .create("debugger", ThreadRetention::Session)
            .await
            .unwrap();
        store
            .append(
                &id,
                vec![ThreadMessage::new(Role::User, "why did it fail?")],
            )
            .await
            .unwrap();
        let history = store.load(&id).await.unwrap().unwrap();
        assert_eq!(history.len(), 1);
        store.delete(&id).await.unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
    }
}
