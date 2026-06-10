//! Embedding payloads (RFC-093 Phase 1.3, §D5).
//!
//! An embedder turns text/multimodal content into vectors. Descriptors expose
//! model/provider *aliases* and dimensions, never provider credentials (§D5).

use crate::ModelRef;

/// The framework-visible description of a registered embedder.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmbedderDescriptor {
    /// Stable embedder id.
    pub id: String,
    /// The model alias used to embed (e.g. `"local/embed"`).
    pub model: ModelRef,
    /// Output vector dimensionality.
    pub dimensions: u32,
}

impl EmbedderDescriptor {
    /// An embedder descriptor.
    pub fn new(id: impl Into<String>, model: ModelRef, dimensions: u32) -> Self {
        Self {
            id: id.into(),
            model,
            dimensions,
        }
    }
}

/// A single embedding vector.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Embedding {
    /// The vector components.
    pub vector: Vec<f32>,
}

impl Embedding {
    /// Wrap a vector.
    pub fn new(vector: Vec<f32>) -> Self {
        Self { vector }
    }

    /// The vector dimensionality.
    pub fn dimensions(&self) -> usize {
        self.vector.len()
    }
}

/// A batch of embeddings produced from a batch of inputs.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingBatch {
    /// One embedding per input, in input order.
    pub embeddings: Vec<Embedding>,
    /// The shared dimensionality of every embedding in the batch.
    pub dimensions: u32,
}

impl EmbeddingBatch {
    /// Build a batch, stamping the shared dimensionality.
    pub fn new(embeddings: Vec<Embedding>, dimensions: u32) -> Self {
        Self {
            embeddings,
            dimensions,
        }
    }

    /// Number of embeddings in the batch.
    pub fn len(&self) -> usize {
        self.embeddings.len()
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.embeddings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_round_trips_and_reports_dimensions() {
        let batch = EmbeddingBatch::new(vec![Embedding::new(vec![0.1, 0.2, 0.3])], 3);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch.embeddings[0].dimensions(), 3);
        let back: EmbeddingBatch =
            serde_json::from_str(&serde_json::to_string(&batch).unwrap()).unwrap();
        assert_eq!(batch, back);
    }

    #[test]
    fn descriptor_carries_alias_not_credentials() {
        let descriptor = EmbedderDescriptor::new("local", ModelRef::new("local/embed"), 384);
        let json = serde_json::to_string(&descriptor).unwrap();
        assert!(json.contains(r#""model":"local/embed""#));
    }
}
