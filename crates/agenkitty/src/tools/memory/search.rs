//! `memory.search`: lexical search over the caller's accessible memory.

use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{MemoryKind, MemoryRuntime, MemoryScope, MemorySearchHit};
use super::store::search_caller_namespaces;

pub const MEMORY_SEARCH_TOOL_ID: &str = "memory.search";

#[derive(Clone)]
pub struct MemorySearchTool {
    runtime: Arc<MemoryRuntime>,
}

impl MemorySearchTool {
    pub fn new(runtime: Arc<MemoryRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(&self, input: MemorySearchInput) -> AgenkitResult<MemorySearchOutput> {
        let context = self
            .runtime
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let hits = search_caller_namespaces(
            self.runtime.store().as_ref(),
            &context,
            &input.query,
            &input.scopes,
            &input.kinds,
            &input.tags,
            input.updated_after_ms,
            input.limit,
        )
        .await?;
        Ok(MemorySearchOutput { hits })
    }
}

impl AiTool for MemorySearchTool {
    const ID: &'static str = MEMORY_SEARCH_TOOL_ID;
    type Input = MemorySearchInput;
    type Output = MemorySearchOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            MEMORY_SEARCH_TOOL_ID,
            "Search the caller's memory by query, scope, kind, tags, and recency. Returns bounded \
             snippet hits ordered by relevance — never full bodies.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input).await })
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct MemorySearchInput {
    #[serde(default)]
    pub query: String,
    /// Restrict to these scopes. Empty means every accessible scope.
    #[serde(default)]
    pub scopes: Vec<MemoryScope>,
    #[serde(default)]
    pub kinds: Vec<MemoryKind>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub updated_after_ms: Option<u64>,
    /// Max hits (0 = default). Hard-capped by the store.
    #[serde(default)]
    pub limit: usize,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct MemorySearchOutput {
    pub hits: Vec<MemorySearchHit>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::common::{
        CurrentMemoryContext, MemoryEntry, MemoryRetention, MemorySource,
    };

    fn context() -> CurrentMemoryContext {
        CurrentMemoryContext {
            project_id: "proj".to_string(),
            agent_id: "agent".to_string(),
            thread_id: Some("thread".to_string()),
        }
    }

    async fn seed(runtime: &MemoryRuntime, scope: MemoryScope, namespace: &str, title: &str) {
        let entry = MemoryEntry::draft(
            scope,
            namespace,
            MemoryKind::Fact,
            title,
            "body about yrs collaboration",
            vec![],
            MemorySource::Agent,
            vec![],
            "reason",
            MemoryRetention::Session,
            None,
        )
        .unwrap();
        runtime.store().append(entry).await.unwrap();
    }

    fn search_input(runtime: &MemoryRuntime, body: serde_json::Value) -> MemorySearchInput {
        let args = runtime.inject_context_args(&body, context()).unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn search_only_returns_caller_namespaces() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        seed(&runtime, MemoryScope::Project, "proj", "yrs in project").await;
        // A foreign project's entry must not surface.
        seed(&runtime, MemoryScope::Project, "other", "yrs elsewhere").await;
        // The caller's own session namespace (thread) is accessible.
        seed(&runtime, MemoryScope::Session, "thread", "yrs in session").await;

        let out = MemorySearchTool::new(runtime.clone())
            .run(search_input(
                &runtime,
                serde_json::json!({ "query": "yrs" }),
            ))
            .await
            .unwrap();
        let namespaces: Vec<_> = out.hits.iter().map(|h| h.namespace.as_str()).collect();
        assert!(namespaces.contains(&"proj"));
        assert!(namespaces.contains(&"thread"));
        assert!(!namespaces.contains(&"other"));
    }

    #[tokio::test]
    async fn search_respects_scope_filter_and_limit() {
        let runtime = Arc::new(MemoryRuntime::in_memory());
        seed(&runtime, MemoryScope::Project, "proj", "yrs one").await;
        seed(&runtime, MemoryScope::Session, "thread", "yrs two").await;

        let out = MemorySearchTool::new(runtime.clone())
            .run(search_input(
                &runtime,
                serde_json::json!({ "query": "yrs", "scopes": ["session"] }),
            ))
            .await
            .unwrap();
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].scope, MemoryScope::Session);
    }
}
