//! `pocopine new <name>` (alias `create`) — scaffold a new project by
//! cloning the starter template repo (degit-style: shallow clone, strip the
//! history + submodule wiring, re-init). The template is intentionally *not*
//! vendored into this binary — it lives in its own repo so it can evolve
//! without a CLI release. Needs network + a reachable starter repo.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::args::NewArgs;
use crate::skills;

const STARTER_REPO: &str = "https://github.com/mambisi/pocopine-starter-template.git";

/// File extensions we string-replace the `pocopine-app` / `pocopine_app`
/// placeholder inside, after cloning.
const RENAME_EXTS: &[&str] = &[
    "toml", "html", "rs", "md", "css", "json", "js", "ts", "poco",
];

pub fn run(args: &NewArgs) -> Result<()> {
    let crate_name = sanitize(&args.name)?;
    let module = crate_name.replace('-', "_"); // wasm-pack output / js module name
    let target = args.path.join(&args.name);

    if target.exists() {
        bail!("{} already exists", target.display());
    }

    // 1. degit: shallow-clone the starter, without its submodules — the skills
    //    are fetched fresh + living via `pocopine skills`, never pinned here.
    let status = Command::new("git")
        .args(["clone", "--depth", "1", STARTER_REPO])
        .arg(&target)
        .status()
        .context("run `git clone` (is git installed?)")?;
    if !status.success() {
        bail!(
            "could not clone the starter template from {STARTER_REPO}\n\
             (needs network access; the repo must be reachable)"
        );
    }

    // 2. strip the clone's history + submodule wiring → a clean, unversioned tree.
    fs::remove_dir_all(target.join(".git")).context("strip cloned .git")?;
    let _ = fs::remove_file(target.join(".gitmodules"));
    let _ = fs::remove_dir_all(target.join(".claude/skills")); // empty gitlink dir
    let _ = fs::remove_file(target.join("LICENSE")); // the new project picks its own

    // 3. rename the `pocopine-app` placeholder throughout the tree.
    rename_placeholder(&target, &crate_name, &module)?;

    // 4. fresh history.
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&target)
        .status();

    // 5. optionally fetch the living skills now.
    if args.skills {
        if let Err(e) = skills::install(&target, false) {
            eprintln!("  ! skipped --skills: {e}");
        }
    }

    println!("✓ created {}", target.display());
    println!();
    println!("  cd {}", args.name);
    println!("  just dev                  # build + serve with live reload");
    println!("  just                      # list all tasks");
    if !args.skills {
        println!("  pocopine skills install   # add the framework's agent guides");
    }
    Ok(())
}

/// Recursively replace the `pocopine-app` / `pocopine_app` placeholder in every
/// text file of the cloned tree.
fn rename_placeholder(root: &Path, crate_name: &str, module: &str) -> Result<()> {
    fn walk(dir: &Path, f: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                walk(&path, f)?;
            } else {
                f(&path)?;
            }
        }
        Ok(())
    }

    walk(root, &mut |path: &Path| {
        let ext_ok = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| RENAME_EXTS.contains(&e));
        if !ext_ok {
            return Ok(());
        }
        let contents =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if contents.contains("pocopine-app") || contents.contains("pocopine_app") {
            let rendered = contents
                .replace("pocopine-app", crate_name)
                .replace("pocopine_app", module);
            fs::write(path, rendered).with_context(|| format!("write {}", path.display()))?;
        }
        Ok(())
    })
}

/// Derive a valid Cargo package name from the project name.
fn sanitize(name: &str) -> Result<String> {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-').to_string();
    if trimmed.is_empty() || !trimmed.starts_with(|c: char| c.is_ascii_alphabetic()) {
        bail!(
            "project name '{name}' is not a valid crate name (start with a letter; use letters, digits, '-' or '_')"
        );
    }
    Ok(trimmed)
}
