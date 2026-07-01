use std::fs;
use std::path::{Path, PathBuf};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{FsPathKind, canonical_existing_path, canonical_root, metadata_kind};

pub const FS_STAT_TOOL_ID: &str = "fs.stat";

#[derive(Clone, Debug)]
pub struct FsStatTool {
    root: PathBuf,
}

impl FsStatTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsStatInput) -> AgenkitResult<FsStatOutput> {
        let canonical = canonical_existing_path(&self.root, &input.path, FS_STAT_TOOL_ID)?;
        let metadata = fs::metadata(&canonical).map_err(|err| {
            AgenkitError::internal(format!("read metadata `{}`: {err}", input.path))
        })?;

        Ok(FsStatOutput {
            path: input.path,
            kind: metadata_kind(&metadata),
            size_bytes: metadata.is_file().then_some(metadata.len()),
            readonly: metadata.permissions().readonly(),
        })
    }
}

impl AiTool for FsStatTool {
    const ID: &'static str = FS_STAT_TOOL_ID;
    type Input = FsStatInput;
    type Output = FsStatOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            FS_STAT_TOOL_ID,
            "Inspect repository path metadata without reading file contents.",
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

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct FsStatInput {
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsStatOutput {
    pub path: String,
    pub kind: FsPathKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    pub readonly: bool,
}
