//! Filesystem-backed host tools.

pub(crate) mod common;
mod exists;
mod list;
mod mutation;
mod read;
mod registry;
mod search;
mod stat;
#[cfg(test)]
mod tests;

pub use common::FsPathKind;
pub use exists::{FS_EXISTS_TOOL_ID, FsExistsEntry, FsExistsInput, FsExistsOutput, FsExistsTool};
pub use list::{FS_LIST_TOOL_ID, FsListEntry, FsListInput, FsListOutput, FsListTool};
pub use mutation::{
    FS_APPEND_TOOL_ID, FS_COPY_TOOL_ID, FS_MKDIR_TOOL_ID, FS_MOVE_TOOL_ID, FS_REMOVE_TOOL_ID,
    FS_WRITE_TOOL_ID, FsAppendInput, FsAppendTool, FsCopyInput, FsCopyTool, FsMkdirInput,
    FsMkdirTool, FsMoveInput, FsMoveTool, FsMutationOperation, FsMutationOutput, FsRemoveInput,
    FsRemoveTool, FsWriteInput, FsWriteTool,
};
pub use read::{FS_READ_TOOL_ID, FsReadInput, FsReadLine, FsReadOutput, FsReadTool};
pub use registry::{default_read_only_tool_ids, register_fs_tools, resolve_tool_ids};
pub use search::{
    FS_SEARCH_TOOL_ID, FsSearchEngine, FsSearchHit, FsSearchInput, FsSearchOutput, FsSearchTool,
};
pub use stat::{FS_STAT_TOOL_ID, FsStatInput, FsStatOutput, FsStatTool};
