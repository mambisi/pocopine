//! Embedders: typed [`AiEmbedder`] trait, object-safe [`DynEmbedder`] erasure,
//! and the [`EmbedderRegistry`] (RFC-093 Phase 2.4, §D5).
//!
//! Embedders expose model/provider *aliases* and dimensions, never provider
//! credentials (§D5). The runtime erases the typed trait to dispatch on
//! `serde_json::Value`.

use std::collections::HashMap;
use std::sync::Arc;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult, EmbedderDescriptor, EmbeddingBatch};
use serde::de::DeserializeOwned;

use super::context::EmbedContext;
use super::provider::BoxFuture;

/// A typed, author-facing embedder.
pub trait AiEmbedder: Send + Sync + 'static {
    /// Stable embedder id.
    const ID: &'static str;

    /// Typed input, deserialized when driven generically.
    type Input: DeserializeOwned + Send + 'static;

    /// The framework-visible descriptor.
    fn descriptor() -> EmbedderDescriptor;

    /// Produce embeddings for `input`.
    fn embed(
        &self,
        input: Self::Input,
        ctx: EmbedContext,
    ) -> BoxFuture<'_, AgenkitResult<EmbeddingBatch>>;
}

/// Object-safe erased embedder. Stored as `Arc<dyn DynEmbedder>`.
pub trait DynEmbedder: Send + Sync + 'static {
    /// The embedder id.
    fn id(&self) -> &'static str;
    /// The descriptor.
    fn descriptor(&self) -> EmbedderDescriptor;
    /// Validate + dispatch a JSON-encoded input.
    fn embed_json<'a>(
        &'a self,
        input: serde_json::Value,
        ctx: EmbedContext,
    ) -> BoxFuture<'a, AgenkitResult<EmbeddingBatch>>;
}

impl<E: AiEmbedder> DynEmbedder for E {
    fn id(&self) -> &'static str {
        E::ID
    }

    fn descriptor(&self) -> EmbedderDescriptor {
        E::descriptor()
    }

    fn embed_json<'a>(
        &'a self,
        input: serde_json::Value,
        ctx: EmbedContext,
    ) -> BoxFuture<'a, AgenkitResult<EmbeddingBatch>> {
        Box::pin(async move {
            let input: E::Input = serde_json::from_value(input).map_err(|err| {
                AgenkitError::validation(format!("embedder `{}` input: {err}", E::ID))
            })?;
            E::embed(self, input, ctx).await
        })
    }
}

/// A registry of embedders keyed by id.
#[derive(Default, Clone)]
pub struct EmbedderRegistry {
    embedders: HashMap<&'static str, Arc<dyn DynEmbedder>>,
}

impl EmbedderRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed embedder.
    pub fn register<E: AiEmbedder>(&mut self, embedder: E) {
        self.embedders.insert(E::ID, Arc::new(embedder));
    }

    /// Look up an embedder by id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn DynEmbedder>> {
        self.embedders.get(id).cloned()
    }

    /// Whether an embedder id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.embedders.contains_key(id)
    }
}

#[cfg(test)]
mod tests {
    use super::super::context::{AiContext, AppState};
    use super::*;
    use pocopine_agenkit_core::{Embedding, ModelRef};
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct EmbedInput {
        text: String,
    }

    struct LocalEmbedder;

    impl AiEmbedder for LocalEmbedder {
        const ID: &'static str = "local";
        type Input = EmbedInput;

        fn descriptor() -> EmbedderDescriptor {
            EmbedderDescriptor::new("local", ModelRef::new("local/embed"), 3)
        }

        fn embed(
            &self,
            input: EmbedInput,
            _ctx: EmbedContext,
        ) -> BoxFuture<'_, AgenkitResult<EmbeddingBatch>> {
            Box::pin(async move {
                let value = input.text.len() as f32;
                Ok(EmbeddingBatch::new(
                    vec![Embedding::new(vec![value, value, value])],
                    3,
                ))
            })
        }
    }

    #[tokio::test]
    async fn registry_embeds_typed_input() {
        let mut registry = EmbedderRegistry::new();
        registry.register(LocalEmbedder);
        let embedder = registry.get("local").unwrap();
        let batch = embedder
            .embed_json(
                serde_json::json!({"text": "abc"}),
                AiContext::new(Arc::new(AppState::new())),
            )
            .await
            .unwrap();
        assert_eq!(batch.dimensions, 3);
        assert_eq!(batch.embeddings[0].vector, vec![3.0, 3.0, 3.0]);
    }
}
