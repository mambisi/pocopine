//! `pocopine skills` — fetch + refresh the *living* pocopine-skills agent
//! guides into `.claude/skills/`.
//!
//! The guides evolve in their own repo, independent of the framework and CLI
//! releases. `install` vendors the current set, `update` refetches the latest
//! (reporting what changed), and `check` reports — cheaply, via
//! `git ls-remote`, with no clone — whether a newer revision is available so a
//! user (or an editor/CI hook) can auto-fetch when there's something new.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::args::{SkillsArgs, SkillsCmd};

const SKILLS_REPO: &str = "https://github.com/mambisi/pocopine-skills.git";
const SKILLS_DIR: &str = ".claude/skills";
/// Written under `.claude/skills/` at install/update time so `check` can
/// compare the installed revision against the remote HEAD.
const REV_FILE: &str = ".claude/skills/.pocopine-skills-rev";

pub fn run(args: &SkillsArgs) -> Result<()> {
    match args.cmd {
        SkillsCmd::Install => install(&args.path, false),
        SkillsCmd::Update => install(&args.path, true),
        SkillsCmd::Check => check(&args.path),
        SkillsCmd::List => list(&args.path),
    }
}

/// Vendor the skills into `<root>/.claude/skills`.
///
/// `update == false` (install): errors if skills are already present, so a
/// first-time install never silently clobbers local edits.
/// `update == true`: refetches over any existing install and reports the diff.
pub fn install(root: &Path, update: bool) -> Result<()> {
    let dest = root.join(SKILLS_DIR);
    let before = installed_skills(&dest);

    if has_skills(&dest) && !update {
        bail!(
            "{} already has skills — run `pocopine skills update` to refresh",
            dest.display()
        );
    }

    // Shallow-clone into a temp dir beside dest, then swap it in.
    let tmp = root.join(".claude/.skills-fetch");
    let _ = fs::remove_dir_all(&tmp);
    if let Some(parent) = tmp.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let status = Command::new("git")
        .args(["clone", "--depth", "1", SKILLS_REPO])
        .arg(&tmp)
        .status()
        .context("run `git clone` for skills (is git installed?)")?;
    if !status.success() {
        let _ = fs::remove_dir_all(&tmp);
        bail!("could not clone skills from {SKILLS_REPO} (needs network + access)");
    }

    let rev = git_head_rev(&tmp).unwrap_or_default();

    // Strip the clone's own history, then replace dest.
    let _ = fs::remove_dir_all(tmp.join(".git"));
    if dest.exists() {
        fs::remove_dir_all(&dest).with_context(|| format!("clear {}", dest.display()))?;
    }
    fs::rename(&tmp, &dest).with_context(|| format!("install skills into {}", dest.display()))?;

    if !rev.is_empty() {
        let _ = fs::write(root.join(REV_FILE), format!("{rev}\n"));
    }

    report_changes(&before, &installed_skills(&dest), update);
    Ok(())
}

/// Cheap freshness check: compare the stored revision against the remote HEAD
/// with `git ls-remote` (no clone, no working-tree download).
fn check(root: &Path) -> Result<()> {
    let remote = ls_remote_head(SKILLS_REPO)?;
    match read_rev(root) {
        Some(local) if local == remote => {
            println!("✓ skills are up to date ({})", short(&remote));
        }
        Some(local) => {
            println!(
                "↑ skills update available: {} → {}",
                short(&local),
                short(&remote)
            );
            println!("  run: pocopine skills update");
        }
        None => {
            println!("skills are not installed (or the revision is unknown)");
            println!("  run: pocopine skills install");
        }
    }
    Ok(())
}

/// List the skills currently installed in `.claude/skills/`.
fn list(root: &Path) -> Result<()> {
    let dest = root.join(SKILLS_DIR);
    let skills = installed_skills(&dest);
    if skills.is_empty() {
        println!("no skills installed — run `pocopine skills install`");
        return Ok(());
    }
    println!("{} skills in {}:", skills.len(), dest.display());
    for name in &skills {
        println!("  • {name}");
    }
    Ok(())
}

/// Names of installed skills = immediate subdirs that contain a `SKILL.md`.
fn installed_skills(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().join("SKILL.md").is_file()
                && let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
        }
    }
    names.sort();
    names
}

fn has_skills(dir: &Path) -> bool {
    !installed_skills(dir).is_empty()
}

fn report_changes(before: &[String], after: &[String], update: bool) {
    println!("✓ {} skills in {SKILLS_DIR}", after.len());
    if !update {
        return;
    }
    let added: Vec<&String> = after.iter().filter(|s| !before.contains(s)).collect();
    let removed: Vec<&String> = before.iter().filter(|s| !after.contains(s)).collect();
    if added.is_empty() && removed.is_empty() {
        println!("  (no changes — already current)");
        return;
    }
    if !added.is_empty() {
        let names: Vec<_> = added.iter().map(|s| s.as_str()).collect();
        println!("  + new: {}", names.join(", "));
    }
    if !removed.is_empty() {
        let names: Vec<_> = removed.iter().map(|s| s.as_str()).collect();
        println!("  - removed: {}", names.join(", "));
    }
}

fn git_head_rev(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let rev = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!rev.is_empty()).then_some(rev)
}

fn ls_remote_head(repo: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["ls-remote", repo, "HEAD"])
        .output()
        .context("run `git ls-remote` (is git installed?)")?;
    if !out.status.success() {
        bail!("could not reach {repo} (need network + access)");
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .context("empty response from `git ls-remote`")
}

fn read_rev(root: &Path) -> Option<String> {
    let rev = fs::read_to_string(root.join(REV_FILE)).ok()?;
    let rev = rev.trim().to_string();
    (!rev.is_empty()).then_some(rev)
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(8)]
}
