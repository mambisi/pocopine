use std::fs;

use super::*;
use crate::tools::{default_read_only_tool_ids, resolve_tool_ids};

#[test]
fn patch_preview_rejects_missing_markers() {
    let dir = tempfile::tempdir().unwrap();
    let tool = PatchPreviewTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: "*** Add File: note.txt\n+hello\n".to_string(),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "validation");
}

#[test]
fn patch_preview_rejects_paths_outside_root() {
    let dir = tempfile::tempdir().unwrap();
    let tool = PatchPreviewTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: add_file_patch("../escape.txt", "nope"),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[test]
fn patch_preview_rejects_secret_paths() {
    let dir = tempfile::tempdir().unwrap();
    let tool = PatchPreviewTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: add_file_patch(".env", "TOKEN=secret"),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[test]
fn patch_preview_reports_add_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let tool = PatchPreviewTool::new(dir.path()).unwrap();

    let output = tool
        .run(PatchInput {
            patch: add_file_patch("notes/todo.txt", "one\ntwo"),
        })
        .unwrap();

    assert!(!output.applied);
    assert_eq!(
        output.files,
        vec![PatchFileSummary {
            path: "notes/todo.txt".to_string(),
            destination: None,
            operation: PatchOperation::Add,
            hunks: 1,
            matched_hunks: Vec::new(),
            additions: 2,
            deletions: 0,
        }]
    );
    let diff = output.diff.unwrap();
    assert!(!diff.truncated);
    assert!(diff.text.contains("--- /dev/null\n+++ b/notes/todo.txt\n"));
    assert!(diff.text.contains("@@ -0,0 +1,2 @@\n+one\n+two\n"));
    assert!(!dir.path().join("notes/todo.txt").exists());
}

#[test]
fn patch_apply_adds_file_and_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let output = tool
        .run(PatchInput {
            patch: add_file_patch("notes/todo.txt", "one\ntwo"),
        })
        .unwrap();

    assert!(output.applied);
    assert_eq!(
        fs::read_to_string(dir.path().join("notes/todo.txt")).unwrap(),
        "one\ntwo\n"
    );
}

#[test]
fn patch_preview_reports_matched_hunk_line_numbers() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.rs"), "one\ntwo\nthree\n").unwrap();
    let tool = PatchPreviewTool::new(dir.path()).unwrap();

    let output = tool
        .run(PatchInput {
            patch: update_patch("main.rs", &["@@", " two", "-three", "+four"]),
        })
        .unwrap();

    assert_eq!(
        output.files[0].matched_hunks,
        vec![PatchHunkSummary {
            old_start_line: Some(2),
            old_line_count: 2,
            new_start_line: Some(2),
            new_line_count: 2,
        }]
    );
    assert!(!output.applied);
    let diff = output.diff.unwrap();
    assert!(!diff.truncated);
    assert!(diff.text.contains("--- a/main.rs\n+++ b/main.rs\n"));
    assert!(diff.text.contains("@@ -2,2 +2,2 @@\n two\n-three\n+four\n"));
}

#[test]
fn patch_apply_updates_file_with_exact_context() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {\n    old();\n}\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    tool.run(PatchInput {
        patch: update_patch(
            "main.rs",
            &["@@", " fn main() {", "-    old();", "+    new();", " }"],
        ),
    })
    .unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("main.rs")).unwrap(),
        "fn main() {\n    new();\n}\n"
    );
}

#[test]
fn patch_apply_appends_after_final_context_without_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "a\nb").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    tool.run(PatchInput {
        patch: update_patch("note.txt", &["@@ -2,1 +2,2 @@", " b", "+c"]),
    })
    .unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "a\nb\nc\n"
    );
}

#[test]
fn patch_apply_matches_crlf_source_and_preserves_line_endings_for_adds() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "a\r\nb\r\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    tool.run(PatchInput {
        patch: update_patch("note.txt", &["@@ -2,1 +2,1 @@", "-b", "+c"]),
    })
    .unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "a\r\nc\r\n"
    );
}

#[test]
fn patch_apply_accepts_blank_context_lines() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "a\n\nb\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    tool.run(PatchInput {
        patch: update_patch("note.txt", &["@@", " a", "", "-b", "+c"]),
    })
    .unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "a\n\nc\n"
    );
}

#[test]
fn patch_apply_rejects_add_only_update_hunk() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "a\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: update_patch("note.txt", &["@@", "+b"]),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "validation");
}

#[test]
fn patch_apply_rejects_ambiguous_context_without_line_hint() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "target\nold\n\ntarget\nold\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: update_patch("note.txt", &["@@", " target", "-old", "+new"]),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "validation");
    assert!(err.to_string().contains("ambiguous context"));
}

#[test]
fn patch_apply_uses_hunk_line_hint_for_duplicate_context() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "target\nold\n\ntarget\nold\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    tool.run(PatchInput {
        patch: update_patch("note.txt", &["@@ -4,2 +4,2 @@", " target", "-old", "+new"]),
    })
    .unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "target\nold\n\ntarget\nnew\n"
    );
}

#[test]
fn patch_preview_diff_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let tool = PatchPreviewTool::new(dir.path()).unwrap();
    let mut content = String::new();
    for index in 0..600 {
        if index > 0 {
            content.push('\n');
        }
        content.push_str(&format!(
            "line-{index:03}-abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz"
        ));
    }

    let output = tool
        .run(PatchInput {
            patch: add_file_patch("big.txt", &content),
        })
        .unwrap();

    let diff = output.diff.unwrap();
    assert!(diff.truncated);
    assert!(!diff.text.contains("line-599"));
}

#[test]
fn patch_apply_deletes_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("old.txt"), "remove me\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let output = tool
        .run(PatchInput {
            patch: "*** Begin Patch\n*** Delete File: old.txt\n*** End Patch\n".to_string(),
        })
        .unwrap();

    assert!(output.applied);
    assert!(!dir.path().join("old.txt").exists());
}

#[test]
fn patch_apply_moves_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("old.txt"), "keep me\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let output = tool
        .run(PatchInput {
            patch:
                "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n*** End Patch\n"
                    .to_string(),
        })
        .unwrap();

    assert!(output.applied);
    assert!(!dir.path().join("old.txt").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "keep me\n"
    );
}

#[test]
fn patch_apply_stale_multifile_patch_leaves_files_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("first.txt"), "alpha\n").unwrap();
    fs::write(dir.path().join("second.txt"), "bravo\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: format!(
                "*** Begin Patch\n{}{}*** End Patch\n",
                update_patch_body("first.txt", &["@@", "-alpha", "+changed"]),
                update_patch_body("second.txt", &["@@", "-missing", "+changed"]),
            ),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "validation");
    let message = err.to_string();
    assert!(message.contains("stale context"));
    assert!(message.contains("missing context"));
    assert!(message.contains("`missing`"));
    assert_eq!(
        fs::read_to_string(dir.path().join("first.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("second.txt")).unwrap(),
        "bravo\n"
    );
}

#[test]
fn patch_apply_update_missing_file_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: update_patch("missing.txt", &["@@", "-old", "+new"]),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "not_found");
}

#[test]
fn patch_apply_add_over_existing_file_is_validation_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "exists\n").unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: add_file_patch("note.txt", "new"),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "validation");
}

#[test]
fn patch_apply_rejects_duplicate_normalized_paths() {
    let dir = tempfile::tempdir().unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: format!(
                "*** Begin Patch\n{}{}*** End Patch\n",
                add_file_patch_body("note.txt", "one"),
                add_file_patch_body("./note.txt", "two"),
            ),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "validation");
    assert!(!dir.path().join("note.txt").exists());
}

#[cfg(unix)]
#[test]
fn patch_apply_staging_failure_leaves_prior_files_unchanged() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("first.txt"), "first\n").unwrap();
    fs::create_dir(dir.path().join("locked")).unwrap();
    fs::write(dir.path().join("locked/second.txt"), "second\n").unwrap();
    let mut permissions = fs::metadata(dir.path().join("locked"))
        .unwrap()
        .permissions();
    permissions.set_mode(0o555);
    fs::set_permissions(dir.path().join("locked"), permissions).unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: format!(
                "*** Begin Patch\n{}{}*** End Patch\n",
                update_patch_body("first.txt", &["@@", "-first", "+changed"]),
                update_patch_body("locked/second.txt", &["@@", "-second", "+changed"]),
            ),
        })
        .unwrap_err();

    let mut permissions = fs::metadata(dir.path().join("locked"))
        .unwrap()
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(dir.path().join("locked"), permissions).unwrap();

    assert_eq!(err.kind(), "internal");
    assert_eq!(
        fs::read_to_string(dir.path().join("first.txt")).unwrap(),
        "first\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("locked/second.txt")).unwrap(),
        "second\n"
    );
}

#[cfg(unix)]
#[test]
fn patch_apply_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "outside\n").unwrap();
    symlink(
        outside.path().join("secret.txt"),
        dir.path().join("link.txt"),
    )
    .unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: update_patch("link.txt", &["@@", "-outside", "+inside"]),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[cfg(unix)]
#[test]
fn patch_apply_rejects_broken_symlink_escape() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_target = outside.path().join("created.txt");
    symlink(&outside_target, dir.path().join("link.txt")).unwrap();
    let tool = PatchApplyTool::new(dir.path()).unwrap();

    let err = tool
        .run(PatchInput {
            patch: add_file_patch("link.txt", "inside"),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
    assert!(!outside_target.exists());
}

#[test]
fn patch_tool_id_resolution_accepts_patch_ids() {
    assert!(resolve_tool_ids(&["patch.preview".to_string()]).is_ok());
    assert!(resolve_tool_ids(&["patch.apply".to_string()]).is_ok());
    assert!(resolve_tool_ids(&["patch.nope".to_string()]).is_err());
    assert!(!default_read_only_tool_ids().contains(&"patch.apply".to_string()));
}

fn add_file_patch(path: &str, content: &str) -> String {
    let mut patch = format!("*** Begin Patch\n{}", add_file_patch_body(path, content));
    patch.push_str("*** End Patch\n");
    patch
}

fn add_file_patch_body(path: &str, content: &str) -> String {
    let mut patch = format!("*** Add File: {path}\n");
    for line in content.split('\n') {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }
    patch
}

fn update_patch(path: &str, lines: &[&str]) -> String {
    format!(
        "*** Begin Patch\n{}*** End Patch\n",
        update_patch_body(path, lines)
    )
}

fn update_patch_body(path: &str, lines: &[&str]) -> String {
    let mut patch = format!("*** Update File: {path}\n");
    for line in lines {
        patch.push_str(line);
        patch.push('\n');
    }
    patch
}
