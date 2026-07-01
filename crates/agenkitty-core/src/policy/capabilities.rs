use serde::{Deserialize, Serialize};

/// Capability request/grant carried by policy-gated tools and sub-agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    #[serde(default)]
    pub filesystem: FilesystemCapability,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub commands: Vec<Vec<String>>,
}

impl Default for CapabilitySet {
    fn default() -> Self {
        Self {
            filesystem: FilesystemCapability::None,
            network: false,
            commands: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemCapability {
    #[default]
    None,
    Read,
    Write,
}
