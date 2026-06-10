//! Agent descriptor (RFC-093 Phase 1.4, §D4, §D7).
//!
//! An [`AiAgentDescriptor`] is the framework-visible description of a typed
//! runnable AI unit: its id, model alias, system prompt, tool allowlist,
//! schemas, and limits. The runnable `AiAgent` trait lives in the facade.

use crate::{ModelRef, SchemaRef};

/// The framework-visible description of a typed agent.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AiAgentDescriptor {
    /// Stable agent id.
    pub id: String,
    /// The model alias the agent runs on; `None` inherits the flow default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// The agent's system prompt/instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The ids of the tools the agent may call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_ids: Vec<String>,
    /// Typed input schema.
    pub input: SchemaRef,
    /// Typed output schema.
    pub output: SchemaRef,
    /// Maximum output tokens, if bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

impl AiAgentDescriptor {
    /// A new agent descriptor with empty schemas and no tools.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            model: None,
            system: None,
            tool_ids: Vec::new(),
            input: SchemaRef::default(),
            output: SchemaRef::default(),
            max_tokens: None,
        }
    }

    /// Set the model alias.
    pub fn model(mut self, model: ModelRef) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the system prompt.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Allow a tool.
    pub fn tool(mut self, id: impl Into<String>) -> Self {
        self.tool_ids.push(id.into());
        self
    }

    /// Set the input schema.
    pub fn input(mut self, input: SchemaRef) -> Self {
        self.input = input;
        self
    }

    /// Set the output schema.
    pub fn output(mut self, output: SchemaRef) -> Self {
        self.output = output;
        self
    }

    /// Cap output tokens.
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_round_trips_and_omits_defaults() {
        let agent = AiAgentDescriptor::new("fast_researcher")
            .model(ModelRef::new("local/fast"))
            .system("Find likely answers quickly, then cite evidence.")
            .tool("search_docs")
            .max_tokens(700);
        let json = serde_json::to_string(&agent).unwrap();
        assert!(json.contains(r#""model":"local/fast""#));
        assert!(json.contains(r#""max_tokens":700"#));
        let back: AiAgentDescriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(agent, back);
    }
}
