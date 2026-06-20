//! Flow descriptors (RFC-093 Phase 1.4, §D4, §D6).
//!
//! A [`FlowDescriptor`] is the app-callable unit's contract: typed input/output
//! schemas, the declared context manifest, the public stream mode, and whether
//! the flow is exposed as a server function.

use crate::{ContextManifest, ResourceKind, SchemaRef, StreamMode};

/// The schema of a flow's typed input.
pub type FlowInputSchema = SchemaRef;
/// The schema of a flow's typed output.
pub type FlowOutputSchema = SchemaRef;

/// The framework-visible contract of a registered flow.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FlowDescriptor {
    /// Stable flow id.
    pub id: String,
    /// Whether the flow is exposed as a public server function (§D9).
    #[serde(default)]
    pub public: bool,
    /// Typed input schema.
    pub input: FlowInputSchema,
    /// Typed output schema.
    pub output: FlowOutputSchema,
    /// The public streaming mode for this flow (§D8).
    #[serde(default)]
    pub stream_mode: StreamMode,
    /// Whether this flow may stream the model's reasoning ("thinking") *text* to
    /// the client (§D10). Off by default: reasoning stays server-side and only a
    /// redacted activity count crosses. An author opts in per flow when the UX
    /// wants to show the reasoning (e.g. a coding-agent "thinking" panel).
    #[serde(default)]
    pub expose_reasoning: bool,
    /// The declared context manifest (§D6).
    #[serde(default)]
    pub manifest: ContextManifest,
}

impl FlowDescriptor {
    /// A private flow descriptor with empty schemas and manifest.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            public: false,
            input: SchemaRef::default(),
            output: SchemaRef::default(),
            stream_mode: StreamMode::FinalOnly,
            expose_reasoning: false,
            manifest: ContextManifest::new(),
        }
    }

    /// Set the input schema.
    pub fn input(mut self, input: FlowInputSchema) -> Self {
        self.input = input;
        self
    }

    /// Set the output schema.
    pub fn output(mut self, output: FlowOutputSchema) -> Self {
        self.output = output;
        self
    }

    /// Mark the flow public (exposed as a server function).
    pub fn public(mut self) -> Self {
        self.public = true;
        self
    }

    /// Set the public stream mode.
    pub fn stream_mode(mut self, mode: StreamMode) -> Self {
        self.stream_mode = mode;
        self
    }

    /// Opt this flow into streaming the model's reasoning ("thinking") text to
    /// the client (§D10). Off by default — see [`FlowDescriptor::expose_reasoning`].
    pub fn expose_reasoning(mut self) -> Self {
        self.expose_reasoning = true;
        self
    }

    /// Declare a tool dependency in the manifest.
    pub fn uses_tool(mut self, id: impl Into<String>) -> Self {
        self.manifest.declare(ResourceKind::Tool, id);
        self
    }

    /// Declare a retriever dependency in the manifest.
    pub fn uses_retriever(mut self, id: impl Into<String>) -> Self {
        self.manifest.declare(ResourceKind::Retriever, id);
        self
    }

    /// Declare an embedder dependency in the manifest.
    pub fn uses_embedder(mut self, id: impl Into<String>) -> Self {
        self.manifest.declare(ResourceKind::Embedder, id);
        self
    }

    /// Declare an agent dependency in the manifest.
    pub fn uses_agent(mut self, id: impl Into<String>) -> Self {
        self.manifest.declare(ResourceKind::Agent, id);
        self
    }

    /// Declare an app-state dependency in the manifest.
    pub fn uses_state(mut self, key: impl Into<String>) -> Self {
        self.manifest.declare(ResourceKind::State, key);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_declares_manifest_resources() {
        let flow = FlowDescriptor::new("answer_question")
            .public()
            .input(SchemaRef::named("QuestionInput"))
            .output(SchemaRef::named("Answer"))
            .stream_mode(StreamMode::Progress)
            .uses_retriever("project_docs")
            .uses_agent("docs_first")
            .uses_tool("search_docs");

        assert!(flow.public);
        assert_eq!(flow.stream_mode, StreamMode::Progress);
        assert!(
            flow.manifest
                .contains(ResourceKind::Retriever, "project_docs")
        );
        assert!(flow.manifest.contains(ResourceKind::Agent, "docs_first"));

        let back: FlowDescriptor =
            serde_json::from_str(&serde_json::to_string(&flow).unwrap()).unwrap();
        assert_eq!(flow, back);
    }
}
