//! Registration helpers for the artifact tool family.
//!
//! Artifacts are opt-in like memory: the tools register on demand with an
//! [`ArtifactRuntime`] and are deliberately absent from the default read-only
//! tool set (see `tools::default_read_only_tool_ids`).

use std::sync::Arc;

use pocopine_agenkit::server::AgenkitBuilder;

use super::common::ArtifactRuntime;
use super::delete::{ARTIFACT_DELETE_TOOL_ID, ArtifactDeleteTool};
use super::link::{ARTIFACT_LINK_TOOL_ID, ArtifactLinkTool};
use super::list::{ARTIFACT_LIST_TOOL_ID, ArtifactListTool};
use super::read::{ARTIFACT_READ_TOOL_ID, ArtifactReadTool};
use super::write::{ARTIFACT_WRITE_TOOL_ID, ArtifactWriteTool};

/// Register all five model-facing artifact tools against one runtime.
pub fn register_artifact_tools(
    builder: AgenkitBuilder,
    runtime: Arc<ArtifactRuntime>,
) -> AgenkitBuilder {
    builder
        .tool(ArtifactWriteTool::new(runtime.clone()))
        .tool(ArtifactReadTool::new(runtime.clone()))
        .tool(ArtifactListTool::new(runtime.clone()))
        .tool(ArtifactLinkTool::new(runtime.clone()))
        .tool(ArtifactDeleteTool::new(runtime))
}

/// Every artifact tool id, in a stable order.
pub fn known_artifact_tool_ids() -> [&'static str; 5] {
    [
        ARTIFACT_WRITE_TOOL_ID,
        ARTIFACT_READ_TOOL_ID,
        ARTIFACT_LIST_TOOL_ID,
        ARTIFACT_LINK_TOOL_ID,
        ARTIFACT_DELETE_TOOL_ID,
    ]
}

/// Validate and de-duplicate a caller-supplied list of artifact tool ids.
pub fn resolve_artifact_tool_ids(raw: &[String]) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for value in raw {
        for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if !known_artifact_tool_ids().contains(&id) {
                return Err(format!("unknown artifact tool `{id}`"));
            }
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids_are_unique_and_namespaced() {
        let ids = known_artifact_tool_ids();
        for id in ids {
            assert!(id.starts_with("artifact."));
        }
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    #[test]
    fn resolve_artifact_tool_ids_validates_and_dedupes() {
        assert!(resolve_artifact_tool_ids(&[]).unwrap().is_empty());
        assert_eq!(
            resolve_artifact_tool_ids(&["artifact.read,artifact.read".to_string()]).unwrap(),
            vec![ARTIFACT_READ_TOOL_ID.to_string()]
        );
        assert!(resolve_artifact_tool_ids(&["artifact.nope".to_string()]).is_err());
    }
}
