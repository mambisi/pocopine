//! `MemoryRetriever`: an [`AiRetriever`] over the same `MemoryStore`, for
//! deterministic flow-side context loading.
//!
//! The native `memory.search` tool is the model-facing path. This retriever is
//! the flow-facing one: a flow constructs it bound to a fixed
//! [`CurrentMemoryContext`] (its own project/agent/thread) and retrieves citable
//! hits. It is **not** auto-registered as a default tool — exposing both it and
//! `memory.search` to the model would duplicate the same capability. A host that
//! wants it agent-callable can register `retriever.into_tool()` explicitly.

use std::sync::Arc;

use pocopine_agenkit::server::{
    AiRetriever, BoxFuture, DynTool, RetrievalContext, retriever_as_tool,
};
use pocopine_agenkit_core::{
    AgenkitResult, Content, RetrievalHit, RetrievalSet, RetrieverDescriptor, SourceRef,
};
use schemars::JsonSchema;
use serde::Deserialize;

use super::common::{CurrentMemoryContext, MemoryKind, MemoryRuntime, MemoryScope, MemoryStore};
use super::store::search_caller_namespaces;

pub const MEMORY_RETRIEVER_ID: &str = "memory.retrieve";

/// A retriever bound to one caller context. Reads only that caller's namespaces.
#[derive(Clone)]
pub struct MemoryRetriever {
    store: Arc<dyn MemoryStore>,
    context: CurrentMemoryContext,
}

impl MemoryRetriever {
    pub fn new(store: Arc<dyn MemoryStore>, context: CurrentMemoryContext) -> Self {
        Self { store, context }
    }

    pub fn from_runtime(runtime: &MemoryRuntime, context: CurrentMemoryContext) -> Self {
        Self::new(runtime.store().clone(), context)
    }

    /// Wrap as a [`DynTool`] for agent-controlled retrieval. The trait's
    /// `as_tool()` requires `Default`, which a context-bound retriever cannot be,
    /// so we erase an `Arc` of `self` directly.
    pub fn into_tool(self) -> Arc<dyn DynTool> {
        retriever_as_tool(Arc::new(self))
    }

    /// Run the retrieval without an Agenkit `RetrievalContext`. This is the real
    /// work; the `AiRetriever::retrieve` impl delegates here. Useful for flows
    /// holding a retriever directly.
    pub async fn search(&self, query: MemoryRetrieveQuery) -> AgenkitResult<RetrievalSet> {
        let hits = search_caller_namespaces(
            self.store.as_ref(),
            &self.context,
            &query.query,
            &query.scopes,
            &query.kinds,
            &query.tags,
            query.updated_after_ms,
            query.limit,
        )
        .await?;
        let hits = hits
            .into_iter()
            .map(|hit| {
                let body = if hit.snippet.is_empty() {
                    hit.title.clone()
                } else {
                    format!("{}\n{}", hit.title, hit.snippet)
                };
                RetrievalHit {
                    source: SourceRef::new("memory", hit.id.clone()).with_uri(format!(
                        "memory://{}/{}",
                        hit.scope.as_str(),
                        hit.id
                    )),
                    score: Some(f64::from(hit.score)),
                    content: Content::text(body),
                    citation: None,
                }
            })
            .collect();
        Ok(RetrievalSet::new(hits))
    }
}

impl AiRetriever for MemoryRetriever {
    const ID: &'static str = MEMORY_RETRIEVER_ID;
    type Query = MemoryRetrieveQuery;

    fn descriptor() -> RetrieverDescriptor {
        RetrieverDescriptor::new(
            MEMORY_RETRIEVER_ID,
            "Retrieve the caller's memory as citable hits — bounded snippets, never full bodies.",
        )
        .with_source_kinds(["memory"])
    }

    fn retrieve(
        &self,
        query: Self::Query,
        _ctx: RetrievalContext,
    ) -> BoxFuture<'_, AgenkitResult<RetrievalSet>> {
        Box::pin(async move { self.search(query).await })
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct MemoryRetrieveQuery {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub scopes: Vec<MemoryScope>,
    #[serde(default)]
    pub kinds: Vec<MemoryKind>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub updated_after_ms: Option<u64>,
    #[serde(default)]
    pub limit: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::common::{MemoryEntry, MemoryRetention, MemorySource};
    use pocopine_agenkit_core::ToolSideEffectPolicy;

    fn context() -> CurrentMemoryContext {
        CurrentMemoryContext {
            project_id: "proj".to_string(),
            agent_id: "agent".to_string(),
            thread_id: None,
        }
    }

    async fn seed(
        runtime: &MemoryRuntime,
        scope: MemoryScope,
        namespace: &str,
        title: &str,
    ) -> String {
        let entry = MemoryEntry::draft(
            scope,
            namespace,
            MemoryKind::Fact,
            title,
            "body mentioning yrs",
            vec![],
            MemorySource::Agent,
            vec![],
            "reason",
            MemoryRetention::Session,
            None,
        )
        .unwrap();
        runtime.store().append(entry).await.unwrap().id
    }

    fn query(text: &str) -> MemoryRetrieveQuery {
        MemoryRetrieveQuery {
            query: text.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn retriever_returns_caller_hits_with_memory_source_refs() {
        let runtime = MemoryRuntime::in_memory();
        let id = seed(&runtime, MemoryScope::Project, "proj", "yrs decision").await;
        // A foreign project's entry must not surface.
        seed(&runtime, MemoryScope::Project, "other", "yrs elsewhere").await;

        let retriever = MemoryRetriever::from_runtime(&runtime, context());
        let set = retriever.search(query("yrs")).await.unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set.hits[0].source.kind, "memory");
        assert_eq!(set.hits[0].source.id, id);
        assert_eq!(
            set.hits[0].source.uri.as_deref(),
            Some(format!("memory://project/{id}").as_str())
        );
        assert!(set.hits[0].score.is_some());
    }

    #[tokio::test]
    async fn retriever_only_reads_caller_namespaces() {
        let runtime = MemoryRuntime::in_memory();
        // Bound to project "proj"; an entry under "other" is invisible.
        seed(&runtime, MemoryScope::Project, "other", "yrs elsewhere").await;
        let retriever = MemoryRetriever::from_runtime(&runtime, context());
        let set = retriever.search(query("yrs")).await.unwrap();
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn retriever_as_tool_is_read_only_with_query_schema() {
        let runtime = MemoryRuntime::in_memory();
        let tool = MemoryRetriever::from_runtime(&runtime, context()).into_tool();
        assert_eq!(tool.id(), MEMORY_RETRIEVER_ID);
        // Retrieval never mutates: the wrapped tool is read-only, and the typed
        // query becomes the tool's input parameters.
        assert_eq!(
            tool.descriptor().side_effect,
            ToolSideEffectPolicy::ReadOnly
        );
        let input = tool
            .descriptor()
            .input
            .json_schema
            .expect("query schema derived");
        assert_eq!(input["properties"]["query"]["type"], "string");
    }
}
