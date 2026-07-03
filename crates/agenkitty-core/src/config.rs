use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::policy::ToolMode;

/// Project-local Agenkitty config. Unknown keys are hard parse errors
/// (`deny_unknown_fields`): a misspelled policy key must never silently run
/// under the defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgenkittyConfig {
    #[serde(default)]
    pub agent: AgentConfigSection,
    #[serde(default)]
    pub workspace: WorkspaceConfigSection,
    #[serde(default)]
    pub policy: PolicyConfigSection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigSection {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub instructions: Vec<PathBuf>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_max_steps")]
    pub max_steps_per_turn: u32,
}

impl Default for AgentConfigSection {
    fn default() -> Self {
        Self {
            id: "agenkitty".to_string(),
            model: "local/default".to_string(),
            instructions: vec![PathBuf::from("AGENTS.md")],
            tools: Vec::new(),
            max_steps_per_turn: default_max_steps(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfigSection {
    pub root: PathBuf,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default = "default_excludes")]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub writable_roots: Vec<PathBuf>,
}

impl Default for WorkspaceConfigSection {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            include: Vec::new(),
            exclude: default_excludes(),
            writable_roots: vec![PathBuf::from(".")],
        }
    }
}

/// Per-class [`ToolMode`] overrides. `None` (the field absent from the config
/// file) defers to each tool's own spec-declared default mode; `Some(mode)`
/// overrides the whole class — tightening (`deny`) or loosening (`allow`).
/// Tools classed `Other` (secrets, MCP verbs) are never affected by these —
/// their spec mode / subsystem gate always rules.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfigSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_mode: Option<ToolMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_mode: Option<ToolMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_mode: Option<ToolMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<ToolMode>,
}

fn default_max_steps() -> u32 {
    8
}

fn default_excludes() -> Vec<String> {
    vec![
        "target/**".to_string(),
        ".git/**".to_string(),
        ".env*".to_string(),
        ".agenkitty/sessions/**".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_conservative() {
        let config = AgenkittyConfig::default();
        assert_eq!(config.agent.model, "local/default");
        assert!(config.workspace.exclude.iter().any(|p| p == ".env*"));
        // No class overrides by default: each tool's spec-declared mode rules.
        assert_eq!(config.policy, PolicyConfigSection::default());
        assert_eq!(config.policy.read_mode, None);
        assert_eq!(config.policy.command_mode, None);
    }

    #[test]
    fn policy_overrides_parse_from_toml() {
        let section: PolicyConfigSection =
            toml::from_str("write_mode = \"deny\"\nnetwork_mode = \"allow\"").unwrap();
        assert_eq!(section.write_mode, Some(ToolMode::Deny));
        assert_eq!(section.network_mode, Some(ToolMode::Allow));
        assert_eq!(section.read_mode, None);
        assert_eq!(section.command_mode, None);
    }

    #[test]
    fn misspelled_config_keys_are_hard_errors() {
        // A typo must never silently fail open to the default policy.
        assert!(toml::from_str::<PolicyConfigSection>("write_mdoe = \"deny\"").is_err());
        assert!(toml::from_str::<AgenkittyConfig>("[polciy]\nwrite_mode = \"deny\"").is_err());
    }
}
