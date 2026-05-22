//! `pocopine deploy` subcommand (RFC 080 §7).
//!
//! Wires `pocopine-deploy`'s `DeployAdapter` pipeline to the CLI:
//!
//!   1. read `[package.metadata.pocopine.deploy]` from the project's
//!      `Cargo.toml` (RFC 080 §4.1 — Pocopine.toml is the long-term home,
//!      but cargo metadata is what's available today)
//!   2. infer `app_name` from `[package].name` and `git_sha` from
//!      `git rev-parse HEAD`
//!   3. resolve the adapter (`fly` is built-in; new vendors live in
//!      their own crates)
//!   4. run the pipeline: `detect_constraints` → `render_config` →
//!      flush to disk → `build_artefact` → `deploy` → `post_deploy_hint`
//!
//! Non-`run` subcommands:
//!   * `pocopine deploy auth <host>` / `--list` / `--revoke <host>` —
//!     manage `~/.pocopine/credentials.toml`.
//!   * `pocopine deploy doctor` — check docker daemon + configured
//!     tokens for every known host.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use pocopine_deploy::{
    credentials, docker::DockerClient, spec, Constraint, DeployAdapter, Hint, StagedFiles,
};

use crate::args::{AuthArgs, DeployArgs, DeployCmd};

pub fn run(args: &DeployArgs) -> Result<()> {
    match &args.cmd {
        None => run_deploy(args),
        Some(DeployCmd::Auth(a)) => run_auth(a),
        Some(DeployCmd::Doctor) => run_doctor(),
    }
}

fn run_deploy(args: &DeployArgs) -> Result<()> {
    let project = args
        .path
        .canonicalize()
        .with_context(|| format!("resolving project path {}", args.path.display()))?;

    let manifest = read_manifest(&project)?;
    let app_name = app_name_from_manifest(&manifest)?;
    let deploy_table = deploy_table_from_manifest(&manifest)?;
    let git_sha = short_git_sha(&project)?;

    let target = args
        .target
        .as_deref()
        .context("--target required (e.g. `--target fly`)")?;

    let environment = if args.prod {
        Some("production".to_owned())
    } else {
        None
    };
    let mut spec = spec::parse(deploy_table.clone(), app_name, git_sha, environment)?;
    // first_deploy: derived from .pocopine/deploy/<target>.toml presence.
    // The state file is env-scoped so production and staging deploys
    // track their first-deploy state independently.
    let state_basename = match spec.environment.as_deref() {
        Some(env) => format!("{target}-{env}.toml"),
        None => format!("{target}.toml"),
    };
    let state_file = project.join(".pocopine/deploy").join(&state_basename);
    spec.first_deploy = !state_file.exists();
    spec.skip_build = args.skip_build;
    // The `origin` remote lets adapters derive a default container
    // registry (GitHub → GHCR, GitLab → GitLab CR) when no explicit
    // `image_registry` is set. Best-effort: `None` if there's no remote.
    spec.git_remote = discover_git_remote(&project);

    let adapter = resolve_adapter(target)?;

    // 1. Constraints.
    let constraints = adapter.detect_constraints(&spec);
    let mut refused = false;
    for c in &constraints {
        match c {
            Constraint::Refuse(msg) => {
                eprintln!("error: {msg}");
                refused = true;
            }
            Constraint::Warn(msg) => eprintln!("warn:  {msg}"),
            Constraint::Hint(msg) => eprintln!("hint:  {msg}"),
        }
    }
    if refused {
        bail!("aborting: deploy refused by adapter constraints");
    }

    // 2. Render config to a staging dir.
    let mut staged = StagedFiles::new();
    adapter.render_config(&spec, &mut staged);

    if args.dry_run {
        eprintln!(
            "--dry-run: would write {} file(s) under {}",
            staged.len(),
            project.display(),
        );
        for (path, content) in staged.iter() {
            println!("\n=== {path} ===");
            println!("{content}");
        }
        if args.skip_build {
            eprintln!("--dry-run: --skip-build is set; would reuse the existing pushed image and run the host-API deploy only.");
        } else {
            eprintln!("--dry-run: would then bundle wasm + run `docker build` + `docker push` + host-API deploy.");
        }
        return Ok(());
    }

    // 3. Flush staged files to disk. Refuse to clobber a hand-edited
    //    file that doesn't carry our `GENERATED_MARKER` — RFC 080 §7
    //    expects regenerated files to be warned/frozen, not silently
    //    overwritten.
    for (path, content) in staged.iter() {
        let dest = project.join(path);
        flush_one(&dest, content)?;
    }

    // 4. Build the client bundle + configured bins exactly the same
    //    way `pocopine build --release` does, so the Dockerfile's
    //    final-stage COPY picks up fresh assets instead of a stale
    //    `pkg/` or empty `dist/`. Skipped under `--skip-build`: the
    //    image is expected to already be in the host's registry.
    if !args.skip_build {
        let cfg = crate::config::load(&args.path)?;
        crate::build::wasm(&project, true)?;
        crate::client_modules::build(&project, true)?;
        crate::build::configured_bins(&args.path, &cfg, true)?;
        if let Some(tw) = cfg.tailwind.as_ref() {
            crate::tailwind::run_once(&project, tw, true)?;
        }
    }

    // 5. CD into project so adapter `build_artefact` (which calls
    //    `docker build .`) and `deploy` resolve files at the right root.
    let original_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(&project).with_context(|| format!("cd {}", project.display()))?;
    let result = (|| -> Result<()> {
        let artefact = if args.skip_build {
            adapter.default_artefact(&spec)
        } else {
            adapter.build_artefact(&spec)?
        };
        let outcome = adapter.deploy(&spec, &artefact)?;

        for hint in adapter.post_deploy_hint(&spec, &outcome) {
            match hint {
                Hint::Info(s) => println!("{s}"),
                Hint::OneTime(s) => {
                    println!("\n--- one-time setup ---\n{s}");
                }
            }
        }

        // Record the deploy so subsequent runs see `first_deploy = false`.
        if let Some(parent) = state_file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&state_file, format!("app = {:?}\n", spec.app_name)).ok();

        Ok(())
    })();
    if let Some(cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }
    result
}

fn run_auth(args: &AuthArgs) -> Result<()> {
    if args.list {
        let entries = credentials::list()?;
        if entries.is_empty() {
            println!("No host tokens configured.");
            return Ok(());
        }
        for (host, source) in entries {
            let src = match source {
                credentials::Source::File => "file",
                credentials::Source::Env => "env",
                credentials::Source::EnvOverridesFile => "env (overrides file)",
            };
            println!("{host:<12} {src}");
        }
        return Ok(());
    }

    if let Some(host) = args.revoke.as_deref() {
        credentials::revoke(host)?;
        println!("Revoked token for `{host}`.");
        return Ok(());
    }

    let host = args
        .host
        .as_deref()
        .context("usage: `pocopine deploy auth <host>` | `--list` | `--revoke <host>`")?;
    let dashboard = dashboard_url_for(host);
    eprintln!("Paste the API token for `{host}`. Get one from:");
    eprintln!("    {dashboard}");
    eprintln!();
    eprint!("token: ");
    std::io::stderr().flush().ok();

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading token from stdin")?;
    let token = line.trim();
    if token.is_empty() {
        bail!("empty token");
    }

    credentials::store(host, token)?;
    eprintln!("Stored token for `{host}` to ~/.pocopine/credentials.toml (mode 0600).");
    Ok(())
}

fn run_doctor() -> Result<()> {
    println!("pocopine deploy doctor");
    println!();

    print!("docker: ");
    let docker = DockerClient::new();
    match docker.check_available() {
        Ok(()) => println!("ok"),
        Err(e) => println!("unavailable\n  {e}"),
    }
    println!();

    println!("configured hosts:");
    for host in credentials::KNOWN_HOSTS {
        match credentials::load(host) {
            Ok(_) => println!("  {host:<10} ok"),
            Err(_) => {
                println!("  {host:<10} no token (run `pocopine deploy auth {host}` to configure)")
            }
        }
    }

    Ok(())
}

// ─── Helpers ───────────────────────────────────────────────────────────

/// Write `content` to `dest`, refusing to overwrite a hand-edited file.
/// "Hand-edited" = the existing file is present and does not contain the
/// `pocopine-deploy::common::GENERATED_MARKER`. Identical existing
/// content (e.g. a re-render produced the same bytes) is treated as a
/// no-op so the unmarked-but-canonical case doesn't keep refusing.
fn flush_one(dest: &Path, content: &str) -> Result<()> {
    use pocopine_deploy::common::GENERATED_MARKER;

    if dest.exists() {
        let existing = std::fs::read_to_string(dest)
            .with_context(|| format!("reading existing {}", dest.display()))?;
        let existing_is_ours = existing.contains(GENERATED_MARKER);
        if !existing_is_ours && existing.trim() != content.trim() {
            bail!(
                "refusing to overwrite hand-edited file `{}` — file does not carry the pocopine-generated marker.\n\
                 Move it aside (or delete it) to let pocopine regenerate, or commit the generated version next pass.",
                dest.display(),
            );
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(dest, content).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

fn resolve_adapter(target: &str) -> Result<Box<dyn DeployAdapter>> {
    match target {
        "fly" => Ok(Box::new(pocopine_deploy_fly::FlyAdapter)),
        "railway" => Ok(Box::new(pocopine_deploy_railway::RailwayAdapter)),
        "render" => Ok(Box::new(pocopine_deploy_render::RenderAdapter)),
        other => bail!("unknown target `{other}`. Known: fly, railway, render."),
    }
}

fn dashboard_url_for(host: &str) -> &'static str {
    match host {
        "fly" => "https://fly.io/user/personal_access_tokens",
        "railway" => "https://railway.com/account/tokens",
        "render" => "https://dashboard.render.com/u/settings#api-keys",
        _ => "<host dashboard>",
    }
}

fn read_manifest(project: &Path) -> Result<toml::Value> {
    let cargo_toml = project.join("Cargo.toml");
    let raw = std::fs::read_to_string(&cargo_toml)
        .with_context(|| format!("reading {}", cargo_toml.display()))?;
    let v: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parsing {}", cargo_toml.display()))?;
    Ok(v)
}

fn app_name_from_manifest(manifest: &toml::Value) -> Result<String> {
    manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_owned)
        .context("Cargo.toml missing [package].name (required to derive app name)")
}

fn deploy_table_from_manifest(manifest: &toml::Value) -> Result<toml::Value> {
    manifest
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("pocopine"))
        .and_then(|p| p.get("deploy"))
        .cloned()
        .context(
            "no [package.metadata.pocopine.deploy] in Cargo.toml — see RFC 080 §4.1 for the schema.",
        )
}

fn short_git_sha(project: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project)
        .output()
        .context("running `git rev-parse HEAD`")?;
    if !out.status.success() {
        bail!(
            "`git rev-parse HEAD` failed; is `{}` a git repo?",
            project.display()
        );
    }
    let sha = String::from_utf8(out.stdout)?.trim().to_owned();
    if sha.is_empty() {
        bail!("`git rev-parse HEAD` returned an empty SHA");
    }
    Ok(sha.chars().take(7).collect())
}

/// Best-effort `git remote get-url origin`. Returns `None` when there is
/// no `origin` remote — adapters then fall back to requiring an explicit
/// `image_registry`.
fn discover_git_remote(project: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(project)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if url.is_empty() {
        None
    } else {
        Some(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_from_manifest_reads_package_name() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [package]
            name = "myapp"
            version = "0.1.0"
        "#,
        )
        .unwrap();
        assert_eq!(app_name_from_manifest(&manifest).unwrap(), "myapp");
    }

    #[test]
    fn app_name_from_manifest_errors_without_package() {
        let manifest: toml::Value = toml::from_str(r#"[workspace]"#).unwrap();
        assert!(app_name_from_manifest(&manifest).is_err());
    }

    #[test]
    fn deploy_table_reads_nested_metadata() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [package]
            name = "myapp"

            [package.metadata.pocopine.deploy]
            mode = "fullstack"
        "#,
        )
        .unwrap();
        let table = deploy_table_from_manifest(&manifest).unwrap();
        assert_eq!(
            table.get("mode").and_then(|v| v.as_str()),
            Some("fullstack")
        );
    }

    #[test]
    fn deploy_table_errors_when_missing() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [package]
            name = "myapp"
        "#,
        )
        .unwrap();
        let err = deploy_table_from_manifest(&manifest)
            .unwrap_err()
            .to_string();
        assert!(err.contains("[package.metadata.pocopine.deploy]"));
    }

    #[test]
    fn dashboard_url_for_known_hosts() {
        assert!(dashboard_url_for("fly").contains("fly.io"));
        assert!(dashboard_url_for("railway").contains("railway"));
        assert_eq!(dashboard_url_for("nope"), "<host dashboard>");
    }

    #[test]
    fn resolve_adapter_known_target() {
        assert!(resolve_adapter("fly").is_ok());
        assert!(resolve_adapter("railway").is_ok());
        assert!(resolve_adapter("render").is_ok());
        assert!(resolve_adapter("nope").is_err());
    }

    #[test]
    fn flush_one_writes_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("Dockerfile");
        flush_one(
            &dest,
            "# Generated by pocopine-deploy (test)\nFROM scratch\n",
        )
        .unwrap();
        assert!(dest.exists());
    }

    #[test]
    fn flush_one_refuses_to_clobber_unmarked_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("Dockerfile");
        std::fs::write(&dest, "FROM ubuntu\nRUN do-not-clobber-me\n").unwrap();
        let err = flush_one(
            &dest,
            "# Generated by pocopine-deploy (test)\nFROM scratch\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("refusing to overwrite hand-edited"));
    }

    #[test]
    fn flush_one_allows_overwrite_of_marker_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("Dockerfile");
        std::fs::write(&dest, "# Generated by pocopine-deploy (old)\nold\n").unwrap();
        flush_one(&dest, "# Generated by pocopine-deploy (new)\nnew\n").unwrap();
        let after = std::fs::read_to_string(&dest).unwrap();
        assert!(after.contains("new"));
    }

    #[test]
    fn flush_one_noop_when_existing_matches_unmarked_content() {
        // Re-renders that happen to produce the same bytes shouldn't
        // start refusing just because the canonical content has no
        // marker (e.g. an external user-checked-in copy).
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(".dockerignore");
        let body = "target/\n";
        std::fs::write(&dest, body).unwrap();
        flush_one(&dest, body).unwrap(); // identical → no-op write, no error
    }
}
