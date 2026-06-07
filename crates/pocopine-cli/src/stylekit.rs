//! Pine Stylekit build stage (RFC 092 D2/D6, step 7).
//!
//! Compiles utility CSS from a project's `.poco` sources in-process —
//! no external watcher, no shelling out. Runs **by default** (RFC 092
//! step 7); opt out with `--no-stylekit` or `enabled = false`, and a
//! Tailwind-only project (a `[tailwind]` block with no `[stylekit]`
//! block) defers automatically. Fails loud: on any error-severity
//! diagnostic the build aborts and the stale stylesheet is left
//! untouched rather than silently re-served.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pocopine_stylekit::{compile_project, render, CompileOptions, ProjectCss, SourceFile};

use crate::config::{PocopineConfig, StylekitConfig};

/// Whether the Stylekit stage should run (RFC 092 step 7 — on by
/// default). Precedence: `--no-stylekit` off, `--stylekit` on, then the
/// config block's `enabled`, else default on *unless* the project opted
/// into Tailwind only (a `[tailwind]` block with no `[stylekit]` block).
pub fn enabled(cfg: &PocopineConfig, force_on: bool, force_off: bool) -> bool {
    if force_off {
        return false;
    }
    if force_on {
        return true;
    }
    match &cfg.stylekit {
        Some(c) => c.enabled,
        None => cfg.tailwind.is_none(),
    }
}

/// Whether Stylekit was *explicitly* requested (vs. defaulted on). When
/// only defaulted, a missing input CSS is a silent skip rather than an
/// error — config-less projects without an `app.css` just don't get a
/// stylesheet.
fn explicit(cfg: &PocopineConfig, force_on: bool) -> bool {
    force_on || cfg.stylekit.is_some()
}

/// Resolve the effective config (defaults when Stylekit runs without a
/// config block).
fn resolve(cfg: &PocopineConfig) -> StylekitConfig {
    match &cfg.stylekit {
        Some(c) => StylekitConfig {
            input: c.input.clone(),
            output: c.output.clone(),
            src: c.src.clone(),
            preflight: c.preflight,
            enabled: c.enabled,
        },
        None => StylekitConfig::default(),
    }
}

/// LSP support: the project's `@theme` input CSS, or `None` when Stylekit isn't
/// enabled for the project or the input file is absent. The language server
/// feeds this to [`pocopine_stylekit::compile_project`] together with a single
/// open document, so editor class diagnostics use the same registry + theme
/// tokens + known component classes the build does — no false positives on
/// project-defined color tokens or author CSS classes.
pub fn project_theme_css(project: &Path) -> Option<String> {
    let cfg = crate::config::load(project).ok()?;
    if !enabled(&cfg, false, false) {
        return None;
    }
    let scfg = resolve(&cfg);
    std::fs::read_to_string(project.join(&scfg.input)).ok()
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

    let options = CompileOptions {
        preflight: scfg.preflight,
        ..Default::default()
    };
    let out = compile_project(&theme_css, &files, options);
    Ok((out, files))
}

/// One-shot compile for `build` / `run` / dev startup. Renders every
/// diagnostic; aborts the build (without writing) if any are errors.
///
/// `force_on` is the `--stylekit` flag. When the stage is only
/// defaulted on (not explicitly requested or configured) and the
/// project has no input CSS, it's a silent no-op instead of an error —
/// so config-less projects without an `app.css` aren't broken.
pub fn run_once(
    project: &Path,
    cfg: &PocopineConfig,
    force_on: bool,
    _release: bool,
) -> Result<()> {
    let scfg = resolve(cfg);
    if !explicit(cfg, force_on) && !project.join(&scfg.input).exists() {
        return Ok(());
    }
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
    if !project.join(&scfg.input).exists() {
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{StylekitConfig, TailwindConfig};

    #[test]
    fn default_on_for_bare_project() {
        // No CSS config at all → Stylekit runs (RFC 092 step 7).
        assert!(enabled(&PocopineConfig::default(), false, false));
    }

    #[test]
    fn tailwind_only_defers_but_flag_forces() {
        let cfg = PocopineConfig {
            tailwind: Some(TailwindConfig::default()),
            ..Default::default()
        };
        assert!(!enabled(&cfg, false, false), "tailwind-only project defers");
        assert!(enabled(&cfg, true, false), "--stylekit forces it on");
    }

    #[test]
    fn config_enabled_flag_is_honored() {
        let on = PocopineConfig {
            stylekit: Some(StylekitConfig::default()),
            ..Default::default()
        };
        assert!(enabled(&on, false, false));
        let off = PocopineConfig {
            stylekit: Some(StylekitConfig {
                enabled: false,
                ..StylekitConfig::default()
            }),
            ..Default::default()
        };
        assert!(!enabled(&off, false, false));
    }

    #[test]
    fn no_stylekit_flag_overrides_everything() {
        let cfg = PocopineConfig {
            stylekit: Some(StylekitConfig::default()),
            ..Default::default()
        };
        assert!(!enabled(&cfg, false, true));
    }

    #[test]
    fn explicit_when_configured_or_forced() {
        let bare = PocopineConfig::default();
        assert!(!explicit(&bare, false));
        assert!(explicit(&bare, true));
        let configured = PocopineConfig {
            stylekit: Some(StylekitConfig::default()),
            ..Default::default()
        };
        assert!(explicit(&configured, false));
    }
}
