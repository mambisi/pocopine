//! Execution context and app state for tools, retrievers, and embedders
//! (RFC-093 Phase 2.4, §D5).
//!
//! [`AiContext`] is the owned, cheaply-cloneable handle passed into tool /
//! retriever / embedder calls. It exposes framework-mediated app state so
//! those reads are visible to traces (§D6). The caller principal is threaded
//! in by the Phase 3.2 adapter.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use pocopine_auth::Principal;

/// A type-erased registry of app resources keyed by name. Tools/retrievers
/// reach app services (a doc index, a db pool handle, ...) through here so the
/// access is declared and traceable, rather than via hidden globals (§D6).
#[derive(Default, Clone)]
pub struct AppState {
    entries: HashMap<String, Arc<dyn Any + Send + Sync>>,
}

impl AppState {
    /// An empty state map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a value under `key`.
    pub fn insert<T: Any + Send + Sync>(&mut self, key: impl Into<String>, value: T) {
        self.entries.insert(key.into(), Arc::new(value));
    }

    /// Insert an already-shared value under `key`.
    pub fn insert_arc<T: Any + Send + Sync>(&mut self, key: impl Into<String>, value: Arc<T>) {
        self.entries.insert(key.into(), value);
    }

    /// Fetch and downcast the value stored under `key`.
    pub fn get<T: Any + Send + Sync>(&self, key: &str) -> Option<Arc<T>> {
        self.entries.get(key).cloned()?.downcast::<T>().ok()
    }
}

/// The owned context handed to tool / retriever / embedder calls. Carries the
/// caller [`Principal`] so reads/writes run under the request's identity (§D5).
#[derive(Clone)]
pub struct AiContext {
    state: Arc<AppState>,
    principal: Principal,
    /// The per-invocation artifact capture surface (RFC-122 §2) — present
    /// only for a tool call dispatched by a runtime with an `ArtifactSink`
    /// wired.
    artifacts: Option<super::artifact::Artifacts>,
}

impl AiContext {
    /// Build an anonymous context over the given app state.
    pub(crate) fn new(state: Arc<AppState>) -> Self {
        Self::with_principal(state, Principal::anonymous())
    }

    /// Build a context bound to the caller principal.
    pub(crate) fn with_principal(state: Arc<AppState>, principal: Principal) -> Self {
        Self {
            state,
            principal,
            artifacts: None,
        }
    }

    /// Attach the per-invocation capture surface (dispatcher-only).
    pub(crate) fn with_artifacts(mut self, artifacts: super::artifact::Artifacts) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    /// Fetch a framework-mediated app resource by key, downcast to `T`.
    pub fn state<T: Any + Send + Sync>(&self, key: &str) -> AgenkitResult<Arc<T>> {
        self.state.get::<T>(key).ok_or_else(|| {
            AgenkitError::not_found(format!("app state `{key}` not found (or wrong type)"))
        })
    }

    /// The caller principal a tool/retriever runs under. Anonymous unless the
    /// flow was invoked with a principal (§D5/§D10).
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// The artifact capture surface (RFC-122 §2), or a loud config error when
    /// the host wired no [`ArtifactSink`](super::artifact::ArtifactSink). A
    /// tool that can degrade should treat the error as "artifacts
    /// unavailable" and fall back to text.
    pub fn artifacts(&self) -> AgenkitResult<super::artifact::Artifacts> {
        self.artifacts.clone().ok_or_else(|| {
            AgenkitError::config(
                "no artifact sink configured: wire one with \
                 Agenkit::builder().artifact_sink(...)",
            )
        })
    }
}

/// Context for a tool call.
pub type AiToolContext = AiContext;
/// Context for a retrieval call.
pub type RetrievalContext = AiContext;
/// Context for an embedding call.
pub type EmbedContext = AiContext;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct DocIndex {
        docs: Vec<&'static str>,
    }

    #[test]
    fn state_round_trips_typed_resource() {
        let mut state = AppState::new();
        state.insert(
            "doc_index",
            DocIndex {
                docs: vec!["a", "b"],
            },
        );
        let ctx = AiContext::new(Arc::new(state));
        let index = ctx.state::<DocIndex>("doc_index").unwrap();
        assert_eq!(index.docs.len(), 2);
        assert_eq!(
            ctx.state::<DocIndex>("missing").unwrap_err().kind(),
            "not_found"
        );
    }
}
