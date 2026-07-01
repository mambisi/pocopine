use std::io;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::SearcherBuilder;
use grep_searcher::sinks::Lossy;
use ignore::overrides::OverrideBuilder;
use ignore::{DirEntry, WalkBuilder};
use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{
    canonical_root, clamp_limit, normalize_slashes, reject_secret_path, relative_display_path,
};

pub const FS_SEARCH_TOOL_ID: &str = "fs.search";

const DEFAULT_SEARCH_LIMIT: usize = 20;
const MAX_SEARCH_LIMIT: usize = 100;

#[derive(Clone, Debug)]
pub struct FsSearchTool {
    root: PathBuf,
    rg_binary: String,
}

impl FsSearchTool {
    pub fn new(root: impl AsRef<Path>) -> AgenkitResult<Self> {
        Ok(Self {
            root: canonical_root(root.as_ref())?,
            rg_binary: "rg".to_string(),
        })
    }

    pub fn with_rg_binary(mut self, rg_binary: impl Into<String>) -> Self {
        self.rg_binary = rg_binary.into();
        self
    }

    pub fn run(&self, input: FsSearchInput) -> AgenkitResult<FsSearchOutput> {
        let options = SearchOptions::from_input(input)?;
        match self.run_with_rg(&options) {
            Ok(output) => Ok(output),
            Err(SearchError::RipgrepMissing) => self.run_with_rust_fallback(&options),
            Err(SearchError::Other(error)) => Err(error),
        }
    }

    fn run_with_rg(&self, options: &SearchOptions) -> Result<FsSearchOutput, SearchError> {
        let mut command = Command::new(&self.rg_binary);
        command
            .arg("--json")
            .arg("--color")
            .arg("never")
            .current_dir(&self.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        if options.fixed_strings {
            command.arg("--fixed-strings");
        }
        if !options.case_sensitive {
            command.arg("--ignore-case");
        }
        if let Some(glob) = options.glob.as_deref() {
            command.arg("--glob").arg(glob);
        }
        command.arg("--").arg(&options.query).arg(".");

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(SearchError::RipgrepMissing);
            }
            Err(err) => {
                return Err(SearchError::Other(AgenkitError::config(format!(
                    "spawn `{}`: {err}",
                    self.rg_binary
                ))));
            }
        };
        let stdout = child.stdout.take().ok_or_else(|| {
            SearchError::Other(AgenkitError::internal("rg stdout was not captured"))
        })?;
        let mut hits = Vec::new();
        let mut truncated = false;

        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|err| {
                SearchError::Other(AgenkitError::internal(format!("read rg output: {err}")))
            })?;
            let Some(hit) = parse_rg_json_line(&line).map_err(SearchError::Other)? else {
                continue;
            };
            if !is_secret_search_path(&hit.path) {
                hits.push(hit);
            }
            if hits.len() >= options.max_results {
                truncated = true;
                let _ = child.kill();
                break;
            }
        }

        let status = child.wait().map_err(|err| {
            SearchError::Other(AgenkitError::internal(format!("wait for rg: {err}")))
        })?;
        if !truncated && !status.success() {
            match status.code() {
                Some(1) => {}
                Some(code) => {
                    return Err(SearchError::Other(AgenkitError::internal(format!(
                        "rg failed with status {code}"
                    ))));
                }
                None => {
                    return Err(SearchError::Other(AgenkitError::internal(
                        "rg terminated by signal",
                    )));
                }
            }
        }

        Ok(FsSearchOutput {
            hits,
            truncated,
            engine: FsSearchEngine::Ripgrep,
        })
    }

    fn run_with_rust_fallback(&self, options: &SearchOptions) -> AgenkitResult<FsSearchOutput> {
        let mut hits = Vec::new();
        let mut truncated = false;
        let mut matcher_builder = RegexMatcherBuilder::new();
        matcher_builder
            .case_insensitive(!options.case_sensitive)
            .fixed_strings(options.fixed_strings);
        let matcher = matcher_builder
            .build(&options.query)
            .map_err(|err| AgenkitError::validation(format!("fs.search pattern: {err}")))?;

        let mut walker = WalkBuilder::new(&self.root);
        walker.standard_filters(true);
        if let Some(glob) = options.glob.as_deref() {
            let mut overrides = OverrideBuilder::new(&self.root);
            overrides
                .add(glob)
                .map_err(|err| AgenkitError::validation(format!("fs.search glob: {err}")))?;
            walker.overrides(
                overrides
                    .build()
                    .map_err(|err| AgenkitError::validation(format!("fs.search glob: {err}")))?,
            );
        }

        let mut searcher_builder = SearcherBuilder::new();
        searcher_builder.line_number(true);
        let mut searcher = searcher_builder.build();

        for entry in walker.build() {
            let entry =
                entry.map_err(|err| AgenkitError::internal(format!("walk repository: {err}")))?;
            if !is_regular_file(&entry) {
                continue;
            }
            let path = entry.path();
            let display_path = relative_display_path(&self.root, path);
            if is_secret_search_path(&display_path) {
                continue;
            }
            let column_matcher = matcher.clone();
            searcher
                .search_path(
                    &matcher,
                    path,
                    Lossy(|line_number, line| {
                        let column = column_matcher
                            .find(line.as_bytes())
                            .map_err(|_| io::Error::other("matcher failed"))?
                            .map(|m| m.start() + 1)
                            .unwrap_or(1);
                        hits.push(FsSearchHit {
                            path: display_path.clone(),
                            line: line_number as usize,
                            column,
                            text: line.trim_end_matches(['\r', '\n']).to_string(),
                        });
                        if hits.len() >= options.max_results {
                            truncated = true;
                            Ok(false)
                        } else {
                            Ok(true)
                        }
                    }),
                )
                .map_err(|err| {
                    AgenkitError::internal(format!("search `{}`: {err}", path.display()))
                })?;
            if truncated {
                break;
            }
        }

        Ok(FsSearchOutput {
            hits,
            truncated,
            engine: FsSearchEngine::RustFallback,
        })
    }
}

impl AiTool for FsSearchTool {
    const ID: &'static str = FS_SEARCH_TOOL_ID;
    type Input = FsSearchInput;
    type Output = FsSearchOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            FS_SEARCH_TOOL_ID,
            "Search repository text using ripgrep. Use this before reading files when you need to locate relevant code or docs.",
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
pub struct FsSearchInput {
    pub query: String,
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default)]
    pub max_results: Option<usize>,
    #[serde(default)]
    pub fixed_strings: Option<bool>,
    #[serde(default)]
    pub case_sensitive: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsSearchOutput {
    pub hits: Vec<FsSearchHit>,
    pub truncated: bool,
    pub engine: FsSearchEngine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FsSearchEngine {
    Ripgrep,
    RustFallback,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FsSearchHit {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub text: String,
}

#[derive(Clone, Debug)]
struct SearchOptions {
    query: String,
    glob: Option<String>,
    max_results: usize,
    fixed_strings: bool,
    case_sensitive: bool,
}

impl SearchOptions {
    fn from_input(input: FsSearchInput) -> AgenkitResult<Self> {
        let query = input.query.trim().to_string();
        if query.is_empty() {
            return Err(AgenkitError::validation(
                "fs.search query must not be empty",
            ));
        }
        Ok(Self {
            query,
            glob: input.glob.filter(|glob| !glob.trim().is_empty()),
            max_results: clamp_limit(input.max_results, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT),
            fixed_strings: input.fixed_strings.unwrap_or(true),
            case_sensitive: input.case_sensitive.unwrap_or(true),
        })
    }
}

enum SearchError {
    RipgrepMissing,
    Other(AgenkitError),
}

fn parse_rg_json_line(line: &str) -> AgenkitResult<Option<FsSearchHit>> {
    let value: Value = serde_json::from_str(line)
        .map_err(|err| AgenkitError::internal(format!("parse rg json: {err}")))?;
    if value.get("type").and_then(Value::as_str) != Some("match") {
        return Ok(None);
    }
    let data = value
        .get("data")
        .ok_or_else(|| AgenkitError::internal("rg match event missing data"))?;
    let path = data
        .get("path")
        .and_then(|path| path.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| AgenkitError::internal("rg match event missing text path"))?;
    let line_number = data
        .get("line_number")
        .and_then(Value::as_u64)
        .ok_or_else(|| AgenkitError::internal("rg match event missing line number"))?;
    let line_text = data
        .get("lines")
        .and_then(|lines| lines.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| AgenkitError::internal("rg match event missing line text"))?;
    let column = data
        .get("submatches")
        .and_then(Value::as_array)
        .and_then(|submatches| submatches.first())
        .and_then(|submatch| submatch.get("start"))
        .and_then(Value::as_u64)
        .map(|start| start as usize + 1)
        .unwrap_or(1);
    Ok(Some(FsSearchHit {
        path: normalize_slashes(path),
        line: line_number as usize,
        column,
        text: line_text.trim_end_matches(['\r', '\n']).to_string(),
    }))
}

fn is_secret_search_path(path: &str) -> bool {
    let normalized = normalize_slashes(path);
    let relative = normalized.strip_prefix("./").unwrap_or(&normalized);
    reject_secret_path(Path::new(relative), FS_SEARCH_TOOL_ID).is_err()
}

fn is_regular_file(entry: &DirEntry) -> bool {
    entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn fs_search_finds_text_with_rg() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lib.rs"), "fn target_symbol() {}\n").unwrap();
        let tool = FsSearchTool::new(dir.path()).unwrap();

        let output = tool
            .run(FsSearchInput {
                query: "target_symbol".to_string(),
                glob: Some("*.rs".to_string()),
                max_results: Some(5),
                fixed_strings: Some(true),
                case_sensitive: Some(true),
            })
            .unwrap();

        assert_eq!(output.hits.len(), 1);
        assert_eq!(output.hits[0].path, "./lib.rs");
        assert_eq!(output.hits[0].line, 1);
    }

    #[test]
    fn fs_search_filters_secret_hits_from_rg() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "TOKEN=secret\n").unwrap();
        let tool = FsSearchTool::new(dir.path()).unwrap();

        let output = tool
            .run(FsSearchInput {
                query: "TOKEN".to_string(),
                glob: Some(".env*".to_string()),
                max_results: Some(5),
                fixed_strings: Some(true),
                case_sensitive: Some(true),
            })
            .unwrap();

        assert!(output.hits.is_empty());
    }

    #[test]
    fn fs_search_reports_paths_with_colons_from_rg() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("with:colon.txt"), "target_symbol\n").unwrap();
        let tool = FsSearchTool::new(dir.path()).unwrap();

        let output = tool
            .run(FsSearchInput {
                query: "target_symbol".to_string(),
                glob: Some("*.txt".to_string()),
                max_results: Some(5),
                fixed_strings: Some(true),
                case_sensitive: Some(true),
            })
            .unwrap();

        assert_eq!(output.hits.len(), 1);
        assert_eq!(output.hits[0].path, "./with:colon.txt");
    }

    #[test]
    fn fs_search_falls_back_when_rg_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lib.rs"), "fn fallback_symbol() {}\n").unwrap();
        let tool = FsSearchTool::new(dir.path())
            .unwrap()
            .with_rg_binary("agenkitty-rg-missing");

        let output = tool
            .run(FsSearchInput {
                query: "fallback_symbol".to_string(),
                glob: Some("*.rs".to_string()),
                max_results: Some(5),
                fixed_strings: Some(true),
                case_sensitive: Some(true),
            })
            .unwrap();

        assert_eq!(output.engine, FsSearchEngine::RustFallback);
        assert_eq!(output.hits.len(), 1);
        assert_eq!(output.hits[0].path, "./lib.rs");
    }

    #[test]
    fn fs_search_filters_secret_hits_from_fallback() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".envrc"), "TOKEN=secret\n").unwrap();
        let tool = FsSearchTool::new(dir.path())
            .unwrap()
            .with_rg_binary("agenkitty-rg-missing");

        let output = tool
            .run(FsSearchInput {
                query: "TOKEN".to_string(),
                glob: Some(".env*".to_string()),
                max_results: Some(5),
                fixed_strings: Some(true),
                case_sensitive: Some(true),
            })
            .unwrap();

        assert_eq!(output.engine, FsSearchEngine::RustFallback);
        assert!(output.hits.is_empty());
    }

    #[test]
    fn fs_search_regex_fallback_matches_without_rg() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lib.rs"), "fn fallback_symbol() {}\n").unwrap();
        let tool = FsSearchTool::new(dir.path())
            .unwrap()
            .with_rg_binary("agenkitty-rg-missing");

        let output = tool
            .run(FsSearchInput {
                query: "fallback_.*".to_string(),
                glob: Some("*.rs".to_string()),
                max_results: Some(5),
                fixed_strings: Some(false),
                case_sensitive: Some(true),
            })
            .unwrap();

        assert_eq!(output.engine, FsSearchEngine::RustFallback);
        assert_eq!(output.hits.len(), 1);
        assert_eq!(output.hits[0].path, "./lib.rs");
        assert_eq!(output.hits[0].column, 4);
    }

    #[test]
    fn fs_search_invalid_regex_fallback_reports_validation() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lib.rs"), "fn fallback_symbol() {}\n").unwrap();
        let tool = FsSearchTool::new(dir.path())
            .unwrap()
            .with_rg_binary("agenkitty-rg-missing");

        let err = tool
            .run(FsSearchInput {
                query: "[".to_string(),
                glob: Some("*.rs".to_string()),
                max_results: Some(5),
                fixed_strings: Some(false),
                case_sensitive: Some(true),
            })
            .unwrap_err();

        assert_eq!(err.kind(), "validation");
        assert!(err.to_string().contains("fs.search pattern"));
    }
}
