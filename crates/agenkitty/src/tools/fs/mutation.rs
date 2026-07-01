use std::fs;
use std::path::{Path, PathBuf};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    canonical_existing_path, canonical_root, checked_descendant_target_path,
    checked_existing_entry_path, checked_target_path,
};

macro_rules! impl_tool {
    ($tool:ty, $input:ty, $output:ty, $id:expr, $description:expr) => {
        impl AiTool for $tool {
            const ID: &'static str = $id;
            type Input = $input;
            type Output = $output;

            fn descriptor() -> ToolDescriptor {
                ToolDescriptor::new($id, $description).side_effecting()
            }

            fn call(
                &self,
                input: Self::Input,
                _ctx: AiToolContext,
            ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
                Box::pin(async move { self.run(input) })
            }
        }
    };
}

pub const FS_WRITE_TOOL_ID: &str = "fs.write";
pub const FS_APPEND_TOOL_ID: &str = "fs.append";
pub const FS_MKDIR_TOOL_ID: &str = "fs.mkdir";
pub const FS_COPY_TOOL_ID: &str = "fs.copy";
pub const FS_MOVE_TOOL_ID: &str = "fs.move";
pub const FS_REMOVE_TOOL_ID: &str = "fs.remove";

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsMutationOutput {
    pub path: String,
    pub operation: FsMutationOperation,
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FsMutationOperation {
    Write,
    Append,
    Mkdir,
    Copy,
    Move,
    Remove,
}

#[derive(Clone, Debug)]
pub struct FsWriteTool {
    root: PathBuf,
}

impl FsWriteTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsWriteInput) -> AgenkitResult<FsMutationOutput> {
        let target = checked_target_path(&self.root, &input.path, FS_WRITE_TOOL_ID)?;
        if target.is_dir() {
            return Err(AgenkitError::validation(format!(
                "fs.write path `{}` is a directory",
                input.path
            )));
        }
        let bytes = input.content.len() as u64;
        fs::write(&target, input.content)
            .map_err(|err| AgenkitError::internal(format!("write `{}`: {err}", input.path)))?;
        Ok(mutation(
            input.path,
            FsMutationOperation::Write,
            Some(bytes),
        ))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct FsWriteInput {
    pub path: String,
    pub content: String,
}

impl_tool!(
    FsWriteTool,
    FsWriteInput,
    FsMutationOutput,
    FS_WRITE_TOOL_ID,
    "Create or replace a UTF-8 text file inside the project workspace."
);

#[derive(Clone, Debug)]
pub struct FsAppendTool {
    root: PathBuf,
}

impl FsAppendTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsAppendInput) -> AgenkitResult<FsMutationOutput> {
        let target = checked_target_path(&self.root, &input.path, FS_APPEND_TOOL_ID)?;
        if target.is_dir() {
            return Err(AgenkitError::validation(format!(
                "fs.append path `{}` is a directory",
                input.path
            )));
        }
        let bytes = input.content.len() as u64;
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .map_err(|err| AgenkitError::internal(format!("open `{}`: {err}", input.path)))?;
        file.write_all(input.content.as_bytes())
            .map_err(|err| AgenkitError::internal(format!("append `{}`: {err}", input.path)))?;
        Ok(mutation(
            input.path,
            FsMutationOperation::Append,
            Some(bytes),
        ))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct FsAppendInput {
    pub path: String,
    pub content: String,
}

impl_tool!(
    FsAppendTool,
    FsAppendInput,
    FsMutationOutput,
    FS_APPEND_TOOL_ID,
    "Append UTF-8 text to a file inside the project workspace."
);

#[derive(Clone, Debug)]
pub struct FsMkdirTool {
    root: PathBuf,
}

impl FsMkdirTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsMkdirInput) -> AgenkitResult<FsMutationOutput> {
        let target = checked_descendant_target_path(&self.root, &input.path, FS_MKDIR_TOOL_ID)?;
        let changed = !target.exists();
        fs::create_dir_all(&target)
            .map_err(|err| AgenkitError::internal(format!("mkdir `{}`: {err}", input.path)))?;
        Ok(FsMutationOutput {
            path: input.path,
            operation: FsMutationOperation::Mkdir,
            changed,
            bytes: None,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct FsMkdirInput {
    pub path: String,
}

impl_tool!(
    FsMkdirTool,
    FsMkdirInput,
    FsMutationOutput,
    FS_MKDIR_TOOL_ID,
    "Create a directory inside the project workspace."
);

#[derive(Clone, Debug)]
pub struct FsCopyTool {
    root: PathBuf,
}

impl FsCopyTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsCopyInput) -> AgenkitResult<FsMutationOutput> {
        let source = canonical_existing_path(&self.root, &input.source, FS_COPY_TOOL_ID)?;
        if !source.is_file() {
            return Err(AgenkitError::validation(format!(
                "fs.copy source `{}` is not a file",
                input.source
            )));
        }
        let destination = checked_target_path(&self.root, &input.destination, FS_COPY_TOOL_ID)?;
        if destination.exists() {
            return Err(AgenkitError::validation(format!(
                "fs.copy destination `{}` already exists",
                input.destination
            )));
        }
        let bytes = fs::copy(&source, &destination).map_err(|err| {
            AgenkitError::internal(format!(
                "copy `{}` to `{}`: {err}",
                input.source, input.destination
            ))
        })?;
        Ok(mutation(
            input.destination,
            FsMutationOperation::Copy,
            Some(bytes),
        ))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct FsCopyInput {
    pub source: String,
    pub destination: String,
}

impl_tool!(
    FsCopyTool,
    FsCopyInput,
    FsMutationOutput,
    FS_COPY_TOOL_ID,
    "Copy a file inside the project workspace."
);

#[derive(Clone, Debug)]
pub struct FsMoveTool {
    root: PathBuf,
}

impl FsMoveTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsMoveInput) -> AgenkitResult<FsMutationOutput> {
        let source = checked_existing_entry_path(&self.root, &input.source, FS_MOVE_TOOL_ID, true)?;
        let destination = checked_target_path(&self.root, &input.destination, FS_MOVE_TOOL_ID)?;
        if destination.exists() {
            return Err(AgenkitError::validation(format!(
                "fs.move destination `{}` already exists",
                input.destination
            )));
        }
        fs::rename(&source, &destination).map_err(|err| {
            AgenkitError::internal(format!(
                "move `{}` to `{}`: {err}",
                input.source, input.destination
            ))
        })?;
        Ok(mutation(input.destination, FsMutationOperation::Move, None))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct FsMoveInput {
    pub source: String,
    pub destination: String,
}

impl_tool!(
    FsMoveTool,
    FsMoveInput,
    FsMutationOutput,
    FS_MOVE_TOOL_ID,
    "Move or rename a path inside the project workspace."
);

#[derive(Clone, Debug)]
pub struct FsRemoveTool {
    root: PathBuf,
}

impl FsRemoveTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsRemoveInput) -> AgenkitResult<FsMutationOutput> {
        let target = checked_existing_entry_path(&self.root, &input.path, FS_REMOVE_TOOL_ID, true)?;
        let metadata = fs::symlink_metadata(&target)
            .map_err(|err| AgenkitError::not_found(format!("metadata `{}`: {err}", input.path)))?;
        if metadata.is_dir() {
            fs::remove_dir(&target).map_err(|err| {
                AgenkitError::internal(format!("remove directory `{}`: {err}", input.path))
            })?;
        } else {
            fs::remove_file(&target)
                .map_err(|err| AgenkitError::internal(format!("remove `{}`: {err}", input.path)))?;
        }
        Ok(mutation(input.path, FsMutationOperation::Remove, None))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct FsRemoveInput {
    pub path: String,
}

impl_tool!(
    FsRemoveTool,
    FsRemoveInput,
    FsMutationOutput,
    FS_REMOVE_TOOL_ID,
    "Remove a file or empty directory inside the project workspace."
);

fn mutation(path: String, operation: FsMutationOperation, bytes: Option<u64>) -> FsMutationOutput {
    FsMutationOutput {
        path,
        operation,
        changed: true,
        bytes,
    }
}
