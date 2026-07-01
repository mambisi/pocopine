use serde::{Deserialize, Serialize};

/// Reusable or inline framework agent definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentSpec {
    pub id: String,
    pub model: String,
    pub instructions: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    #[serde(default)]
    pub workspace: WorkspaceSpec,
    #[serde(default)]
    pub sandbox: SandboxSpec,
    #[serde(default)]
    pub budget: BudgetSpec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSpec {
    #[serde(default = "default_workspace_mode")]
    pub mode: String,
    #[serde(default)]
    pub writable_roots: Vec<String>,
}

impl Default for WorkspaceSpec {
    fn default() -> Self {
        Self {
            mode: default_workspace_mode(),
            writable_roots: vec![".".to_string()],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSpec {
    #[serde(default = "default_sandbox_runtime")]
    pub runtime: String,
    #[serde(default = "default_network")]
    pub network: String,
}

impl Default for SandboxSpec {
    fn default() -> Self {
        Self {
            runtime: default_sandbox_runtime(),
            network: default_network(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetSpec {
    #[serde(default)]
    pub max_usd: Option<f64>,
    #[serde(default)]
    pub max_child_agents: Option<u32>,
}

fn default_max_steps() -> u32 {
    8
}

fn default_workspace_mode() -> String {
    "copy".to_string()
}

fn default_sandbox_runtime() -> String {
    "local".to_string()
}

fn default_network() -> String {
    "none".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_spec_defaults_network_to_none() {
        let spec: AgentSpec = toml::from_str(
            r#"
id = "reviewer"
model = "local/default"
instructions = "Review the patch."
"#,
        )
        .unwrap();
        assert_eq!(spec.max_steps, 8);
        assert_eq!(spec.sandbox.network, "none");
    }
}
