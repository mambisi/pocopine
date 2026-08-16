//! Conformance and confinement tests for the loader (RFC-121 test plan):
//! discovery + shadowing across ordered roots, progressive-disclosure reads,
//! symlink-escape defense, secret-path refusal, index budgeting, and the
//! diagnostics contract.

#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::Path;

use agenkitty_skills::{ReadOpts, Severity, SkillLimits, SkillLoader};
use tempfile::TempDir;

fn write_skill(root: &Path, name: &str, frontmatter_extra: &str, body: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Description for {name}. Use when testing.\n{frontmatter_extra}---\n{body}"),
    )
    .unwrap();
}

fn loader(roots: &[&Path]) -> SkillLoader {
    SkillLoader::new(roots.iter().map(|root| root.to_path_buf()).collect())
}

#[test]
fn discovers_valid_skills_and_excludes_invalid_ones() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "good-skill", "", "Do the thing.\n");
    // Directory/name mismatch → excluded with an error diagnostic.
    let bad = root.join("bad-dir");
    fs::create_dir_all(&bad).unwrap();
    fs::write(
        bad.join("SKILL.md"),
        "---\nname: other-name\ndescription: d\n---\n",
    )
    .unwrap();

    let catalog = loader(&[&root]).discover();
    assert_eq!(catalog.len(), 1);
    assert!(catalog.get("good-skill").is_some());
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|d| d.severity == Severity::Error && d.rule == "name.dir-mismatch")
    );
}

#[test]
fn earlier_root_wins_and_shadowed_is_kept() {
    let tmp = TempDir::new().unwrap();
    let primary = tmp.path().join("primary");
    let compat = tmp.path().join("compat");
    write_skill(&primary, "deploy", "", "primary body\n");
    write_skill(&compat, "deploy", "", "compat body\n");
    write_skill(&compat, "extra", "", "extra body\n");

    let catalog = loader(&[&primary, &compat]).discover();
    assert_eq!(catalog.len(), 2);
    let winner = catalog.get("deploy").unwrap();
    assert!(
        winner
            .root
            .starts_with(tmp.path().join("primary").canonicalize().unwrap())
    );
    assert_eq!(catalog.shadowed().len(), 1);
    assert!(catalog.shadowed()[0].shadowed_by.is_some());
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|d| d.rule == "name.shadowed")
    );
    // The winner's body is served.
    let body = catalog.body("deploy", None).unwrap();
    assert!(body.body.contains("primary body"));
}

#[test]
fn body_substitutes_arguments_and_flags_truncation() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(
        &root,
        "greet",
        "arguments: who\n",
        "Say hello to $who.\nAll: $ARGUMENTS\n",
    );

    let catalog = loader(&[&root]).discover();
    let body = catalog.body("greet", Some("world extra")).unwrap();
    assert!(body.body.contains("Say hello to world."));
    assert!(body.body.contains("All: world extra"));
    assert!(!body.truncated);

    let tiny = SkillLimits {
        body_byte_limit: 24,
        ..SkillLimits::default()
    };
    let catalog = loader(&[&root]).with_limits(tiny).discover();
    let body = catalog.body("greet", None).unwrap();
    assert!(body.truncated);
    assert!(body.body.ends_with("…(truncated)…"));
}

#[test]
fn read_resource_pages_and_reports_eof() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "docs", "", "See references/guide.md\n");
    let refs = root.join("docs/references");
    fs::create_dir_all(&refs).unwrap();
    fs::write(refs.join("guide.md"), "0123456789").unwrap();

    let catalog = loader(&[&root]).discover();
    let skill = catalog.get("docs").unwrap();
    assert_eq!(skill.resources(), ["references/guide.md"]);

    let first = catalog
        .read_resource(
            "docs",
            "references/guide.md",
            ReadOpts {
                offset: None,
                limit: Some(4),
            },
        )
        .unwrap();
    assert_eq!(first.content, "0123");
    assert!(!first.eof);
    let rest = catalog
        .read_resource(
            "docs",
            "references/guide.md",
            ReadOpts {
                offset: Some(4),
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(rest.content, "456789");
    assert!(rest.eof);
}

#[cfg(unix)]
#[test]
fn symlink_escape_reads_as_refusal_not_a_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "sneaky", "", "body\n");
    // A file outside the skill root, reachable via a symlink inside it.
    let outside = tmp.path().join("outside.txt");
    fs::write(&outside, "leaked").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("sneaky/link.md")).unwrap();

    let catalog = loader(&[&root]).discover();
    let err = catalog
        .read_resource("sneaky", "link.md", ReadOpts::default())
        .unwrap_err();
    assert_eq!(err.kind(), "tool_policy");
}

#[test]
fn parent_traversal_and_secret_paths_are_refused() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "safe", "", "body\n");
    fs::write(root.join("safe/.env"), "SECRET=x").unwrap();

    let catalog = loader(&[&root]).discover();
    let err = catalog
        .read_resource("safe", "../safe/SKILL.md", ReadOpts::default())
        .unwrap_err();
    assert_eq!(err.kind(), "tool_policy");
    let err = catalog
        .read_resource("safe", ".env", ReadOpts::default())
        .unwrap_err();
    assert_eq!(err.kind(), "tool_policy");
}

#[test]
fn index_respects_budget_with_explicit_omission_line() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    for i in 0..6 {
        write_skill(&root, &format!("skill-{i}"), "", "b\n");
    }
    let catalog = loader(&[&root]).discover();

    let full = catalog.render_index(64 * 1024);
    assert_eq!(full.lines().count(), 6);
    assert!(full.starts_with("- skill-0: "));

    let two_lines = full.lines().take(2).map(|l| l.len() + 1).sum::<usize>();
    let cut = catalog.render_index(two_lines);
    assert_eq!(cut.lines().count(), 3);
    assert!(
        cut.lines()
            .nth(2)
            .unwrap()
            .contains("4 more skills omitted")
    );
}

#[test]
fn disable_model_invocation_hides_from_index_but_body_still_serves() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "visible", "", "b\n");
    write_skill(&root, "manual", "disable-model-invocation: true\n", "b\n");

    let catalog = loader(&[&root]).discover();
    let index = catalog.render_index(64 * 1024);
    assert!(index.contains("- visible:"));
    assert!(!index.contains("- manual:"));
    // Host-invocation path still works; the runtime tool enforces the rule
    // for model-initiated activation.
    assert!(catalog.body("manual", None).is_ok());
    assert!(catalog.get("manual").unwrap().ext.disable_model_invocation);
}

#[test]
fn credential_looking_description_is_redacted_in_index() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    let dir = root.join("leaky");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: leaky\ndescription: \"Use api_key = sk-123456 to authenticate\"\n---\nbody\n",
    )
    .unwrap();

    let catalog = loader(&[&root]).discover();
    let index = catalog.render_index(64 * 1024);
    assert!(index.contains("- leaky: [redacted"));
    assert!(!index.contains("sk-123456"));
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|d| d.rule == "index.redacted" && d.severity == Severity::Warning)
    );
}

#[test]
fn ansi_in_description_is_stripped_from_index() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    let dir = root.join("ansi");
    fs::create_dir_all(&dir).unwrap();
    // YAML double-quoted `\e` escapes to a real ESC character.
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: ansi\ndescription: \"safe\\e[2K\\e[1A poisoned\"\n---\nbody\n",
    )
    .unwrap();

    let catalog = loader(&[&root]).discover();
    let index = catalog.render_index(64 * 1024);
    assert!(index.contains("- ansi: safe poisoned"));
    assert!(!index.contains('\u{1b}'));
}

#[test]
fn missing_roots_are_silently_skipped() {
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    write_skill(&real, "only", "", "b\n");
    let ghost = tmp.path().join("does-not-exist");

    let catalog = loader(&[&ghost, &real]).discover();
    assert_eq!(catalog.len(), 1);
    assert!(catalog.diagnostics().iter().all(|d| d.rule != "root.io"));
}

#[cfg(unix)]
#[test]
fn same_target_via_two_roots_loads_once() {
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    write_skill(&real, "shared", "", "b\n");
    let alias = tmp.path().join("alias");
    fs::create_dir_all(&alias).unwrap();
    std::os::unix::fs::symlink(real.join("shared"), alias.join("shared")).unwrap();

    let catalog = loader(&[&real, &alias]).discover();
    assert_eq!(catalog.len(), 1);
    assert!(catalog.shadowed().is_empty());
    assert!(
        catalog
            .diagnostics()
            .iter()
            .all(|d| d.rule != "name.shadowed")
    );
}

#[test]
fn digests_change_with_content() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("skills");
    write_skill(&root, "evolving", "", "v1\n");
    let first = loader(&[&root]).discover().get("evolving").unwrap().digest;
    write_skill(&root, "evolving", "", "v2\n");
    let second = loader(&[&root]).discover().get("evolving").unwrap().digest;
    assert_ne!(first, second);
}
