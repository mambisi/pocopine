//! Artefact produced by `build_artefact` and consumed by `deploy`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Artefact {
    /// Multi-process OCI image built from `pocopine-deploy`'s generated
    /// `Dockerfile`. Adapters push this to the host's registry.
    OciImage { tag: String },
    /// Client bundle for static hosts.
    StaticDist { path: PathBuf },
}

/// In-memory staging dir written by `render_config` and flushed (or
/// printed) by the orchestrator. Keeps pure config-rendering testable
/// without filesystem I/O.
#[derive(Debug, Default, Clone)]
pub struct StagedFiles {
    files: BTreeMap<String, String>,
}

impl StagedFiles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&mut self, path: impl Into<String>, content: impl Into<String>) {
        self.files.insert(path.into(), content.into());
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn get(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}
