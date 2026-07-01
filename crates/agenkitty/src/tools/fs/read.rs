use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{canonical_existing_path, canonical_root, clamp_limit, normalize_slashes};

pub const FS_READ_TOOL_ID: &str = "fs.read";

const DEFAULT_READ_LIMIT: usize = 80;
const MAX_READ_LIMIT: usize = 200;
const MAX_READ_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct FsReadTool {
    root: PathBuf,
}

impl FsReadTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
        })
    }

    pub fn run(&self, input: FsReadInput) -> AgenkitResult<FsReadOutput> {
        let canonical = canonical_existing_path(&self.root, &input.path, FS_READ_TOOL_ID)?;
        if !canonical.is_file() {
            return Err(AgenkitError::validation(format!(
                "fs.read path `{}` is not a file",
                input.path
            )));
        }

        let start_line = input.start_line.unwrap_or(1).max(1);
        let max_lines = clamp_limit(input.max_lines, DEFAULT_READ_LIMIT, MAX_READ_LIMIT);
        let file = fs::File::open(&canonical)
            .map_err(|err| AgenkitError::internal(format!("open `{}`: {err}", input.path)))?;
        let mut reader = BufReader::new(file.take(MAX_READ_BYTES as u64 + 1));
        let mut lines = Vec::with_capacity(max_lines);
        let mut line_number = 0;
        let mut bytes_read = 0;
        let mut truncated = false;

        loop {
            let mut line = String::new();
            let count = reader.read_line(&mut line).map_err(|err| {
                if err.kind() == std::io::ErrorKind::InvalidData {
                    AgenkitError::validation(format!(
                        "fs.read path `{}` is not valid UTF-8 text",
                        input.path
                    ))
                } else {
                    AgenkitError::internal(format!("read `{}`: {err}", input.path))
                }
            })?;
            if count == 0 {
                break;
            }
            bytes_read += count;
            line_number += 1;
            if bytes_read > MAX_READ_BYTES {
                truncated = true;
                break;
            }
            if line_number >= start_line {
                lines.push(FsReadLine {
                    number: line_number,
                    text: line.trim_end_matches(['\r', '\n']).to_string(),
                });
                if lines.len() >= max_lines {
                    let mut probe = String::new();
                    truncated = reader.read_line(&mut probe).map_err(|err| {
                        if err.kind() == std::io::ErrorKind::InvalidData {
                            AgenkitError::validation(format!(
                                "fs.read path `{}` is not valid UTF-8 text",
                                input.path
                            ))
                        } else {
                            AgenkitError::internal(format!("read `{}`: {err}", input.path))
                        }
                    })? > 0;
                    break;
                }
            }
        }
        let end_line = lines.last().map(|line| line.number).unwrap_or(start_line);

        Ok(FsReadOutput {
            path: normalize_slashes(&input.path),
            start_line,
            end_line,
            lines,
            truncated,
        })
    }
}

impl AiTool for FsReadTool {
    const ID: &'static str = FS_READ_TOOL_ID;
    type Input = FsReadInput;
    type Output = FsReadOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            FS_READ_TOOL_ID,
            "Read a bounded line range from a repository file. Paths must be relative to the project root.",
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
pub struct FsReadInput {
    pub path: String,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub max_lines: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsReadOutput {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub lines: Vec<FsReadLine>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsReadLine {
    pub number: usize,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn fs_read_returns_bounded_lines() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("note.txt"), "one\ntwo\nthree\n").unwrap();
        let tool = FsReadTool::new(dir.path()).unwrap();

        let output = tool
            .run(FsReadInput {
                path: "note.txt".to_string(),
                start_line: Some(2),
                max_lines: Some(1),
            })
            .unwrap();

        assert_eq!(
            output.lines,
            vec![FsReadLine {
                number: 2,
                text: "two".to_string()
            }]
        );
        assert!(output.truncated);
    }

    #[test]
    fn fs_read_rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let tool = FsReadTool::new(dir.path()).unwrap();
        let err = tool
            .run(FsReadInput {
                path: "../secret.txt".to_string(),
                start_line: None,
                max_lines: None,
            })
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }
}
