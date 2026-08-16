//! `agenkitty skills` subcommand: exit-code matrix and JSON envelope
//! stability (RFC-121 binary execution mode, folded into the framework
//! binary). Drives the real `agenkitty` binary via `CARGO_BIN_EXE`.

#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agenkitty"))
        .arg("skills")
        .args(args)
        .output()
        .expect("binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn write_skill(root: &Path, name: &str, description: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nBody of {name}.\n"),
    )
    .unwrap();
}

#[test]
fn validate_exit_codes() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "fine", "A valid skill. Use for testing.");

    // 0: clean.
    let ok = run(&["validate", root.join("fine").to_str().unwrap()]);
    assert_eq!(ok.status.code(), Some(0), "{}", stdout(&ok));

    // 1: spec violation.
    let bad = root.join("bad");
    fs::create_dir_all(&bad).unwrap();
    fs::write(
        bad.join("SKILL.md"),
        "---\nname: MISMATCH\ndescription: d\n---\n",
    )
    .unwrap();
    let violated = run(&["validate", bad.to_str().unwrap()]);
    assert_eq!(violated.status.code(), Some(1));
    assert!(stdout(&violated).contains("name.charset"));
    assert!(stdout(&violated).contains("name.dir-mismatch"));

    // 2: usage error.
    let usage = run(&["validate"]);
    assert_eq!(usage.status.code(), Some(2));
    let missing = run(&["validate", tmp.path().join("nope").to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn validate_root_covers_all_skills() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "one", "First.");
    write_skill(&root, "two", "Second.");
    let out = run(&["validate", "--root", root.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("2 skills checked, 0 errors"));
}

#[test]
fn list_and_inspect_json_envelopes() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "alpha", "First skill.");
    write_skill(&root, "beta", "Second skill.");

    let list = run(&["list", "--root", root.to_str().unwrap(), "--json"]);
    assert_eq!(list.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_str(&stdout(&list)).expect("valid JSON");
    assert_eq!(value["schema"], "agenkitty-skills/v1");
    assert_eq!(value["command"], "list");
    let names: Vec<&str> = value["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|skill| skill["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha", "beta"], "deterministic name order");

    let inspect = run(&[
        "inspect",
        "alpha",
        "--root",
        root.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(inspect.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_str(&stdout(&inspect)).expect("valid JSON");
    assert_eq!(value["schema"], "agenkitty-skills/v1");
    assert_eq!(value["skill"]["name"], "alpha");
    assert!(
        value["skill"]["raw"].is_object(),
        "inspect carries raw frontmatter"
    );
    assert_eq!(value["skill"]["digest"].as_str().unwrap().len(), 64);

    let missing = run(&["inspect", "gamma", "--root", root.to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(1));
}

#[test]
fn index_renders_the_exact_prompt_block() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "alpha", "First skill. Use early.");

    let out = run(&["index", "--root", root.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    // Header + index: byte-for-byte the block a runtime injects.
    let text = stdout(&out);
    assert!(text.starts_with("## Skills\n"), "{text}");
    assert!(
        text.ends_with("\n- alpha: First skill. Use early.\n"),
        "{text}"
    );

    let tiny = run(&["index", "--root", root.to_str().unwrap(), "--budget", "4"]);
    assert!(stdout(&tiny).contains("1 more skill omitted"));
}
