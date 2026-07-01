use std::fs;
use std::path::{Path, PathBuf};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    FsPathKind, canonical_existing_path, canonical_root, clamp_limit, reject_secret_path,
    relative_display_path, symlink_metadata_kind,
};

pub const FS_LIST_TOOL_ID: &str = "fs.list";

const DEFAULT_LIST_LIMIT: usize = 80;
const MAX_LIST_LIMIT: usize = 300;

#[derive(Clone, Debug)]
pub struct FsListTool {
    root: PathBuf,
}

impl FsListTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsListInput) -> AgenkitResult<FsListOutput> {
        let requested_path = input.path.as_deref().unwrap_or(".");
        let canonical = canonical_existing_path(&self.root, requested_path, FS_LIST_TOOL_ID)?;
        if !canonical.is_dir() {
            return Err(AgenkitError::validation(format!(
                "fs.list path `{requested_path}` is not a directory"
            )));
        }

        let include_hidden = input.include_hidden.unwrap_or(false);
        let max_entries = clamp_limit(input.max_entries, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT);
        let mut entries = Vec::new();

        for entry in fs::read_dir(&canonical)
            .map_err(|err| AgenkitError::internal(format!("list `{requested_path}`: {err}")))?
        {
            let entry = entry.map_err(|err| {
                AgenkitError::internal(format!("read entry in `{requested_path}`: {err}"))
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !include_hidden && name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let relative = path.strip_prefix(&self.root).unwrap_or(&path);
            if reject_secret_path(relative, FS_LIST_TOOL_ID).is_err() {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(|err| {
                AgenkitError::internal(format!("read metadata `{}`: {err}", path.display()))
            })?;
            entries.push(FsListEntry {
                name,
                path: relative_display_path(&self.root, &path),
                kind: symlink_metadata_kind(&path)?,
                size_bytes: metadata.is_file().then_some(metadata.len()),
            });
        }

        entries.sort_by(|left, right| {
            entry_sort_rank(left.kind)
                .cmp(&entry_sort_rank(right.kind))
                .then_with(|| left.name.cmp(&right.name))
        });
        let truncated = entries.len() > max_entries;
        entries.truncate(max_entries);

        Ok(FsListOutput {
            path: requested_path.to_string(),
            entries,
            truncated,
        })
    }
}

impl AiTool for FsListTool {
    const ID: &'static str = FS_LIST_TOOL_ID;
    type Input = FsListInput;
    type Output = FsListOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            FS_LIST_TOOL_ID,
            "List entries in a repository directory. Use this to orient before reading files.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.run(input) })
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct FsListInput {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub max_entries: Option<usize>,
    #[serde(default)]
    pub include_hidden: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsListOutput {
    pub path: String,
    pub entries: Vec<FsListEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsListEntry {
    pub name: String,
    pub path: String,
    pub kind: FsPathKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

fn entry_sort_rank(kind: FsPathKind) -> u8 {
    match kind {
        FsPathKind::Directory => 0,
        FsPathKind::File => 1,
        FsPathKind::Symlink => 2,
        FsPathKind::Other => 3,
    }
}
