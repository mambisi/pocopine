//! Retrievers: typed [`AiRetriever`] trait, object-safe [`DynRetriever`]
//! erasure, the [`RetrieverRegistry`], and the retriever-as-tool adapter
//! (RFC-093 Phase 2.4, §D5).
//!
//! Flows call retrievers directly for deterministic context loading; the
//! adapter wraps a retriever as a [`DynTool`] so an agent can decide when to
//! search. Retrieval is read-only by default.

use std::collections::HashMap;
use std::sync::Arc;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult, RetrievalSet, RetrieverDescriptor};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use super::context::RetrievalContext;
use super::provider::BoxFuture;
use super::schema::schema_ref_for;
use super::tool::DynTool;

/// A typed, author-facing retriever. Returns the concrete [`RetrievalSet`] so
/// hits carry citations/source refs for traces and reducers (§D5, §D12).
pub trait AiRetriever: Send + Sync + 'static {
    /// Stable retriever id.
    const ID: &'static str;

    /// Typed query, deserialized when the retriever is driven as a tool. The
    /// `JsonSchema` derive becomes the `parameters` schema when this retriever
    /// is exposed as a tool, so the model sees the query shape (§D5).
    type Query: DeserializeOwned + JsonSchema + Send + 'static;

    /// The framework-visible descriptor. Authors supply id/description/source
    /// kinds; the runtime fills the query schema from [`Self::Query`] unless the
    /// descriptor already carries one (see the [`DynRetriever`] erasure).
    fn descriptor() -> RetrieverDescriptor;

    /// Run the retrieval.
    fn retrieve(
        &self,
        query: Self::Query,
        ctx: RetrievalContext,
    ) -> BoxFuture<'_, AgenkitResult<RetrievalSet>>;

    /// Wrap this retriever as a tool, so an agent can call it (§D5). Requires a
    /// default-constructible retriever (the common stateless case).
    fn as_tool() -> Arc<dyn DynTool>
    where
        Self: Default,
    {
        let retriever: Arc<dyn DynRetriever> = Arc::new(Self::default());
        retriever_as_tool(retriever)
    }
}

/// Object-safe erased retriever. Stored as `Arc<dyn DynRetriever>`.
pub trait DynRetriever: Send + Sync + 'static {
    /// The retriever id.
    fn id(&self) -> &'static str;
    /// The descriptor.
    fn descriptor(&self) -> RetrieverDescriptor;
    /// Validate + dispatch a JSON-encoded query.
    fn retrieve_json<'a>(
        &'a self,
        query: serde_json::Value,
        ctx: RetrievalContext,
    ) -> BoxFuture<'a, AgenkitResult<RetrievalSet>>;
}

impl<R: AiRetriever> DynRetriever for R {
    fn id(&self) -> &'static str {
        R::ID
    }

    fn descriptor(&self) -> RetrieverDescriptor {
        // Author supplies id/description/source kinds; the runtime fills the
        // typed query schema (the model's `parameters` when used as a tool)
        // from `schemars`, unless the author already attached one.
        let mut descriptor = R::descriptor();
        if descriptor.query.json_schema.is_none() {
            descriptor.query = schema_ref_for::<R::Query>();
        }
        descriptor
    }

    fn retrieve_json<'a>(
        &'a self,
        query: serde_json::Value,
        ctx: RetrievalContext,
    ) -> BoxFuture<'a, AgenkitResult<RetrievalSet>> {
        Box::pin(async move {
            let query: R::Query = serde_json::from_value(query).map_err(|err| {
                AgenkitError::validation(format!("retriever `{}` query: {err}", R::ID))
            })?;
            R::retrieve(self, query, ctx).await
        })
    }
}

/// A registry of retrievers keyed by id.
#[derive(Default, Clone)]
pub struct RetrieverRegistry {
    retrievers: HashMap<&'static str, Arc<dyn DynRetriever>>,
}

impl RetrieverRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed retriever.
    pub fn register<R: AiRetriever>(&mut self, retriever: R) {
        self.retrievers.insert(R::ID, Arc::new(retriever));
    }

    /// Look up a retriever by id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn DynRetriever>> {
        self.retrievers.get(id).cloned()
    }

    /// Whether a retriever id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.retrievers.contains_key(id)
    }
}

/// Wrap an erased retriever as a [`DynTool`] for agent-controlled search.
pub fn retriever_as_tool(retriever: Arc<dyn DynRetriever>) -> Arc<dyn DynTool> {
    Arc::new(RetrieverTool { retriever })
}

struct RetrieverTool {
    retriever: Arc<dyn DynRetriever>,
}

impl DynTool for RetrieverTool {
    fn id(&self) -> &'static str {
        self.retriever.id()
    }

    fn descriptor(&self) -> pocopine_agenkit_core::ToolDescriptor {
        let descriptor = self.retriever.descriptor();
        // Read-only by construction: retrieval never mutates state. The
        // retriever's query schema becomes the tool's input `parameters`, so an
        // agent sees the query shape — same wiring as a native tool (§D5).
        pocopine_agenkit_core::ToolDescriptor::new(descriptor.id, descriptor.description)
            .with_input(descriptor.query)
    }

    fn call_json<'a>(
        &'a self,
        args: serde_json::Value,
        ctx: RetrievalContext,
    ) -> BoxFuture<'a, AgenkitResult<serde_json::Value>> {
        let retriever = self.retriever.clone();
        Box::pin(async move {
            let set = retriever.retrieve_json(args, ctx).await?;
            serde_json::to_value(set).map_err(AgenkitError::from)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::context::{AiContext, AppState};
    use super::*;
    use pocopine_agenkit_core::{Content, RetrievalHit, SourceRef};
    use serde::Deserialize;

    #[derive(Default)]
    struct ProjectDocs;

    #[derive(Deserialize, schemars::JsonSchema)]
    struct ProjectDocsQuery {
        question: String,
    }

    impl AiRetriever for ProjectDocs {
        const ID: &'static str = "project_docs";
        type Query = ProjectDocsQuery;

        fn descriptor() -> RetrieverDescriptor {
            RetrieverDescriptor::new("project_docs", "Search project docs")
                .with_source_kinds(["doc"])
        }

        fn retrieve(
            &self,
            query: ProjectDocsQuery,
            _ctx: RetrievalContext,
        ) -> BoxFuture<'_, AgenkitResult<RetrievalSet>> {
            Box::pin(async move {
                Ok(RetrievalSet::new(vec![RetrievalHit {
                    source: SourceRef::new("doc", "uploads.md"),
                    score: Some(0.9),
                    content: Content::text(format!("answer to: {}", query.question)),
                    citation: None,
                }]))
            })
        }
    }

    fn ctx() -> AiContext {
        AiContext::new(Arc::new(AppState::new()))
    }

    #[tokio::test]
    async fn registry_retrieves_typed_query() {
        let mut registry = RetrieverRegistry::new();
        registry.register(ProjectDocs);
        let retriever = registry.get("project_docs").unwrap();
        let set = retriever
            .retrieve_json(serde_json::json!({"question": "uploads?"}), ctx())
            .await
            .unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set.hits[0].source.id, "uploads.md");
    }

    #[tokio::test]
    async fn retriever_as_tool_dispatches_and_serializes() {
        let tool = ProjectDocs::as_tool();
        assert_eq!(tool.id(), "project_docs");
        let value = tool
            .call_json(serde_json::json!({"question": "uploads?"}), ctx())
            .await
            .unwrap();
        // The serialized RetrievalSet is a JSON array of hits.
        assert!(value.is_array());
        assert_eq!(value[0]["source"]["id"], "uploads.md");
    }

    #[test]
    fn retriever_as_tool_exposes_the_query_schema_as_parameters() {
        // Same wiring as a native tool: the typed query becomes the tool's
        // input `parameters`, so an agent sees the query shape.
        let descriptor = ProjectDocs::as_tool().descriptor();
        let input = descriptor.input.json_schema.expect("query schema derived");
        assert_eq!(input["properties"]["question"]["type"], "string");
    }

    #[test]
    fn retriever_descriptor_carries_the_derived_query_schema() {
        let descriptor = DynRetriever::descriptor(&ProjectDocs);
        let query = descriptor.query.json_schema.expect("query schema derived");
        assert_eq!(query["properties"]["question"]["type"], "string");
    }

    #[test]
    fn builder_registers_a_retriever_as_a_tool() {
        // `tool_dyn(R::as_tool())` puts the retriever in the tool registry under
        // its id, so an agent can call it (the documented §D5 "as a tool" path).
        use crate::server::{Agenkit, MockProvider};
        use pocopine_agenkit_core::ModelRef;
        let agenkit = Agenkit::builder()
            .provider(MockProvider::new("local"))
            .default_model(ModelRef::new("local/default"))
            .tool_dyn(ProjectDocs::as_tool())
            .build()
            .unwrap();
        assert!(agenkit.tools().get("project_docs").is_some());
    }
}
