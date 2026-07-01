use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Public framework contract for hosts that run commands/tools.
///
/// Concrete host implementations live outside the open framework.
pub trait SandboxHost: Send + Sync {
    fn describe(&self) -> SandboxDescription;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDescription {
    pub id: String,
    pub workspace_root: PathBuf,
    #[serde(default)]
    pub limits: SandboxLimits,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxLimits {
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub memory_mb: Option<u64>,
    #[serde(default)]
    pub output_bytes: Option<u64>,
}
