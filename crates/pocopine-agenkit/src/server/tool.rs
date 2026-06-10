//! Tools: typed [`AiTool`] trait, object-safe [`DynTool`] erasure, and the
//! [`ToolRegistry`] (RFC-093 Phase 2.4, §D5).
//!
//! Authors implement the typed [`AiTool`]; the runtime erases it to a
//! [`DynTool`] that dispatches on `serde_json::Value`, validating input before
//! execution and output before it crosses a model/client boundary (§D5).

use std::collections::HashMap;
use std::sync::Arc;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

use super::context::AiToolContext;
use super::provider::BoxFuture;
use super::schema::schema_ref_for;

/// A typed, author-facing tool.
pub trait AiTool: Send + Sync + 'static {
    /// Stable tool id (the model calls the tool by this id).
    const ID: &'static str;

    /// Typed input, deserialized from the model's tool-call arguments. The
    /// `JsonSchema` derive becomes the function-calling `parameters` schema, so
    /// the model sees the real argument shape (§D5).
    type Input: DeserializeOwned + JsonSchema + Send + 'static;
    /// Typed output, serialized back to the model/flow.
    type Output: Serialize + JsonSchema + Send + 'static;

    /// The framework-visible descriptor. Authors supply the id, description, and
    /// side-effect policy; the runtime fills the input/output JSON schemas from
    /// [`Self::Input`]/[`Self::Output`] unless the descriptor already carries
    /// them (see the [`DynTool`] erasure).
    fn descriptor() -> ToolDescriptor;

    /// Execute the tool.
    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>>;
}

/// Object-safe erased tool. Stored as `Arc<dyn DynTool>`.
pub trait DynTool: Send + Sync + 'static {
    /// The tool id.
    fn id(&self) -> &'static str;
    /// The descriptor.
    fn descriptor(&self) -> ToolDescriptor;
    /// Validate + dispatch a JSON-encoded call.
    fn call_json<'a>(
        &'a self,
        args: serde_json::Value,
        ctx: AiToolContext,
    ) -> BoxFuture<'a, AgenkitResult<serde_json::Value>>;
}

impl<T: AiTool> DynTool for T {
    fn id(&self) -> &'static str {
        T::ID
    }

    fn descriptor(&self) -> ToolDescriptor {
        // Author supplies id/description/policy; the runtime fills the typed
        // input/output schemas (the model's `parameters`) from `schemars`,
        // unless the author already attached them.
        let mut descriptor = T::descriptor();
        if descriptor.input.json_schema.is_none() {
            descriptor.input = schema_ref_for::<T::Input>();
        }
        if descriptor.output.json_schema.is_none() {
            descriptor.output = schema_ref_for::<T::Output>();
        }
        descriptor
    }

    fn call_json<'a>(
        &'a self,
        args: serde_json::Value,
        ctx: AiToolContext,
    ) -> BoxFuture<'a, AgenkitResult<serde_json::Value>> {
        Box::pin(async move {
            let input: T::Input = serde_json::from_value(args).map_err(|err| {
                AgenkitError::validation(format!("tool `{}` input: {err}", T::ID))
            })?;
            let output = T::call(self, input, ctx).await?;
            serde_json::to_value(output)
                .map_err(|err| AgenkitError::validation(format!("tool `{}` output: {err}", T::ID)))
        })
    }
}

/// A registry of tools keyed by id.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn DynTool>>,
}

impl ToolRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed tool.
    pub fn register<T: AiTool>(&mut self, tool: T) {
        self.tools.insert(T::ID, Arc::new(tool));
    }

    /// Register an already-erased tool (e.g. a retriever-as-tool adapter).
    pub fn register_dyn(&mut self, tool: Arc<dyn DynTool>) {
        self.tools.insert(tool.id(), tool);
    }

    /// Look up a tool by id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn DynTool>> {
        self.tools.get(id).cloned()
    }

    /// Whether a tool id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.tools.contains_key(id)
    }

    /// The descriptors of all registered tools.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools.values().map(|tool| tool.descriptor()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::context::{AiContext, AppState};
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, JsonSchema)]
    struct SearchInput {
        query: String,
    }

    #[derive(Serialize, JsonSchema)]
    struct SearchHit {
        title: String,
    }

    struct SearchDocs;

    impl AiTool for SearchDocs {
        const ID: &'static str = "search_docs";
        type Input = SearchInput;
        type Output = Vec<SearchHit>;

        fn descriptor() -> ToolDescriptor {
            ToolDescriptor::new("search_docs", "Search project docs")
        }

        fn call(
            &self,
            input: SearchInput,
            _ctx: AiToolContext,
        ) -> BoxFuture<'_, AgenkitResult<Vec<SearchHit>>> {
            Box::pin(async move {
                Ok(vec![SearchHit {
                    title: format!("hit for {}", input.query),
                }])
            })
        }
    }

    fn ctx() -> AiContext {
        AiContext::new(std::sync::Arc::new(AppState::new()))
    }

    #[tokio::test]
    async fn registry_dispatches_and_validates() {
        let mut registry = ToolRegistry::new();
        registry.register(SearchDocs);
        let tool = registry.get("search_docs").unwrap();

        let output = tool
            .call_json(serde_json::json!({"query": "uploads"}), ctx())
            .await
            .unwrap();
        assert_eq!(output, serde_json::json!([{"title": "hit for uploads"}]));
    }

    #[test]
    fn descriptor_carries_a_derived_parameter_schema() {
        // The author's `descriptor()` names only id/description; the runtime
        // fills the real input schema so a provider sees the argument shape.
        let mut registry = ToolRegistry::new();
        registry.register(SearchDocs);
        let descriptor = registry.get("search_docs").unwrap().descriptor();

        let input = descriptor.input.json_schema.expect("input schema derived");
        assert_eq!(input["properties"]["query"]["type"], "string");
        assert!(descriptor.output.json_schema.is_some());
    }

    #[tokio::test]
    async fn invalid_input_is_validation_error() {
        let mut registry = ToolRegistry::new();
        registry.register(SearchDocs);
        let tool = registry.get("search_docs").unwrap();
        let err = tool
            .call_json(serde_json::json!({"wrong": 1}), ctx())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }
}
