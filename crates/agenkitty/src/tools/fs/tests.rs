use std::fs;

use super::*;

#[test]
fn default_read_only_tools_are_fs_namespace() {
    assert_eq!(
        default_read_only_tool_ids(),
        vec!["fs.search", "fs.list", "fs.read", "fs.stat", "fs.exists"]
    );
    assert!(resolve_tool_ids(&["none".to_string()]).unwrap().is_empty());
    assert_eq!(
        resolve_tool_ids(&["fs.list,fs.read".to_string()]).unwrap(),
        vec!["fs.list", "fs.read"]
    );
    assert!(resolve_tool_ids(&["repo.read".to_string()]).is_err());
}

#[test]
fn fs_list_returns_sorted_visible_entries() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("README.md"), "hello").unwrap();
    fs::write(dir.path().join(".env"), "secret").unwrap();
    let tool = FsListTool::new(dir.path()).unwrap();

    let output = tool
        .run(FsListInput {
            path: None,
            max_entries: Some(10),
            include_hidden: None,
        })
        .unwrap();

    assert_eq!(
        output
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect::<Vec<_>>(),
        vec![
            ("src", FsPathKind::Directory),
            ("README.md", FsPathKind::File)
        ]
    );
    assert!(!output.truncated);
}

#[test]
fn fs_list_reports_truncation() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(dir.path().join(name), name).unwrap();
    }
    let tool = FsListTool::new(dir.path()).unwrap();

    let output = tool
        .run(FsListInput {
            path: None,
            max_entries: Some(2),
            include_hidden: None,
        })
        .unwrap();

    assert_eq!(output.entries.len(), 2);
    assert!(output.truncated);
}

#[test]
fn fs_read_rejects_secret_paths() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".env"), "TOKEN=secret\n").unwrap();
    let tool = FsReadTool::new(dir.path()).unwrap();

    let err = tool
        .run(FsReadInput {
            path: ".env".to_string(),
            start_line: None,
            max_lines: None,
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[test]
fn fs_read_rejects_env_prefixed_paths() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".envrc"), "export TOKEN=secret\n").unwrap();
    let tool = FsReadTool::new(dir.path()).unwrap();

    let err = tool
        .run(FsReadInput {
            path: ".envrc".to_string(),
            start_line: None,
            max_lines: None,
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[test]
fn fs_read_rejects_common_credential_files() {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in [
        (".aws/credentials", "aws_secret_access_key=abc\n"),
        (
            ".config/gcloud/application_default_credentials.json",
            "{}\n",
        ),
        (".npmrc", "//registry.npmjs.org/:_authToken=abc\n"),
        (".ssh/id_ed25519", "-----BEGIN OPENSSH PRIVATE KEY-----\n"),
        ("deploy/service-account.json", "{}\n"),
        ("certs/client.pem", "-----BEGIN PRIVATE KEY-----\n"),
    ] {
        let full = dir.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, contents).unwrap();
    }
    let tool = FsReadTool::new(dir.path()).unwrap();

    for path in [
        ".aws/credentials",
        ".config/gcloud/application_default_credentials.json",
        ".npmrc",
        ".ssh/id_ed25519",
        "deploy/service-account.json",
        "certs/client.pem",
    ] {
        let err = tool
            .run(FsReadInput {
                path: path.to_string(),
                start_line: None,
                max_lines: None,
            })
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy", "{path} should be denied");
    }
}

#[cfg(unix)]
#[test]
fn fs_read_rejects_symlink_to_secret_inside_root() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".env"), "TOKEN=secret\n").unwrap();
    symlink(dir.path().join(".env"), dir.path().join("note.txt")).unwrap();
    let tool = FsReadTool::new(dir.path()).unwrap();

    let err = tool
        .run(FsReadInput {
            path: "note.txt".to_string(),
            start_line: None,
            max_lines: None,
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[cfg(unix)]
#[test]
fn fs_exists_rejects_symlink_to_secret_inside_root() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".env"), "TOKEN=secret\n").unwrap();
    symlink(dir.path().join(".env"), dir.path().join("note.txt")).unwrap();
    let tool = FsExistsTool::new(dir.path()).unwrap();

    let err = tool
        .run(FsExistsInput {
            paths: vec!["note.txt".to_string()],
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[test]
fn fs_stat_returns_metadata_without_contents() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "hello").unwrap();
    let tool = FsStatTool::new(dir.path()).unwrap();

    let output = tool
        .run(FsStatInput {
            path: "note.txt".to_string(),
        })
        .unwrap();

    assert_eq!(output.kind, FsPathKind::File);
    assert_eq!(output.size_bytes, Some(5));
}

#[test]
fn fs_exists_reports_each_path() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("present.txt"), "hello").unwrap();
    let tool = FsExistsTool::new(dir.path()).unwrap();

    let output = tool
        .run(FsExistsInput {
            paths: vec!["present.txt".to_string(), "missing.txt".to_string()],
        })
        .unwrap();

    assert_eq!(
        output
            .paths
            .iter()
            .map(|entry| (entry.path.as_str(), entry.exists))
            .collect::<Vec<_>>(),
        vec![("present.txt", true), ("missing.txt", false)]
    );
}

#[test]
fn fs_read_rejects_binary_as_validation() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("image.bin"), [0xff, 0xfe, 0xfd]).unwrap();
    let tool = FsReadTool::new(dir.path()).unwrap();

    let err = tool
        .run(FsReadInput {
            path: "image.bin".to_string(),
            start_line: None,
            max_lines: None,
        })
        .unwrap_err();

    assert_eq!(err.kind(), "validation");
}

#[test]
fn fs_write_append_and_mkdir_mutate_inside_root() {
    let dir = tempfile::tempdir().unwrap();
    FsMkdirTool::new(dir.path())
        .unwrap()
        .run(FsMkdirInput {
            path: "notes".to_string(),
        })
        .unwrap();
    FsWriteTool::new(dir.path())
        .unwrap()
        .run(FsWriteInput {
            path: "notes/todo.txt".to_string(),
            content: "one\n".to_string(),
        })
        .unwrap();
    FsAppendTool::new(dir.path())
        .unwrap()
        .run(FsAppendInput {
            path: "notes/todo.txt".to_string(),
            content: "two\n".to_string(),
        })
        .unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("notes/todo.txt")).unwrap(),
        "one\ntwo\n"
    );
}

#[test]
fn fs_mkdir_creates_nested_directories() {
    let dir = tempfile::tempdir().unwrap();
    FsMkdirTool::new(dir.path())
        .unwrap()
        .run(FsMkdirInput {
            path: "a/b/c".to_string(),
        })
        .unwrap();

    assert!(dir.path().join("a/b/c").is_dir());
}

#[test]
fn fs_copy_move_and_remove_mutate_inside_root() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("source.txt"), "hello").unwrap();

    FsCopyTool::new(dir.path())
        .unwrap()
        .run(FsCopyInput {
            source: "source.txt".to_string(),
            destination: "copy.txt".to_string(),
        })
        .unwrap();
    FsMoveTool::new(dir.path())
        .unwrap()
        .run(FsMoveInput {
            source: "copy.txt".to_string(),
            destination: "moved.txt".to_string(),
        })
        .unwrap();
    FsRemoveTool::new(dir.path())
        .unwrap()
        .run(FsRemoveInput {
            path: "moved.txt".to_string(),
        })
        .unwrap();

    assert!(dir.path().join("source.txt").exists());
    assert!(!dir.path().join("copy.txt").exists());
    assert!(!dir.path().join("moved.txt").exists());
}

#[test]
fn fs_mutating_descriptors_are_side_effecting() {
    use pocopine_agenkit::server::AiTool;
    use pocopine_agenkit_core::ToolSideEffectPolicy;

    let descriptors = [
        FsWriteTool::descriptor(),
        FsAppendTool::descriptor(),
        FsMkdirTool::descriptor(),
        FsCopyTool::descriptor(),
        FsMoveTool::descriptor(),
        FsRemoveTool::descriptor(),
    ];

    assert!(
        descriptors
            .iter()
            .all(|descriptor| descriptor.side_effect == ToolSideEffectPolicy::SideEffecting)
    );
}

#[test]
fn fs_write_rejects_secret_paths() {
    let dir = tempfile::tempdir().unwrap();
    let err = FsWriteTool::new(dir.path())
        .unwrap()
        .run(FsWriteInput {
            path: ".env.local".to_string(),
            content: "TOKEN=secret\n".to_string(),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[cfg(unix)]
#[test]
fn fs_remove_removes_symlink_itself() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), "keep").unwrap();
    symlink(dir.path().join("target.txt"), dir.path().join("link.txt")).unwrap();

    FsRemoveTool::new(dir.path())
        .unwrap()
        .run(FsRemoveInput {
            path: "link.txt".to_string(),
        })
        .unwrap();

    assert!(dir.path().join("target.txt").exists());
    assert!(!dir.path().join("link.txt").exists());
}

#[cfg(unix)]
#[test]
fn fs_move_moves_symlink_itself() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), "keep").unwrap();
    symlink(dir.path().join("target.txt"), dir.path().join("link.txt")).unwrap();

    FsMoveTool::new(dir.path())
        .unwrap()
        .run(FsMoveInput {
            source: "link.txt".to_string(),
            destination: "moved-link.txt".to_string(),
        })
        .unwrap();

    assert!(dir.path().join("target.txt").exists());
    assert!(!dir.path().join("link.txt").exists());
    assert!(
        fs::symlink_metadata(dir.path().join("moved-link.txt"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn fs_read_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "outside").unwrap();
    symlink(
        outside.path().join("secret.txt"),
        dir.path().join("link.txt"),
    )
    .unwrap();
    let tool = FsReadTool::new(dir.path()).unwrap();

    let err = tool
        .run(FsReadInput {
            path: "link.txt".to_string(),
            start_line: None,
            max_lines: None,
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[cfg(unix)]
#[test]
fn fs_write_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "outside").unwrap();
    symlink(
        outside.path().join("secret.txt"),
        dir.path().join("link.txt"),
    )
    .unwrap();

    let err = FsWriteTool::new(dir.path())
        .unwrap()
        .run(FsWriteInput {
            path: "link.txt".to_string(),
            content: "inside".to_string(),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
}

#[cfg(unix)]
#[test]
fn fs_write_rejects_broken_symlink_escape() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_target = outside.path().join("created.txt");
    symlink(&outside_target, dir.path().join("link.txt")).unwrap();

    let err = FsWriteTool::new(dir.path())
        .unwrap()
        .run(FsWriteInput {
            path: "link.txt".to_string(),
            content: "inside".to_string(),
        })
        .unwrap_err();

    assert_eq!(err.kind(), "tool_policy");
    assert!(!outside_target.exists());
}
