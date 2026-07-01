use std::path::Path;

use pocopine_agenkit::server::AgenkitBuilder;
use pocopine_agenkit_core::AgenkitResult;

use super::common::canonical_root;
use super::exists::{FS_EXISTS_TOOL_ID, FsExistsTool};
use super::list::{FS_LIST_TOOL_ID, FsListTool};
use super::mutation::{
    FS_APPEND_TOOL_ID, FS_COPY_TOOL_ID, FS_MKDIR_TOOL_ID, FS_MOVE_TOOL_ID, FS_REMOVE_TOOL_ID,
    FS_WRITE_TOOL_ID, FsAppendTool, FsCopyTool, FsMkdirTool, FsMoveTool, FsRemoveTool, FsWriteTool,
};
use super::read::{FS_READ_TOOL_ID, FsReadTool};
use super::search::{FS_SEARCH_TOOL_ID, FsSearchTool};
use super::stat::{FS_STAT_TOOL_ID, FsStatTool};

pub fn register_fs_tools(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
) -> AgenkitResult<AgenkitBuilder> {
    let root = canonical_root(root.as_ref())?;
    Ok(builder
        .tool(FsSearchTool::new(&root)?)
        .tool(FsListTool::new(&root)?)
        .tool(FsReadTool::new(&root)?)
        .tool(FsStatTool::new(&root)?)
        .tool(FsExistsTool::new(&root)?)
        .tool(FsWriteTool::new(&root)?)
        .tool(FsAppendTool::new(&root)?)
        .tool(FsMkdirTool::new(&root)?)
        .tool(FsCopyTool::new(&root)?)
        .tool(FsMoveTool::new(&root)?)
        .tool(FsRemoveTool::new(&root)?))
}

pub fn default_read_only_tool_ids() -> Vec<String> {
    vec![
        FS_SEARCH_TOOL_ID.to_string(),
        FS_LIST_TOOL_ID.to_string(),
        FS_READ_TOOL_ID.to_string(),
        FS_STAT_TOOL_ID.to_string(),
        FS_EXISTS_TOOL_ID.to_string(),
    ]
}

pub fn resolve_tool_ids(raw: &[String]) -> Result<Vec<String>, String> {
    if raw.is_empty() {
        return Ok(default_read_only_tool_ids());
    }

    let mut ids = Vec::new();
    for value in raw {
        for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if id.eq_ignore_ascii_case("none") {
                return Ok(Vec::new());
            }
            if !known_tool_ids().contains(&id) {
                return Err(format!("unknown tool `{id}`"));
            }
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

fn known_tool_ids() -> [&'static str; 11] {
    [
        FS_SEARCH_TOOL_ID,
        FS_LIST_TOOL_ID,
        FS_READ_TOOL_ID,
        FS_STAT_TOOL_ID,
        FS_EXISTS_TOOL_ID,
        FS_WRITE_TOOL_ID,
        FS_APPEND_TOOL_ID,
        FS_MKDIR_TOOL_ID,
        FS_COPY_TOOL_ID,
        FS_MOVE_TOOL_ID,
        FS_REMOVE_TOOL_ID,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_id_resolution_defaults_and_validates() {
        assert_eq!(resolve_tool_ids(&[]).unwrap(), default_read_only_tool_ids());
        assert!(resolve_tool_ids(&["none".to_string()]).unwrap().is_empty());
        assert!(resolve_tool_ids(&["fs.nope".to_string()]).is_err());
    }
}
