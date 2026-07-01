use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::policy::ToolMode;

/// Project-local Agenkitty config.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgenkittyConfig {
    #[serde(default)]
    pub agent: AgentConfigSection,
    #[serde(default)]
    pub workspace: WorkspaceConfigSection,
    #[serde(default)]
    pub policy: PolicyConfigSection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyConfigSection {
    #[serde(default)]
    pub read_mode: ToolMode,
    #[serde(default = "ToolMode::ask")]
    pub write_mode: ToolMode,
    #[serde(default = "ToolMode::ask")]
    pub command_mode: ToolMode,
}

impl Default for PolicyConfigSection {
    fn default() -> Self {
        Self {
            read_mode: ToolMode::Allow,
            write_mode: ToolMode::Ask,
            command_mode: ToolMode::Ask,
        }
    }
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
        assert_eq!(config.policy.read_mode, ToolMode::Allow);
        assert_eq!(config.policy.command_mode, ToolMode::Ask);
    }
}
