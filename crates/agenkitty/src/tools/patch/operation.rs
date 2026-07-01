use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tools::fs::common::{
    canonical_existing_path, normalize_slashes, reject_secret_path, validate_relative_path,
};

const BEGIN_MARKER: &str = "*** Begin Patch";
const END_MARKER: &str = "*** End Patch";
const ADD_PREFIX: &str = "*** Add File: ";
const UPDATE_PREFIX: &str = "*** Update File: ";
const DELETE_PREFIX: &str = "*** Delete File: ";
const MOVE_PREFIX: &str = "*** Move to: ";
const MAX_PATCH_BYTES: usize = 256 * 1024;
const MAX_FILES: usize = 50;
const MAX_HUNKS: usize = 200;
const MAX_DIFF_BYTES: usize = 16 * 1024;
const MAX_DIFF_LINES: usize = 400;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct PatchInput {
    pub patch: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct PatchOutput {
    pub applied: bool,
    pub files: Vec<PatchFileSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<PatchDiff>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct PatchDiff {
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct PatchFileSummary {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    pub operation: PatchOperation,
    pub hunks: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_hunks: Vec<PatchHunkSummary>,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct PatchHunkSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_start_line: Option<usize>,
    pub old_line_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_start_line: Option<usize>,
    pub new_line_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PatchOperation {
    Add,
    Update,
    Delete,
    Move,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedChange {
    summary: PatchFileSummary,
    source: Option<PathBuf>,
    destination: Option<PathBuf>,
    new_content: Option<String>,
    diff_lines: Vec<String>,
}

#[derive(Clone, Debug)]
enum ParsedOperation {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<ParsedHunk>,
    },
    Delete {
        path: String,
    },
}

#[derive(Clone, Debug)]
struct ParsedHunk {
    header: Option<HunkHeader>,
    lines: Vec<PatchLine>,
}

#[derive(Clone, Debug)]
struct HunkHeader {
    old_start_line: Option<usize>,
}

#[derive(Clone, Debug)]
enum PatchLine {
    Context(String),
    Add(String),
    Remove(String),
}

pub(crate) fn prepare_patch(
    root: &Path,
    patch: &str,
    tool_id: &str,
) -> AgenkitResult<Vec<PreparedChange>> {
    let parsed = parse_patch(patch, tool_id)?;
    let mut seen_paths = HashSet::new();
    let mut changes = Vec::with_capacity(parsed.len());

    for operation in parsed {
        let change = prepare_operation(root, operation, tool_id)?;
        remember_unique_path(&mut seen_paths, &change.summary.path, tool_id)?;
        if let Some(destination) = &change.summary.destination {
            remember_unique_path(&mut seen_paths, destination, tool_id)?;
        }
        changes.push(change);
    }

    Ok(changes)
}

pub(crate) fn apply_prepared(changes: &[PreparedChange]) -> AgenkitResult<()> {
    let staged = stage_changes(changes)?;
    let mut applied = Vec::new();

    for (index, staged_change) in staged.iter().enumerate() {
        if let Err(err) = commit_staged_change(staged_change) {
            rollback_staged_changes(&staged, &applied);
            cleanup_staged_changes(staged);
            return Err(err);
        }
        applied.push(index);
    }

    cleanup_staged_changes(staged);
    Ok(())
}

struct StagedChange<'a> {
    change: &'a PreparedChange,
    temp: Option<PathBuf>,
    backup: Option<PathBuf>,
}

fn stage_changes(changes: &[PreparedChange]) -> AgenkitResult<Vec<StagedChange<'_>>> {
    let mut staged = Vec::with_capacity(changes.len());
    for (index, change) in changes.iter().enumerate() {
        match stage_change(index, change) {
            Ok(staged_change) => staged.push(staged_change),
            Err(err) => {
                cleanup_staged_changes(staged);
                return Err(err);
            }
        }
    }
    Ok(staged)
}

fn stage_change(index: usize, change: &PreparedChange) -> AgenkitResult<StagedChange<'_>> {
    let backup = match change.summary.operation {
        PatchOperation::Update | PatchOperation::Delete | PatchOperation::Move => {
            let source = change.source.as_ref().ok_or_else(|| {
                AgenkitError::internal("patch source missing for backup operation")
            })?;
            let backup = sibling_stage_path(source, index, "backup");
            copy_to_new(source, &backup).map_err(|err| {
                AgenkitError::internal(format!(
                    "backup `{}` to `{}`: {err}",
                    source.display(),
                    backup.display()
                ))
            })?;
            Some(backup)
        }
        PatchOperation::Add => None,
    };
    let temp = match change.summary.operation {
        PatchOperation::Add | PatchOperation::Update | PatchOperation::Move => {
            let destination = change.destination.as_ref().ok_or_else(|| {
                AgenkitError::internal("patch destination missing for staged write")
            })?;
            let temp = sibling_stage_path(destination, index, "write");
            if let Err(err) =
                write_text_create_new(&temp, change.new_content.as_deref().unwrap_or_default())
            {
                if let Some(backup) = &backup {
                    let _ = fs::remove_file(backup);
                }
                return Err(err);
            }
            Some(temp)
        }
        PatchOperation::Delete => None,
    };
    Ok(StagedChange {
        change,
        temp,
        backup,
    })
}

fn commit_staged_change(staged: &StagedChange<'_>) -> AgenkitResult<()> {
    match staged.change.summary.operation {
        PatchOperation::Add | PatchOperation::Update => {
            let temp = staged
                .temp
                .as_ref()
                .ok_or_else(|| AgenkitError::internal("patch staged write missing temp file"))?;
            let destination = staged.change.destination.as_ref().ok_or_else(|| {
                AgenkitError::internal("patch destination missing for write operation")
            })?;
            fs::rename(temp, destination).map_err(|err| {
                AgenkitError::internal(format!(
                    "replace `{}` with `{}`: {err}",
                    destination.display(),
                    temp.display()
                ))
            })
        }
        PatchOperation::Delete => {
            let source = staged.change.source.as_ref().ok_or_else(|| {
                AgenkitError::internal("patch source missing for delete operation")
            })?;
            fs::remove_file(source).map_err(|err| {
                AgenkitError::internal(format!("delete `{}`: {err}", source.display()))
            })
        }
        PatchOperation::Move => {
            let temp = staged
                .temp
                .as_ref()
                .ok_or_else(|| AgenkitError::internal("patch staged move missing temp file"))?;
            let source =
                staged.change.source.as_ref().ok_or_else(|| {
                    AgenkitError::internal("patch source missing for move operation")
                })?;
            let destination = staged.change.destination.as_ref().ok_or_else(|| {
                AgenkitError::internal("patch destination missing for move operation")
            })?;
            fs::rename(temp, destination).map_err(|err| {
                AgenkitError::internal(format!(
                    "move staged `{}` to `{}`: {err}",
                    temp.display(),
                    destination.display()
                ))
            })?;
            fs::remove_file(source).map_err(|err| {
                AgenkitError::internal(format!("remove moved source `{}`: {err}", source.display()))
            })
        }
    }
}

fn rollback_staged_changes(staged: &[StagedChange<'_>], applied: &[usize]) {
    for index in applied.iter().rev().copied() {
        let staged_change = &staged[index];
        match staged_change.change.summary.operation {
            PatchOperation::Add => {
                if let Some(destination) = &staged_change.change.destination {
                    let _ = fs::remove_file(destination);
                }
            }
            PatchOperation::Update | PatchOperation::Delete => {
                restore_backup(staged_change);
            }
            PatchOperation::Move => {
                if let Some(destination) = &staged_change.change.destination {
                    let _ = fs::remove_file(destination);
                }
                restore_backup(staged_change);
            }
        }
    }
}

fn restore_backup(staged: &StagedChange<'_>) {
    let (Some(backup), Some(source)) = (&staged.backup, &staged.change.source) else {
        return;
    };
    let _ = fs::copy(backup, source);
}

fn cleanup_staged_changes(staged: Vec<StagedChange<'_>>) {
    for staged_change in staged {
        if let Some(temp) = staged_change.temp {
            let _ = fs::remove_file(temp);
        }
        if let Some(backup) = staged_change.backup {
            let _ = fs::remove_file(backup);
        }
    }
}

fn sibling_stage_path(path: &Path, index: usize, kind: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(
        ".agenkitty-patch-{}-{index}-{kind}",
        std::process::id()
    ))
}

/// Copy `source` to a freshly-created `backup`. `create_new` (`O_CREAT|O_EXCL`)
/// fails if the staging path already exists — including a pre-planted **symlink**
/// — so a backup is never written *through* a symlink to a target outside the
/// workspace (the staging paths are predictable, so an untrusted checkout could
/// otherwise pre-create one).
fn copy_to_new(source: &Path, backup: &Path) -> std::io::Result<()> {
    let mut src = fs::File::open(source)?;
    let mut dst = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup)?;
    std::io::copy(&mut src, &mut dst)?;
    Ok(())
}

pub(crate) fn output_for(changes: &[PreparedChange], applied: bool) -> PatchOutput {
    PatchOutput {
        applied,
        files: changes
            .iter()
            .map(|change| change.summary.clone())
            .collect(),
        diff: (!applied).then(|| diff_for(changes)),
    }
}

fn diff_for(changes: &[PreparedChange]) -> PatchDiff {
    let mut builder = DiffBuilder::new(MAX_DIFF_BYTES, MAX_DIFF_LINES);
    for change in changes {
        for line in &change.diff_lines {
            builder.push_line(line);
            if builder.truncated {
                return builder.finish();
            }
        }
    }
    builder.finish()
}

fn diff_for_add(path: &str, lines: &[String]) -> Vec<String> {
    let mut diff = diff_header(None, Some(path));
    diff.push(format!("@@ -0,0 +1,{} @@", lines.len()));
    for line in lines {
        diff.push(format!("+{line}"));
    }
    diff
}

fn diff_for_delete(path: &str, content: &str) -> Vec<String> {
    let lines = content.lines().collect::<Vec<_>>();
    let mut diff = diff_header(Some(path), None);
    diff.push(format!("@@ -1,{} +0,0 @@", lines.len()));
    for line in lines {
        diff.push(format!("-{line}"));
    }
    diff
}

fn diff_for_update(
    path: &str,
    destination: &str,
    hunks: &[ParsedHunk],
    matched_hunks: &[PatchHunkSummary],
) -> Vec<String> {
    let mut diff = diff_header(Some(path), Some(destination));
    for (hunk, summary) in hunks.iter().zip(matched_hunks) {
        diff.push(format!(
            "@@ -{} +{} @@",
            format_hunk_range(summary.old_start_line, summary.old_line_count),
            format_hunk_range(summary.new_start_line, summary.new_line_count)
        ));
        for line in &hunk.lines {
            match line {
                PatchLine::Context(content) => diff.push(format!(" {content}")),
                PatchLine::Add(content) => diff.push(format!("+{content}")),
                PatchLine::Remove(content) => diff.push(format!("-{content}")),
            }
        }
    }
    diff
}

fn diff_for_rename(path: &str, destination: &str) -> Vec<String> {
    vec![
        format!("rename from {path}"),
        format!("rename to {destination}"),
    ]
}

fn diff_header(old_path: Option<&str>, new_path: Option<&str>) -> Vec<String> {
    vec![
        format!("--- {}", diff_label("a", old_path)),
        format!("+++ {}", diff_label("b", new_path)),
    ]
}

fn diff_label(prefix: &str, path: Option<&str>) -> String {
    path.map(|path| format!("{prefix}/{path}"))
        .unwrap_or_else(|| "/dev/null".to_string())
}

fn format_hunk_range(start_line: Option<usize>, line_count: usize) -> String {
    format!("{},{}", start_line.unwrap_or(0), line_count)
}

struct DiffBuilder {
    text: String,
    lines: usize,
    max_bytes: usize,
    max_lines: usize,
    truncated: bool,
}

impl DiffBuilder {
    fn new(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            text: String::new(),
            lines: 0,
            max_bytes,
            max_lines,
            truncated: false,
        }
    }

    fn push_line(&mut self, line: &str) {
        if self.truncated {
            return;
        }
        let next_len = self.text.len() + line.len() + 1;
        if self.lines >= self.max_lines || next_len > self.max_bytes {
            self.truncated = true;
            return;
        }
        self.text.push_str(line);
        self.text.push('\n');
        self.lines += 1;
    }

    fn finish(self) -> PatchDiff {
        PatchDiff {
            text: self.text,
            truncated: self.truncated,
        }
    }
}

fn parse_patch(patch: &str, tool_id: &str) -> AgenkitResult<Vec<ParsedOperation>> {
    if patch.len() > MAX_PATCH_BYTES {
        return Err(AgenkitError::validation(format!(
            "{tool_id} patch is larger than {MAX_PATCH_BYTES} bytes"
        )));
    }

    let normalized = patch.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    if lines.first() != Some(&BEGIN_MARKER) || lines.last() != Some(&END_MARKER) {
        return Err(AgenkitError::validation(format!(
            "{tool_id} patch must start with `{BEGIN_MARKER}` and end with `{END_MARKER}`"
        )));
    }

    let mut operations = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let line = lines[index];
        if let Some(path) = line.strip_prefix(ADD_PREFIX) {
            let (operation, next) = parse_add(path, &lines, index + 1, tool_id)?;
            operations.push(operation);
            index = next;
        } else if let Some(path) = line.strip_prefix(UPDATE_PREFIX) {
            let (operation, next) = parse_update(path, &lines, index + 1, tool_id)?;
            operations.push(operation);
            index = next;
        } else if let Some(path) = line.strip_prefix(DELETE_PREFIX) {
            operations.push(ParsedOperation::Delete {
                path: validate_patch_path(path, tool_id)?,
            });
            index += 1;
        } else {
            return Err(AgenkitError::validation(format!(
                "{tool_id} unexpected patch line `{line}`"
            )));
        }

        if operations.len() > MAX_FILES {
            return Err(AgenkitError::validation(format!(
                "{tool_id} patch touches more than {MAX_FILES} files"
            )));
        }
    }

    if operations.is_empty() {
        return Err(AgenkitError::validation(format!(
            "{tool_id} patch must contain at least one file operation"
        )));
    }
    Ok(operations)
}

fn parse_add(
    path: &str,
    lines: &[&str],
    mut index: usize,
    tool_id: &str,
) -> AgenkitResult<(ParsedOperation, usize)> {
    let path = validate_patch_path(path, tool_id)?;
    let mut added = Vec::new();

    while index < lines.len() && !is_operation_or_end(lines[index]) {
        let line = lines[index];
        let Some(content) = line.strip_prefix('+') else {
            return Err(AgenkitError::validation(format!(
                "{tool_id} add file `{path}` contains a non-add line"
            )));
        };
        added.push(content.to_string());
        index += 1;
    }

    if added.is_empty() {
        return Err(AgenkitError::validation(format!(
            "{tool_id} add file `{path}` must contain at least one line"
        )));
    }

    Ok((ParsedOperation::Add { path, lines: added }, index))
}

fn parse_update(
    path: &str,
    lines: &[&str],
    mut index: usize,
    tool_id: &str,
) -> AgenkitResult<(ParsedOperation, usize)> {
    let path = validate_patch_path(path, tool_id)?;
    let mut move_to = None;
    if index < lines.len()
        && let Some(destination) = lines[index].strip_prefix(MOVE_PREFIX)
    {
        move_to = Some(validate_patch_path(destination, tool_id)?);
        index += 1;
    }

    let mut hunks = Vec::new();
    let mut current = ParsedHunk {
        header: None,
        lines: Vec::new(),
    };
    while index < lines.len() && !is_operation_or_end(lines[index]) {
        let line = lines[index];
        if line == "@@" || line.starts_with("@@ ") {
            if !current.lines.is_empty() {
                hunks.push(current);
            }
            current = ParsedHunk {
                header: parse_hunk_header(line),
                lines: Vec::new(),
            };
            index += 1;
            continue;
        }
        if line == "*** End of File" {
            index += 1;
            continue;
        }
        if line.is_empty() {
            current.lines.push(PatchLine::Context(String::new()));
            index += 1;
            continue;
        }
        let Some(prefix) = line.as_bytes().first().copied() else {
            return Err(AgenkitError::validation(format!(
                "{tool_id} update file `{path}` contains an empty patch line"
            )));
        };
        match prefix {
            b' ' => current
                .lines
                .push(PatchLine::Context(line[1..].to_string())),
            b'+' => current.lines.push(PatchLine::Add(line[1..].to_string())),
            b'-' => current.lines.push(PatchLine::Remove(line[1..].to_string())),
            _ => {
                return Err(AgenkitError::validation(format!(
                    "{tool_id} update file `{path}` contains an invalid line `{line}`"
                )));
            }
        }
        index += 1;
    }
    if !current.lines.is_empty() {
        hunks.push(current);
    }

    if hunks.len() > MAX_HUNKS {
        return Err(AgenkitError::validation(format!(
            "{tool_id} patch contains more than {MAX_HUNKS} hunks"
        )));
    }
    if hunks.is_empty() && move_to.is_none() {
        return Err(AgenkitError::validation(format!(
            "{tool_id} update file `{path}` has no hunks or move target"
        )));
    }

    Ok((
        ParsedOperation::Update {
            path,
            move_to,
            hunks,
        },
        index,
    ))
}

fn parse_hunk_header(line: &str) -> Option<HunkHeader> {
    let body = line
        .strip_prefix("@@")?
        .trim()
        .strip_suffix("@@")
        .unwrap_or_else(|| line.strip_prefix("@@").unwrap_or_default())
        .trim();
    let mut old_start_line = None;
    for token in body.split_whitespace() {
        if let Some(range) = token.strip_prefix('-') {
            old_start_line = parse_range_start(range);
            break;
        }
    }
    old_start_line.map(|old_start_line| HunkHeader {
        old_start_line: Some(old_start_line),
    })
}

fn parse_range_start(range: &str) -> Option<usize> {
    range.split(',').next()?.parse().ok()
}

fn prepare_operation(
    root: &Path,
    operation: ParsedOperation,
    tool_id: &str,
) -> AgenkitResult<PreparedChange> {
    match operation {
        ParsedOperation::Add { path, lines } => {
            let destination = checked_patch_target_path(root, &path, tool_id)?;
            if destination.exists() {
                return Err(AgenkitError::validation(format!(
                    "{tool_id} add file `{path}` already exists"
                )));
            }
            let additions = lines.len();
            let diff_lines = diff_for_add(&path, &lines);
            Ok(PreparedChange {
                summary: PatchFileSummary {
                    path,
                    destination: None,
                    operation: PatchOperation::Add,
                    hunks: 1,
                    matched_hunks: Vec::new(),
                    additions,
                    deletions: 0,
                },
                source: None,
                destination: Some(destination),
                new_content: Some(lines_to_content(&lines)),
                diff_lines,
            })
        }
        ParsedOperation::Delete { path } => {
            let source = canonical_patch_source_path(root, &path, tool_id)?;
            if !source.is_file() {
                return Err(AgenkitError::validation(format!(
                    "{tool_id} delete path `{path}` is not a file"
                )));
            }
            let content = read_text_file(&source, &path, tool_id)?;
            let diff_lines = diff_for_delete(&path, &content);
            Ok(PreparedChange {
                summary: PatchFileSummary {
                    path,
                    destination: None,
                    operation: PatchOperation::Delete,
                    hunks: 1,
                    matched_hunks: Vec::new(),
                    additions: 0,
                    deletions: count_lines(&content),
                },
                source: Some(source),
                destination: None,
                new_content: None,
                diff_lines,
            })
        }
        ParsedOperation::Update {
            path,
            move_to,
            hunks,
        } => {
            let source = canonical_patch_source_path(root, &path, tool_id)?;
            if !source.is_file() {
                return Err(AgenkitError::validation(format!(
                    "{tool_id} update path `{path}` is not a file"
                )));
            }
            let old_content = read_text_file(&source, &path, tool_id)?;
            let (new_content, matched_hunks) = apply_hunks(&old_content, &hunks, &path, tool_id)?;
            let diff_destination = move_to.clone().unwrap_or_else(|| path.clone());
            let mut diff_lines = diff_for_update(&path, &diff_destination, &hunks, &matched_hunks);
            let (operation, destination) = if let Some(destination_path) = move_to {
                let destination = checked_patch_target_path(root, &destination_path, tool_id)?;
                if destination.exists() {
                    return Err(AgenkitError::validation(format!(
                        "{tool_id} move destination `{destination_path}` already exists"
                    )));
                }
                if hunks.is_empty() {
                    diff_lines = diff_for_rename(&path, &destination_path);
                }
                (PatchOperation::Move, Some((destination_path, destination)))
            } else {
                (PatchOperation::Update, None)
            };
            let additions = hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| matches!(line, PatchLine::Add(_)))
                .count();
            let deletions = hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| matches!(line, PatchLine::Remove(_)))
                .count();
            Ok(PreparedChange {
                summary: PatchFileSummary {
                    path,
                    destination: destination.as_ref().map(|(path, _)| path.clone()),
                    operation,
                    hunks: hunks.len().max(1),
                    matched_hunks,
                    additions,
                    deletions,
                },
                source: Some(source.clone()),
                destination: Some(destination.map(|(_, path)| path).unwrap_or(source)),
                new_content: Some(new_content),
                diff_lines,
            })
        }
    }
}

fn apply_hunks(
    old_content: &str,
    hunks: &[ParsedHunk],
    path: &str,
    tool_id: &str,
) -> AgenkitResult<(String, Vec<PatchHunkSummary>)> {
    let source_lines = split_lines_preserve(old_content);
    let line_ending = preferred_line_ending(old_content);
    let mut output = Vec::new();
    let mut matched_hunks = Vec::with_capacity(hunks.len());
    let mut cursor = 0;

    for hunk in hunks {
        let pattern = hunk_pattern(hunk);
        if pattern.is_empty() {
            return Err(AgenkitError::validation(format!(
                "{tool_id} update hunk for `{path}` must include context or removed lines"
            )));
        }
        let start = find_hunk_start(&source_lines, cursor, &pattern, hunk.header.as_ref())
            .map_err(|reason| {
                AgenkitError::validation(format!(
                    "{tool_id} {reason} while applying patch to `{path}`; missing context: {}",
                    format_missing_context(&pattern)
                ))
            })?;
        output.extend(source_lines[cursor..start].iter().cloned());
        let new_start = output.len() + 1;
        let mut input_index = start;
        let mut new_line_count = 0;
        for line in &hunk.lines {
            match line {
                PatchLine::Context(_) => {
                    output.push(source_lines[input_index].clone());
                    input_index += 1;
                    new_line_count += 1;
                }
                PatchLine::Remove(_) => {
                    input_index += 1;
                }
                PatchLine::Add(content) => {
                    ensure_output_ends_with_newline(&mut output, line_ending);
                    output.push(format!("{content}{line_ending}"));
                    new_line_count += 1;
                }
            }
        }
        matched_hunks.push(PatchHunkSummary {
            old_start_line: (!pattern.is_empty()).then_some(start + 1),
            old_line_count: pattern.len(),
            new_start_line: Some(new_start),
            new_line_count,
        });
        cursor = start + pattern.len();
    }

    output.extend(source_lines[cursor..].iter().cloned());
    Ok((output.concat(), matched_hunks))
}

fn hunk_pattern(hunk: &ParsedHunk) -> Vec<&str> {
    hunk.lines
        .iter()
        .filter_map(|line| match line {
            PatchLine::Context(content) | PatchLine::Remove(content) => Some(content.as_str()),
            PatchLine::Add(_) => None,
        })
        .collect()
}

fn find_hunk_start(
    source: &[String],
    cursor: usize,
    pattern: &[&str],
    header: Option<&HunkHeader>,
) -> Result<usize, &'static str> {
    if let Some(old_start_line) = header.and_then(|header| header.old_start_line) {
        let start = old_start_line.saturating_sub(1);
        if start < cursor || !matches_at(source, start, pattern) {
            return Err("stale context");
        }
        return Ok(start);
    }

    let matches = find_subsequence_matches(source, cursor, pattern);
    match matches.as_slice() {
        [start] => Ok(*start),
        [] => Err("stale context"),
        _ => Err("ambiguous context"),
    }
}

fn find_subsequence_matches(source: &[String], cursor: usize, pattern: &[&str]) -> Vec<usize> {
    if pattern.is_empty() {
        return vec![cursor];
    }
    if pattern.len() > source.len().saturating_sub(cursor) {
        return Vec::new();
    }
    (cursor..=source.len() - pattern.len())
        .filter(|&start| matches_at(source, start, pattern))
        .collect()
}

fn matches_at(source: &[String], start: usize, pattern: &[&str]) -> bool {
    if start + pattern.len() > source.len() {
        return false;
    }
    pattern
        .iter()
        .enumerate()
        .all(|(offset, expected)| line_body(&source[start + offset]) == *expected)
}

fn format_missing_context(pattern: &[&str]) -> String {
    if pattern.is_empty() {
        return "<empty>".to_string();
    }

    let mut lines = pattern
        .iter()
        .take(5)
        .map(|line| format!("`{}`", truncate_diagnostic_line(line, 80)))
        .collect::<Vec<_>>();
    if pattern.len() > 5 {
        lines.push("...".to_string());
    }
    lines.join(" | ")
}

fn truncate_diagnostic_line(line: &str, max_chars: usize) -> String {
    let mut chars = line.chars();
    let mut output = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return output;
        };
        output.push(ch);
    }
    if chars.next().is_some() {
        output.push_str("...");
    }
    output
}

fn split_lines_preserve(content: &str) -> Vec<String> {
    content.split_inclusive('\n').map(str::to_string).collect()
}

fn line_body(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn preferred_line_ending(content: &str) -> &'static str {
    if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn ensure_output_ends_with_newline(output: &mut [String], line_ending: &str) {
    let Some(last) = output.last_mut() else {
        return;
    };
    if !last.ends_with('\n') {
        last.push_str(line_ending);
    }
}

fn lines_to_content(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>()
}

fn count_lines(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.lines().count()
    }
}

fn validate_patch_path(path: &str, tool_id: &str) -> AgenkitResult<String> {
    let relative = validate_relative_path(path, tool_id)?;
    reject_secret_path(relative, tool_id)?;
    let normalized = normalized_relative_path(relative);
    if normalized.is_empty() {
        return Err(AgenkitError::validation(format!(
            "{tool_id} path `{path}` must name a file below the project root"
        )));
    }
    Ok(normalized)
}

fn normalized_relative_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            parts.push(name.to_string_lossy().into_owned());
        }
    }
    parts.join("/")
}

fn checked_patch_target_path(root: &Path, path: &str, tool_id: &str) -> AgenkitResult<PathBuf> {
    let relative = validate_relative_path(path, tool_id)?;
    reject_secret_path(relative, tool_id)?;
    let target = root.join(relative);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AgenkitError::tool_policy(format!(
                "{tool_id} path `{}` is a symlink and cannot be used as a patch target",
                normalize_slashes(path)
            )));
        }
        Ok(_) => {
            let canonical = target.canonicalize().map_err(|err| {
                AgenkitError::not_found(format!("{tool_id} path `{path}`: {err}"))
            })?;
            ensure_inside_root(root, &canonical, path, tool_id)?;
            return Ok(target);
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(AgenkitError::not_found(format!(
                "{tool_id} path `{path}`: {err}"
            )));
        }
    }

    let mut ancestor = target.parent().ok_or_else(|| {
        AgenkitError::validation(format!("{tool_id} path `{path}` has no parent"))
    })?;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AgenkitError::tool_policy(format!(
                    "{tool_id} parent for `{}` is a symlink and cannot be used as a patch target",
                    normalize_slashes(path)
                )));
            }
            Ok(_) => {
                let canonical_ancestor = ancestor.canonicalize().map_err(|err| {
                    AgenkitError::not_found(format!("{tool_id} parent for `{path}`: {err}"))
                })?;
                ensure_inside_root(root, &canonical_ancestor, path, tool_id)?;
                return Ok(target);
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or_else(|| {
                    AgenkitError::not_found(format!(
                        "{tool_id} no existing parent for `{}`",
                        normalize_slashes(path)
                    ))
                })?;
            }
            Err(err) => {
                return Err(AgenkitError::not_found(format!(
                    "{tool_id} parent for `{path}`: {err}"
                )));
            }
        }
    }
}

fn canonical_patch_source_path(root: &Path, path: &str, tool_id: &str) -> AgenkitResult<PathBuf> {
    let relative = validate_relative_path(path, tool_id)?;
    reject_secret_path(relative, tool_id)?;
    let raw_path = root.join(relative);
    let metadata = fs::symlink_metadata(&raw_path)
        .map_err(|err| AgenkitError::not_found(format!("{tool_id} path `{path}`: {err}")))?;
    if metadata.file_type().is_symlink() {
        return Err(AgenkitError::tool_policy(format!(
            "{tool_id} path `{}` is a symlink and cannot be patched",
            normalize_slashes(path)
        )));
    }
    canonical_existing_path(root, path, tool_id)
}

fn ensure_inside_root(
    root: &Path,
    canonical: &Path,
    original: &str,
    tool_id: &str,
) -> AgenkitResult<()> {
    if !canonical.starts_with(root) {
        return Err(AgenkitError::tool_policy(format!(
            "{tool_id} path `{}` escapes the project root",
            normalize_slashes(original)
        )));
    }
    Ok(())
}

fn read_text_file(path: &Path, display_path: &str, tool_id: &str) -> AgenkitResult<String> {
    fs::read_to_string(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::InvalidData {
            AgenkitError::validation(format!(
                "{tool_id} path `{display_path}` is not valid UTF-8 text"
            ))
        } else {
            AgenkitError::not_found(format!("{tool_id} read `{display_path}`: {err}"))
        }
    })
}

fn write_text_create_new(path: &Path, content: &str) -> AgenkitResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AgenkitError::internal(format!("create parent `{}`: {err}", parent.display()))
        })?;
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| AgenkitError::internal(format!("create `{}`: {err}", path.display())))?;
    file.write_all(content.as_bytes())
        .map_err(|err| AgenkitError::internal(format!("write `{}`: {err}", path.display())))
}

fn is_operation_or_end(line: &str) -> bool {
    line == END_MARKER
        || line.starts_with(ADD_PREFIX)
        || line.starts_with(UPDATE_PREFIX)
        || line.starts_with(DELETE_PREFIX)
}

fn remember_unique_path(
    seen_paths: &mut HashSet<String>,
    path: &str,
    tool_id: &str,
) -> AgenkitResult<()> {
    if !seen_paths.insert(path.to_string()) {
        return Err(AgenkitError::validation(format!(
            "{tool_id} patch edits `{path}` more than once"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn copy_to_new_refuses_to_write_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        fs::write(&source, b"new content").unwrap();
        // A precious file outside the workspace, and a pre-planted symlink at the
        // (predictable) backup path pointing at it.
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"PRECIOUS").unwrap();
        let backup = dir.path().join("backup");
        std::os::unix::fs::symlink(&outside, &backup).unwrap();

        let err = copy_to_new(&source, &backup).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        // The symlink target was not written through.
        assert_eq!(fs::read(&outside).unwrap(), b"PRECIOUS");
    }
}
