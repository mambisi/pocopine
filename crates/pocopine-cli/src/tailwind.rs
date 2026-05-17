use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::config::TailwindConfig;
use crate::tools;

/// A running Tailwind watcher child. Dropped via [`TailwindChild::kill`]
/// on CLI exit so the process doesn't outlive us.
pub struct TailwindChild {
    child: Child,
}

impl TailwindChild {
    pub(crate) fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run Tailwind once (used by `build` and `run`). `release` enables
/// `--minify`.
pub fn run_once(project: &Path, tw: &TailwindConfig, release: bool) -> Result<()> {
    let mut cmd = command(project, tw)?;
    let input = project.join(&tw.input);
    let output = project.join(&tw.output);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    println!(
        "▶ tailwindcss {} → {}",
        tw.input.trim_start_matches("./"),
        tw.output.trim_start_matches("./")
    );
    cmd.arg("-i").arg(&input).arg("-o").arg(&output);
    if release {
        cmd.arg("--minify");
    }
    cmd.current_dir(project);
    let status = cmd.status().context("invoke tailwindcss")?;
    if !status.success() {
        bail!("tailwindcss exited with {status}");
    }
    Ok(())
}

/// Spawn Tailwind in `--watch` mode for `dev`. The returned handle
/// must be killed on CLI exit.
pub fn spawn_watch(project: &Path, tw: &TailwindConfig) -> Result<TailwindChild> {
    let mut cmd = command(project, tw)?;
    let input = project.join(&tw.input);
    let output = project.join(&tw.output);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    println!(
        "▶ tailwindcss --watch ({} → {})",
        tw.input.trim_start_matches("./"),
        tw.output.trim_start_matches("./")
    );
    let child = cmd
        .arg("-i")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .arg("--watch")
        .current_dir(project)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn tailwindcss --watch")?;
    Ok(TailwindChild { child })
}

/// Resolve the Tailwind command: first explicit config path, then
/// `.pocopine.toml`, `$PATH`, then `<target>/pocopine/bin/tailwindcss(.exe)`,
/// downloading it from GitHub Releases if absent. Uses `version` from the
/// config so projects can pin upstream independently.
fn command(project: &Path, tw: &TailwindConfig) -> Result<Command> {
    if let Some(explicit) = tw.binary.as_ref() {
        let resolved = if explicit.is_absolute() {
            explicit.clone()
        } else {
            project.join(explicit)
        };
        if !resolved.exists() {
            bail!(
                "pocopine.tailwind.binary set to {}, but the file does not exist",
                resolved.display()
            );
        }
        return Ok(Command::new(resolved));
    }

    let project_tools = tools::ProjectTools::load(project)?;
    if let Some(tool) = project_tools.tailwindcss() {
        if tools::resolve_program(&tool).is_none() {
            bail!(
                "{} not found. Fix `[tools].tailwindcss` in {}.",
                tool.display(),
                project_tools.config_path().display()
            );
        }
        return Ok(tool.command());
    }

    if let Some(found) = tools::which("tailwindcss") {
        return Ok(Command::new(found));
    }

    let bin_dir = project.join("target").join("pocopine").join("bin");
    // Version-suffix the cached binary so bumping the pinned version
    // in Cargo.toml (or `latest` resolving to a new release) forces a
    // fresh download instead of silently running a stale binary.
    let bin_name = if cfg!(windows) {
        format!("tailwindcss-{}.exe", tw.version)
    } else {
        format!("tailwindcss-{}", tw.version)
    };
    let bin_path = bin_dir.join(&bin_name);
    if bin_path.exists() {
        return Ok(Command::new(bin_path));
    }

    std::fs::create_dir_all(&bin_dir).with_context(|| format!("create {}", bin_dir.display()))?;

    #[cfg(not(target_arch = "wasm32"))]
    {
        let asset = tailwind_asset_name()?;
        let url = if tw.version == "latest" {
            format!("https://github.com/tailwindlabs/tailwindcss/releases/latest/download/{asset}")
        } else {
            format!(
                "https://github.com/tailwindlabs/tailwindcss/releases/download/{version}/{asset}",
                version = tw.version,
                asset = asset,
            )
        };
        println!("▶ downloading tailwindcss {} ({asset})", tw.version);
        let bytes = reqwest::blocking::get(&url)
            .with_context(|| format!("fetch {url}"))?
            .error_for_status()
            .with_context(|| format!("HTTP error fetching {url}"))?
            .bytes()
            .context("read tailwindcss download")?;
        std::fs::write(&bin_path, &bytes)
            .with_context(|| format!("write {}", bin_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin_path, perms)?;
        }
        Ok(Command::new(bin_path))
    }
    #[cfg(target_arch = "wasm32")]
    {
        bail!("tailwindcss not on $PATH and auto-download requires a host build of pocopine-cli");
    }
}

/// Map the current host to the asset filename on
/// `tailwindlabs/tailwindcss` releases.
#[cfg(not(target_arch = "wasm32"))]
fn tailwind_asset_name() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("tailwindcss-linux-x64"),
        ("linux", "aarch64") => Ok("tailwindcss-linux-arm64"),
        ("macos", "x86_64") => Ok("tailwindcss-macos-x64"),
        ("macos", "aarch64") => Ok("tailwindcss-macos-arm64"),
        ("windows", "x86_64") => Ok("tailwindcss-windows-x64.exe"),
        (os, arch) => bail!(
            "no Tailwind standalone binary known for {os}/{arch} - set \
             `pocopine.tailwind.binary` to a local path"
        ),
    }
}
