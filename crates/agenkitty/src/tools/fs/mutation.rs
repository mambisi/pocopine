use std::fs;
use std::path::{Path, PathBuf};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    canonical_existing_path, canonical_root, checked_descendant_target_path,
    checked_existing_entry_path, checked_target_path, reject_secret_path,
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
        let destination = checked_target_path(&self.root, &input.destination, FS_COPY_TOOL_ID)?;
        if destination.exists() {
            return Err(AgenkitError::validation(format!(
                "fs.copy destination `{}` already exists",
                input.destination
            )));
        }
        if source.is_dir() {
            if !input.recursive.unwrap_or(false) {
                return Err(AgenkitError::validation(format!(
                    "fs.copy source `{}` is a directory; pass `recursive: true` to copy the tree",
                    input.source
                )));
            }
            // Refuse copying a directory into its own subtree: `copy_tree`
            // creates the destination first, so `read_dir(source)` would then
            // include it and recurse into it forever (src/copy/copy/...). The
            // destination does not exist yet, so we test its (existing) parent
            // against the canonical source root.
            if let Some(parent) = destination.parent()
                && let Ok(parent) = parent.canonicalize()
                && parent.starts_with(&source)
            {
                return Err(AgenkitError::validation(format!(
                    "fs.copy destination `{}` is inside the source directory `{}`",
                    input.destination, input.source
                )));
            }
            // On any failure mid-walk (a secret entry, a special file, an IO
            // error) remove the partial copy so fs.copy is all-or-nothing.
            let bytes = match copy_tree(&source, &destination, &self.root) {
                Ok(bytes) => bytes,
                Err(err) => {
                    let _ = fs::remove_dir_all(&destination);
                    return Err(err);
                }
            };
            return Ok(mutation(
                input.destination,
                FsMutationOperation::Copy,
                Some(bytes),
            ));
        }
        if !source.is_file() {
            return Err(AgenkitError::validation(format!(
                "fs.copy source `{}` is not a regular file or directory",
                input.source
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
    /// Copy a directory tree (required when `source` is a directory).
    #[serde(default)]
    pub recursive: Option<bool>,
}

impl_tool!(
    FsCopyTool,
    FsCopyInput,
    FsMutationOutput,
    FS_COPY_TOOL_ID,
    "Copy a file, or a directory tree with `recursive: true`, inside the project workspace."
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
            if input.recursive.unwrap_or(false) {
                // `remove_dir_all` removes symlinks as links (it does not follow
                // them out of the tree). `target` is already confined under the
                // workspace root by `checked_existing_entry_path`.
                fs::remove_dir_all(&target).map_err(|err| {
                    AgenkitError::internal(format!(
                        "recursively remove directory `{}`: {err}",
                        input.path
                    ))
                })?;
            } else {
                // Distinguish "non-empty" (the actionable case) from any other
                // OS error (permissions, a concurrent removal) — mapping every
                // failure to "not empty" would mislead. Check emptiness first,
                // then surface a faithful error for anything else.
                let non_empty = fs::read_dir(&target)
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(false);
                if non_empty {
                    return Err(AgenkitError::validation(format!(
                        "fs.remove directory `{}` is not empty; pass `recursive: true` to remove \
                         the tree",
                        input.path
                    )));
                }
                fs::remove_dir(&target).map_err(|err| {
                    AgenkitError::internal(format!("remove directory `{}`: {err}", input.path))
                })?;
            }
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
    /// Remove a non-empty directory tree (required for a non-empty directory).
    #[serde(default)]
    pub recursive: Option<bool>,
}

impl_tool!(
    FsRemoveTool,
    FsRemoveInput,
    FsMutationOutput,
    FS_REMOVE_TOOL_ID,
    "Remove a file, an empty directory, or a directory tree with `recursive: true`, inside the \
     project workspace."
);

/// Recursively copy the directory at `source` to `destination`, confined to the
/// workspace: directories are created, regular files copied, symlinks recreated
/// as links (never followed out of the tree), and special files refused.
/// Returns the total bytes of file content copied.
///
/// Every source entry is re-checked against the secret-file policy relative to
/// `root`: a recursive copy must not relocate a secret-classified file (e.g.
/// `.docker/config.json`) to a non-classified destination where `fs.read` would
/// then serve it — the secret-path guard is path-based, so the copy has to
/// re-assert it per entry, not just on the top-level source/destination.
///
/// Note: like the rest of the fs tools, this is not itself a security boundary
/// (the OS sandbox is) — it classifies each entry from a pre-walk `file_type()`
/// and then acts on the path, so a *concurrent* adversarial swap of an entry
/// mid-walk is a known TOCTOU limitation. Descriptor-relative (`openat2`)
/// hardening is trigger-gated in the roadmap, not part of today's threat model.
fn copy_tree(source: &Path, destination: &Path, root: &Path) -> AgenkitResult<u64> {
    fn io<E: std::fmt::Display>(context: &str, err: E) -> AgenkitError {
        AgenkitError::internal(format!("{context}: {err}"))
    }
    fs::create_dir(destination).map_err(|err| io("create directory", err))?;
    let mut total = 0u64;
    for entry in fs::read_dir(source).map_err(|err| io("read directory", err))? {
        let entry = entry.map_err(|err| io("read directory entry", err))?;
        let file_type = entry
            .file_type()
            .map_err(|err| io("entry file type", err))?;
        let src = entry.path();
        // Refuse relocating a secret-classified source entry — copying it out of
        // its classified path would defeat the read guard on the copy.
        if let Ok(relative) = src.strip_prefix(root) {
            reject_secret_path(relative, FS_COPY_TOOL_ID)?;
        }
        let dst = destination.join(entry.file_name());
        if file_type.is_symlink() {
            // Recreate the link verbatim; the fs read guard re-confines any
            // out-of-tree target when it is later read.
            let link_target = fs::read_link(&src).map_err(|err| io("read symlink", err))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link_target, &dst)
                .map_err(|err| io("create symlink", err))?;
            #[cfg(not(unix))]
            return Err(AgenkitError::internal(
                "symlink copy is unsupported on this platform",
            ));
        } else if file_type.is_dir() {
            total += copy_tree(&src, &dst, root)?;
        } else if file_type.is_file() {
            total += fs::copy(&src, &dst).map_err(|err| io("copy file", err))?;
        } else {
            // A special file (fifo, socket, device). `fs::copy` on a fifo would
            // BLOCK waiting for a writer; on others it is meaningless. Refuse
            // rather than hang or copy garbage — the single-file fs.copy path
            // rejects non-regular sources for the same reason.
            return Err(AgenkitError::validation(format!(
                "fs.copy cannot copy special file `{}` (not a regular file, directory, or symlink)",
                entry.file_name().to_string_lossy()
            )));
        }
    }
    Ok(total)
}

fn mutation(path: String, operation: FsMutationOperation, bytes: Option<u64>) -> FsMutationOutput {
    FsMutationOutput {
        path,
        operation,
        changed: true,
        bytes,
    }
}
