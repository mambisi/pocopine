//! Registration helpers for the memory tool family.
//!
//! Memory is opt-in: these tools are registered on demand with a
//! [`MemoryRuntime`] and are deliberately absent from the default read-only tool
//! set (see `tools::default_read_only_tool_ids`).

use std::sync::Arc;

use pocopine_agenkit::server::AgenkitBuilder;

use super::common::MemoryRuntime;
use super::forget::{MEMORY_FORGET_TOOL_ID, MemoryForgetTool};
use super::read::{MEMORY_READ_TOOL_ID, MemoryReadTool};
use super::search::{MEMORY_SEARCH_TOOL_ID, MemorySearchTool};
use super::update::{MEMORY_UPDATE_TOOL_ID, MemoryUpdateTool};
use super::write::{MEMORY_WRITE_TOOL_ID, MemoryWriteTool};

/// Register all five model-facing memory tools against one runtime.
pub fn register_memory_tools(
    builder: AgenkitBuilder,
    runtime: Arc<MemoryRuntime>,
) -> AgenkitBuilder {
    builder
        .tool(MemorySearchTool::new(runtime.clone()))
        .tool(MemoryReadTool::new(runtime.clone()))
        .tool(MemoryWriteTool::new(runtime.clone()))
        .tool(MemoryUpdateTool::new(runtime.clone()))
        .tool(MemoryForgetTool::new(runtime))
}

/// Every memory tool id, in a stable order.
pub fn known_memory_tool_ids() -> [&'static str; 5] {
    [
        MEMORY_SEARCH_TOOL_ID,
        MEMORY_READ_TOOL_ID,
        MEMORY_WRITE_TOOL_ID,
        MEMORY_UPDATE_TOOL_ID,
        MEMORY_FORGET_TOOL_ID,
    ]
}

/// Validate and de-duplicate a caller-supplied list of memory tool ids.
pub fn resolve_memory_tool_ids(raw: &[String]) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for value in raw {
        for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if !known_memory_tool_ids().contains(&id) {
                return Err(format!("unknown memory tool `{id}`"));
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
    fn resolve_memory_tool_ids_validates_and_dedupes() {
        assert!(resolve_memory_tool_ids(&[]).unwrap().is_empty());
        assert_eq!(
            resolve_memory_tool_ids(&["memory.search,memory.search".to_string()]).unwrap(),
            vec![MEMORY_SEARCH_TOOL_ID.to_string()]
        );
        assert!(resolve_memory_tool_ids(&["memory.nope".to_string()]).is_err());
    }

    #[test]
    fn known_ids_are_unique_and_namespaced() {
        let ids = known_memory_tool_ids();
        for id in ids {
            assert!(id.starts_with("memory."));
        }
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    #[test]
    fn mutating_tools_are_side_effecting() {
        use pocopine_agenkit::server::AiTool;
        use pocopine_agenkit_core::ToolSideEffectPolicy::{ReadOnly, SideEffecting};

        assert_eq!(MemorySearchTool::descriptor().side_effect, ReadOnly);
        assert_eq!(MemoryReadTool::descriptor().side_effect, ReadOnly);
        assert_eq!(MemoryWriteTool::descriptor().side_effect, SideEffecting);
        assert_eq!(MemoryUpdateTool::descriptor().side_effect, SideEffecting);
        assert_eq!(MemoryForgetTool::descriptor().side_effect, SideEffecting);
    }
}
