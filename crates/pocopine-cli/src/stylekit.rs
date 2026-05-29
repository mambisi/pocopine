//! Pine Stylekit build stage (RFC 092 D2/D6).
//!
//! Compiles utility CSS from a project's `.poco` sources in-process —
//! no external watcher, no shelling out. Opt-in via `--stylekit` or a
//! `[package.metadata.pocopine.stylekit]` block. Fails loud: on any
//! error-severity diagnostic the build aborts and the stale stylesheet
//! is left untouched rather than silently re-served.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pocopine_stylekit::{compile_project, render, CompileOptions, ProjectCss, SourceFile};

use crate::config::{PocopineConfig, StylekitConfig};

/// Whether the Stylekit stage should run: an explicit `--stylekit` flag
/// or a configured `[…pocopine.stylekit]` block.
pub fn enabled(cfg: &PocopineConfig, flag: bool) -> bool {
    flag || cfg.stylekit.is_some()
}

/// Resolve the effective config (defaults when only `--stylekit` is
/// passed with no config block).
fn resolve(cfg: &PocopineConfig) -> StylekitConfig {
    match &cfg.stylekit {
        Some(c) => StylekitConfig {
            input: c.input.clone(),
            output: c.output.clone(),
            src: c.src.clone(),
        },
        None => StylekitConfig::default(),
    }
}

/// Compile the project's CSS, returning the result and the source files
/// (kept for diagnostic rendering). Does not write anything.
fn compile(project: &Path, scfg: &StylekitConfig) -> Result<(ProjectCss, Vec<SourceFile>)> {
    let input = project.join(&scfg.input);
    let theme_css = std::fs::read_to_string(&input)
        .with_context(|| format!("read stylekit input CSS: {}", input.display()))?;

    let src_dir = project.join(&scfg.src);
    let mut files = Vec::new();
    collect_poco(&src_dir, &mut files)
        .with_context(|| format!("scan .poco sources under {}", src_dir.display()))?;
    files.sort_by(|a, b| a.path.cmp(&b.path)); // deterministic file order

    let out = compile_project(&theme_css, &files, CompileOptions::default());
    Ok((out, files))
}

/// One-shot compile for `build` / `run` / dev startup. Renders every
/// diagnostic; aborts the build (without writing) if any are errors.
pub fn run_once(project: &Path, cfg: &PocopineConfig, _release: bool) -> Result<()> {
    let scfg = resolve(cfg);
    let (out, files) = compile(project, &scfg)?;
    report(&out, &files);
    if out.has_errors() {
        bail!(
            "Pine Stylekit: {} error(s) — not writing {}",
            out.error_count(),
            scfg.output
        );
    }
    write_output(project, &scfg, &out.css)?;
    let count = out.css.lines().filter(|l| l.ends_with('{')).count();
    println!(
        "▶ stylekit {} → {} ({count} rules)",
        scfg.input, scfg.output
    );
    Ok(())
}

/// Recompile for a dev watch tick: render diagnostics but never abort
/// the dev loop. On error the previous stylesheet is left in place and
/// the failure is surfaced loudly (RFC 092 D6) rather than overwritten
/// with broken/partial output.
pub fn recompile_quiet(project: &Path, cfg: &PocopineConfig) {
    let scfg = resolve(cfg);
    match compile(project, &scfg) {
        Ok((out, files)) => {
            report(&out, &files);
            if out.has_errors() {
                eprintln!(
                    "✗ stylekit: {} error(s) — keeping last good {}",
                    out.error_count(),
                    scfg.output
                );
            } else if let Err(e) = write_output(project, &scfg, &out.css) {
                eprintln!("✗ stylekit: write failed: {e:#}");
            } else {
                println!("↻ stylekit → {}", scfg.output);
            }
        }
        Err(e) => eprintln!("✗ stylekit: {e:#}"),
    }
}

fn report(out: &ProjectCss, files: &[SourceFile]) {
    for diag in &out.diagnostics {
        eprint!("{}", render(diag, files));
    }
}

fn write_output(project: &Path, scfg: &StylekitConfig, css: &str) -> Result<()> {
    let output = project.join(&scfg.output);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&output, css).with_context(|| format!("write {}", output.display()))?;
    Ok(())
}

/// Recursively collect `*.poco` files under `dir` (skips hidden dirs).
fn collect_poco(dir: &Path, out: &mut Vec<SourceFile>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if !name.starts_with('.') {
                collect_poco(&path, out)?;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("poco") {
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            out.push(SourceFile { path, source });
        }
    }
    Ok(())
}

/// `pocopine stylekit` (hidden debug verb): print the catalog/metadata,
/// or compile once and dump/write the stylesheet.
pub fn run_command(path: &Path, dump: bool, docs: bool, metadata: bool) -> Result<()> {
    if docs {
        print!("{}", pocopine_stylekit::catalog::catalog().to_markdown());
        return Ok(());
    }
    if metadata {
        println!(
            "{}",
            pocopine_stylekit::catalog::catalog().to_metadata_json()
        );
        return Ok(());
    }
    let project = path.canonicalize()?;
    let cfg = crate::config::load(path)?;
    let scfg = resolve(&cfg);
    let (out, files) = compile(&project, &scfg)?;
    report(&out, &files);
    if out.has_errors() {
        bail!("Pine Stylekit: {} error(s)", out.error_count());
    }
    if dump {
        print!("{}", out.css);
    } else {
        write_output(&project, &scfg, &out.css)?;
        println!("▶ stylekit → {}", scfg.output);
    }
    Ok(())
}

/// Output paths derived from config (so callers can return a PathBuf if
/// they need to reference the stylesheet location).
#[allow(dead_code)]
pub fn output_path(project: &Path, cfg: &PocopineConfig) -> PathBuf {
    project.join(resolve(cfg).output)
}
