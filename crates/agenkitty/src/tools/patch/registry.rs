use std::path::Path;

use pocopine_agenkit::server::AgenkitBuilder;
use pocopine_agenkit_core::AgenkitResult;

use super::apply::{PATCH_APPLY_TOOL_ID, PatchApplyTool};
use super::preview::{PATCH_PREVIEW_TOOL_ID, PatchPreviewTool};
use crate::tools::fs::common::canonical_root;

pub fn register_patch_tools(
    builder: AgenkitBuilder,
    root: impl AsRef<Path>,
) -> AgenkitResult<AgenkitBuilder> {
    let root = canonical_root(root.as_ref())?;
    Ok(builder
        .tool(PatchPreviewTool::new(&root)?)
        .tool(PatchApplyTool::new(&root)?))
}

pub fn known_patch_tool_ids() -> [&'static str; 2] {
    [PATCH_PREVIEW_TOOL_ID, PATCH_APPLY_TOOL_ID]
}
