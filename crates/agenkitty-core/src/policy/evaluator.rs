use std::collections::HashMap;

use pocopine_agenkit_core::ToolCall;

use super::decision::{PolicyDecision, ToolMode};
use crate::config::PolicyConfigSection;
use crate::tools::{ToolClass, ToolSpec};

/// Namespace prefix for MCP tools (`mcp.<verb>` and imported
/// `mcp.<server>.<tool>` adapters). Their ids are dynamic per-server, so the
/// evaluator recognizes the family by prefix rather than a static spec.
const MCP_TOOL_PREFIX: &str = "mcp.";

/// The central tool-call policy evaluator (F1).
///
/// Maps a tool call to a [`PolicyDecision`] from two inputs:
/// - the tool's [`ToolSpec`] — its [`ToolClass`] and per-tool default
///   [`ToolMode`] (the per-family registration metadata), and
/// - the project's [`PolicyConfigSection`] — optional per-class overrides
///   (`read_mode` / `write_mode` / `command_mode` / `network_mode`).
///
/// Resolution: an explicit class override wins; otherwise the spec's own mode
/// applies. Tools classed [`ToolClass::Other`] are never affected by class
/// overrides — their spec mode always rules (secrets, MCP verbs).
///
/// This is the *outer* gate at the agent-loop dispatch seam. Subsystem gates
/// stay authoritative for their domain regardless of what this evaluator
/// allows: MCP capability admission still gates `mcp.*` calls inside the tool,
/// and the secret runtime still gates grant resolution — defense in depth, not
/// a bypass.
#[derive(Clone, Debug, Default)]
pub struct PolicyEvaluator {
    config: PolicyConfigSection,
    specs: HashMap<String, ToolSpec>,
}

impl PolicyEvaluator {
    pub fn new(config: PolicyConfigSection, specs: impl IntoIterator<Item = ToolSpec>) -> Self {
        Self {
            config,
            specs: specs
                .into_iter()
                .map(|spec| (spec.descriptor.id.clone(), spec))
                .collect(),
        }
    }

    /// The spec registered for a tool id, if any.
    pub fn spec(&self, tool_id: &str) -> Option<&ToolSpec> {
        self.specs.get(tool_id)
    }

    /// Evaluate one tool call against the registered specs + config overrides.
    pub fn evaluate_call(&self, call: &ToolCall) -> PolicyDecision {
        let Some(spec) = self.specs.get(&call.tool_id) else {
            // Imported MCP adapters (`mcp.<server>.<tool>`) are runtime-discovered;
            // their real gate is the MCP capability admission inside the tool.
            // Anything else without a spec fails toward approval, never silently
            // through.
            if call.tool_id.starts_with(MCP_TOOL_PREFIX) {
                return PolicyDecision::Allow;
            }
            return PolicyDecision::Ask {
                reason: format!("tool `{}` has no policy spec", call.tool_id),
            };
        };
        let mode = self.effective_mode(spec);
        match mode {
            ToolMode::Allow => PolicyDecision::Allow,
            ToolMode::Ask => PolicyDecision::Ask {
                reason: ask_reason(spec),
            },
            ToolMode::Deny => PolicyDecision::Deny {
                reason: deny_reason(spec),
            },
        }
    }

    /// The mode a spec resolves to under the config: an explicit class override
    /// wins; otherwise the spec's own default mode.
    pub fn effective_mode(&self, spec: &ToolSpec) -> ToolMode {
        let class_override = match spec.class {
            ToolClass::Read => self.config.read_mode,
            ToolClass::Write => self.config.write_mode,
            ToolClass::Command => self.config.command_mode,
            ToolClass::Network => self.config.network_mode,
            ToolClass::Other => None,
        };
        class_override.unwrap_or(spec.mode)
    }
}

fn ask_reason(spec: &ToolSpec) -> String {
    format!(
        "tool `{}` ({} class) requires approval",
        spec.descriptor.id,
        spec.class.as_str()
    )
}

fn deny_reason(spec: &ToolSpec) -> String {
    format!(
        "tool `{}` is denied by {} policy",
        spec.descriptor.id,
        spec.class.as_str()
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::ToolClass;

    fn spec(id: &str, class: ToolClass, mode: ToolMode) -> ToolSpec {
        ToolSpec::built_in(id, format!("{id} test tool"))
            .with_class(class)
            .with_mode(mode)
    }

    fn call(tool_id: &str) -> ToolCall {
        ToolCall::new("call-1", tool_id, json!({}))
    }

    #[test]
    fn spec_mode_applies_when_config_has_no_override() {
        let evaluator = PolicyEvaluator::new(
            PolicyConfigSection::default(),
            [
                spec("fs.read", ToolClass::Read, ToolMode::Allow),
                spec("fs.write", ToolClass::Write, ToolMode::Ask),
            ],
        );
        assert_eq!(
            evaluator.evaluate_call(&call("fs.read")),
            PolicyDecision::Allow
        );
        assert!(matches!(
            evaluator.evaluate_call(&call("fs.write")),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn class_override_wins_over_spec_mode() {
        let config = PolicyConfigSection {
            write_mode: Some(ToolMode::Deny),
            ..PolicyConfigSection::default()
        };
        let evaluator =
            PolicyEvaluator::new(config, [spec("fs.write", ToolClass::Write, ToolMode::Ask)]);
        assert!(matches!(
            evaluator.evaluate_call(&call("fs.write")),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn class_override_can_loosen_a_class() {
        let config = PolicyConfigSection {
            command_mode: Some(ToolMode::Allow),
            ..PolicyConfigSection::default()
        };
        let evaluator = PolicyEvaluator::new(
            config,
            [spec("process.run", ToolClass::Command, ToolMode::Ask)],
        );
        assert_eq!(
            evaluator.evaluate_call(&call("process.run")),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn other_class_ignores_every_override() {
        let config = PolicyConfigSection {
            read_mode: Some(ToolMode::Allow),
            write_mode: Some(ToolMode::Allow),
            command_mode: Some(ToolMode::Allow),
            network_mode: Some(ToolMode::Allow),
        };
        let evaluator = PolicyEvaluator::new(
            config,
            [spec("secret.use", ToolClass::Other, ToolMode::Ask)],
        );
        assert!(matches!(
            evaluator.evaluate_call(&call("secret.use")),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn unknown_tool_asks_but_mcp_defers_to_inner_admission() {
        let evaluator = PolicyEvaluator::new(PolicyConfigSection::default(), []);
        assert!(matches!(
            evaluator.evaluate_call(&call("rogue.tool")),
            PolicyDecision::Ask { .. }
        ));
        // MCP adapters are gated by capability admission inside the tool.
        assert_eq!(
            evaluator.evaluate_call(&call("mcp.docs.search")),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn network_class_follows_network_mode() {
        let config = PolicyConfigSection {
            network_mode: Some(ToolMode::Deny),
            ..PolicyConfigSection::default()
        };
        let evaluator = PolicyEvaluator::new(
            config,
            [spec("net.fetch", ToolClass::Network, ToolMode::Ask)],
        );
        assert!(matches!(
            evaluator.evaluate_call(&call("net.fetch")),
            PolicyDecision::Deny { .. }
        ));
    }
}
