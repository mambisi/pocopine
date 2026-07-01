use std::path::{Path, PathBuf};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{canonical_root, reject_secret_path, validate_relative_path};

pub const FS_EXISTS_TOOL_ID: &str = "fs.exists";

const MAX_EXISTS_PATHS: usize = 100;

#[derive(Clone, Debug)]
pub struct FsExistsTool {
    root: PathBuf,
}

impl FsExistsTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsExistsInput) -> AgenkitResult<FsExistsOutput> {
        if input.paths.is_empty() {
            return Err(AgenkitError::validation(
                "fs.exists requires at least one path",
            ));
        }
        if input.paths.len() > MAX_EXISTS_PATHS {
            return Err(AgenkitError::validation(format!(
                "fs.exists accepts at most {MAX_EXISTS_PATHS} paths"
            )));
        }

        let mut paths = Vec::with_capacity(input.paths.len());
        for raw_path in input.paths {
            let relative = validate_relative_path(&raw_path, FS_EXISTS_TOOL_ID)?;
            reject_secret_path(relative, FS_EXISTS_TOOL_ID)?;
            let target = self.root.join(relative);
            let exists = match target.canonicalize() {
                Ok(canonical) => {
                    if !canonical.starts_with(&self.root) {
                        false
                    } else {
                        let resolved_relative =
                            canonical.strip_prefix(&self.root).map_err(|_| {
                                AgenkitError::tool_policy(format!(
                                    "fs.exists path `{raw_path}` escapes the project root"
                                ))
                            })?;
                        reject_secret_path(resolved_relative, FS_EXISTS_TOOL_ID)?;
                        true
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
                Err(err) => {
                    return Err(AgenkitError::internal(format!(
                        "resolve `{raw_path}`: {err}"
                    )));
                }
            };
            paths.push(FsExistsEntry {
                path: raw_path,
                exists,
            });
        }

        Ok(FsExistsOutput { paths })
    }
}

impl AiTool for FsExistsTool {
    const ID: &'static str = FS_EXISTS_TOOL_ID;
    type Input = FsExistsInput;
    type Output = FsExistsOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            FS_EXISTS_TOOL_ID,
            "Check whether project-relative paths exist without reading contents.",
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
pub struct FsExistsInput {
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsExistsOutput {
    pub paths: Vec<FsExistsEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsExistsEntry {
    pub path: String,
    pub exists: bool,
}
