//! Tool descriptors and call/result payloads (RFC-093 Phase 1.3, §D5).
//!
//! A tool is an action a flow or agent may invoke with typed input and typed
//! output. The descriptor declares its side-effect policy and schemas; it
//! carries no implementation and no credentials.

use crate::{SchemaRef, ToolSideEffectPolicy};

/// The framework-visible description of a registered tool.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolDescriptor {
    /// Stable tool id (the model calls the tool by this id).
    pub id: String,
    /// Human/model-readable description of what the tool does.
    pub description: String,
    /// Whether the tool may mutate state (§D5). Read-only is the default.
    #[serde(default)]
    pub side_effect: ToolSideEffectPolicy,
    /// Schema of the tool's typed input.
    pub input: SchemaRef,
    /// Schema of the tool's typed output.
    pub output: SchemaRef,
}

impl ToolDescriptor {
    /// A read-only tool descriptor.
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            side_effect: ToolSideEffectPolicy::ReadOnly,
            input: SchemaRef::default(),
            output: SchemaRef::default(),
        }
    }

    /// Mark the tool as side-effecting (§D5).
    pub fn side_effecting(mut self) -> Self {
        self.side_effect = ToolSideEffectPolicy::SideEffecting;
        self
    }

    /// Set the input schema.
    pub fn with_input(mut self, input: SchemaRef) -> Self {
        self.input = input;
        self
    }

    /// Set the output schema.
    pub fn with_output(mut self, output: SchemaRef) -> Self {
        self.output = output;
        self
    }
}

/// A model-requested tool invocation with opaque, schema-validated args.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    /// Unique id for this call (correlates the result).
    pub id: String,
    /// The tool to invoke.
    pub tool_id: String,
    /// The arguments, validated against the tool's input schema before use.
    pub args: serde_json::Value,
}

impl ToolCall {
    /// A tool call.
    pub fn new(id: impl Into<String>, tool_id: impl Into<String>, args: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            tool_id: tool_id.into(),
            args,
        }
    }
}

/// The result of a tool invocation, validated before crossing a boundary.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    /// The id of the [`ToolCall`] this answers.
    pub call_id: String,
    /// The typed output payload.
    pub output: serde_json::Value,
    /// Whether the tool reported an error.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    /// A successful tool result.
    pub fn ok(call_id: impl Into<String>, output: serde_json::Value) -> Self {
        Self {
            call_id: call_id.into(),
            output,
            is_error: false,
        }
    }

    /// A failed tool result carrying an error payload.
    pub fn error(call_id: impl Into<String>, output: serde_json::Value) -> Self {
        Self {
            call_id: call_id.into(),
            output,
            is_error: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_defaults_read_only_and_round_trips() {
        let tool = ToolDescriptor::new("search_docs", "Search project docs")
            .with_input(SchemaRef::named("SearchDocs"))
            .with_output(SchemaRef::named("Vec<SearchHit>"));
        assert_eq!(tool.side_effect, ToolSideEffectPolicy::ReadOnly);
        let back: ToolDescriptor =
            serde_json::from_str(&serde_json::to_string(&tool).unwrap()).unwrap();
        assert_eq!(tool, back);
    }

    #[test]
    fn side_effecting_builder_flips_policy() {
        let tool = ToolDescriptor::new("delete_doc", "Delete a doc").side_effecting();
        assert_eq!(tool.side_effect, ToolSideEffectPolicy::SideEffecting);
    }

    #[test]
    fn call_and_result_correlate_by_id() {
        let call = ToolCall::new("c1", "search_docs", serde_json::json!({"query": "x"}));
        let result = ToolResult::ok(&call.id, serde_json::json!([]));
        assert_eq!(result.call_id, call.id);
        assert!(!result.is_error);
    }
}
