//! `pocopine native` — the RFC-104 desktop target.
//!
//! `dev` / `build` reuse the existing wasm + CSS pipeline (the same
//! `build::wasm` + Stylekit/Tailwind stages `pocopine build` runs), then
//! drive the app's `src-tauri` host crate:
//!
//! * `dev` exports [`DEV_DIR_ENV`] pointing at the live project
//!   directory and `cargo run`s the host crate, so the native window
//!   serves the on-disk `pkg/` + `index.html` and a rebuild is picked up
//!   on reload.
//! * `build` builds the wasm in release, then bundles with `cargo tauri
//!   build` when the Tauri CLI is available, falling back to a plain
//!   `cargo build` of the host binary.
//!
//! `init` (and `dev`/`build` when `src-tauri/` is missing) scaffolds the
//! host crate. The scaffold targets an external project consuming
//! published crates; in-repo, see `examples/file-browser/src-tauri`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::args::{NativeArgs, NativeBuildArgs, NativeCmd, NativeDevArgs};
use crate::config::{self, NativeConfig, PocopineConfig};
use crate::{build, client_modules, stylekit, tailwind};

/// Environment variable the native shell reads for the dev static root.
/// Must match `pocopine_native::DEV_DIR_ENV` (RFC-104 contract); the CLI
/// does not depend on the native crate, so the string is duplicated here
/// deliberately.
const DEV_DIR_ENV: &str = "POCOPINE_NATIVE_DEV_DIR";

pub fn run(args: NativeArgs) -> Result<()> {
    let project = args
        .path
        .canonicalize()
        .with_context(|| format!("could not resolve project path {}", args.path.display()))?;
    let cfg = config::load(&args.path)?;
    let native = cfg.native.clone().unwrap_or_default();

    match &args.cmd {
        NativeCmd::Init => {
            let created = scaffold(&project, &native)?;
            report_scaffold(&native, &created);
            Ok(())
        }
        NativeCmd::Dev(dev_args) => dev(&project, &cfg, &native, dev_args),
        NativeCmd::Build(build_args) => build_native(&project, &cfg, &native, build_args),
    }
}

fn dev(
    project: &Path,
    cfg: &PocopineConfig,
    native: &NativeConfig,
    args: &NativeDevArgs,
) -> Result<()> {
    ensure_src_tauri(project, native)?;
    wasm_and_css(project, cfg, args.release, args.stylekit, args.no_stylekit)?;
    let src_tauri = project.join(&native.src_tauri);
    cargo_drive(&src_tauri, native, "run", args.release, Some(project))
}

fn build_native(
    project: &Path,
    cfg: &PocopineConfig,
    native: &NativeConfig,
    args: &NativeBuildArgs,
) -> Result<()> {
    ensure_src_tauri(project, native)?;
    let release = !args.debug;
    wasm_and_css(project, cfg, release, args.stylekit, args.no_stylekit)?;
    let src_tauri = project.join(&native.src_tauri);

    if !args.no_bundle && tauri_cli_available() {
        tauri_cli_build(&src_tauri, native, release)
    } else {
        if !args.no_bundle {
            println!(
                "ℹ tauri CLI not found; building the host binary only. \
                 Install it with `cargo install tauri-cli` to produce installers."
            );
        }
        cargo_drive(&src_tauri, native, "build", release, None)
    }
}

/// Build the wasm bundle and CSS — the same stages `pocopine build` runs.
fn wasm_and_css(
    project: &Path,
    cfg: &PocopineConfig,
    release: bool,
    stylekit_flag: bool,
    no_stylekit: bool,
) -> Result<()> {
    build::wasm(project, release)?;
    client_modules::build(project, release)?;
    if let Some(tw) = cfg.tailwind.as_ref() {
        tailwind::run_once(project, tw, release)?;
    }
    if stylekit::enabled(cfg, stylekit_flag, no_stylekit) {
        stylekit::run_once(project, cfg, stylekit_flag, release)?;
    }
    Ok(())
}

/// `cargo run`/`cargo build` the host crate in `src_tauri`. `dev_dir`,
/// when set, is exported as [`DEV_DIR_ENV`] so the shell serves the live
/// project directory.
fn cargo_drive(
    src_tauri: &Path,
    native: &NativeConfig,
    subcommand: &str,
    release: bool,
    dev_dir: Option<&Path>,
) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg(subcommand);
    if release {
        cmd.arg("--release");
    }
    if let Some(bin) = native.bin.as_deref() {
        cmd.arg("--bin").arg(bin);
    }
    if !native.features.is_empty() {
        cmd.arg("--features").arg(native.features.join(","));
    }
    cmd.current_dir(src_tauri);
    if let Some(dir) = dev_dir {
        cmd.env(DEV_DIR_ENV, dir);
    }

    let verb = if dev_dir.is_some() {
        "running"
    } else {
        "building"
    };
    println!("▶ {verb} native host crate ({})", src_tauri.display());
    let status = cmd.status().with_context(|| {
        "failed to invoke cargo for the native host crate (is the Rust toolchain installed?)"
    })?;
    if !status.success() {
        bail!("native host crate `cargo {subcommand}` failed with {status}");
    }
    Ok(())
}

fn tauri_cli_build(src_tauri: &Path, native: &NativeConfig, release: bool) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg("tauri").arg("build");
    if !release {
        cmd.arg("--debug");
    }
    if !native.features.is_empty() {
        cmd.arg("--features").arg(native.features.join(","));
    }
    cmd.current_dir(src_tauri);

    println!("▶ cargo tauri build ({})", src_tauri.display());
    let status = cmd
        .status()
        .context("failed to invoke `cargo tauri build`")?;
    if !status.success() {
        bail!("`cargo tauri build` failed with {status}");
    }
    Ok(())
}

fn tauri_cli_available() -> bool {
    Command::new("cargo")
        .args(["tauri", "--version"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

// ─── scaffolding ────────────────────────────────────────────────────

/// Scaffold `src-tauri/` if it does not already exist.
fn ensure_src_tauri(project: &Path, native: &NativeConfig) -> Result<()> {
    if project.join(&native.src_tauri).join("Cargo.toml").exists() {
        return Ok(());
    }
    println!(
        "ℹ no `{}` found — scaffolding the Tauri host crate",
        native.src_tauri
    );
    let created = scaffold(project, native)?;
    report_scaffold(native, &created);
    Ok(())
}

/// Write the host-crate files that don't already exist, returning the
/// paths created. Never overwrites — re-running is safe.
fn scaffold(project: &Path, native: &NativeConfig) -> Result<Vec<PathBuf>> {
    let app = crate_name(project)?;
    let app_ident = app.replace('-', "_");
    let title = native.title.clone().unwrap_or_else(|| app.clone());

    let dir = project.join(&native.src_tauri);
    std::fs::create_dir_all(dir.join("src"))
        .with_context(|| format!("create {}", dir.join("src").display()))?;

    let files = [
        (dir.join("Cargo.toml"), render_cargo_toml(&app)),
        (dir.join("build.rs"), BUILD_RS.to_string()),
        (
            dir.join("tauri.conf.json"),
            render_tauri_conf(&app_ident, &title),
        ),
        (dir.join("src/main.rs"), render_main_rs(&app_ident, &title)),
        (dir.join(".gitignore"), GITIGNORE.to_string()),
    ];

    let mut created = Vec::new();
    for (path, contents) in files {
        if path.exists() {
            continue;
        }
        std::fs::write(&path, contents).with_context(|| format!("write {}", path.display()))?;
        created.push(path);
    }
    Ok(created)
}

fn report_scaffold(native: &NativeConfig, created: &[PathBuf]) {
    if created.is_empty() {
        println!(
            "✓ `{}` already scaffolded; nothing to write",
            native.src_tauri
        );
        return;
    }
    println!("✓ scaffolded native host crate at `{}`:", native.src_tauri);
    for path in created {
        println!("   + {}", path.display());
    }
    println!(
        "Next:\n  \
         • add an app icon and list it under `bundle.icon` in {}/tauri.conf.json\n  \
         • run `pocopine native dev` to launch the window",
        native.src_tauri
    );
}

/// Read `package.name` from the project's `Cargo.toml`.
fn crate_name(project: &Path) -> Result<String> {
    #[derive(Deserialize)]
    struct Manifest {
        package: PackageName,
    }
    #[derive(Deserialize)]
    struct PackageName {
        name: String,
    }
    let manifest_path = project.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parse {}", manifest_path.display()))?;
    Ok(manifest.package.name)
}

const BUILD_RS: &str = "fn main() {\n    tauri_build::build();\n}\n";

const GITIGNORE: &str = "/target\n/gen\n";

/// `src-tauri/Cargo.toml` for an external project (published crates). The
/// in-repo `examples/file-browser/src-tauri` uses path dependencies.
fn render_cargo_toml(app: &str) -> String {
    format!(
        r#"[package]
name = "{app}-native"
version = "0.1.0"
edition = "2021"
publish = false

[[bin]]
name = "{app}-native"
path = "src/main.rs"

[build-dependencies]
tauri-build = {{ version = "2", features = [] }}

[dependencies]
# The app crate (one directory up), linked so its `#[server]` inventory
# is present in the native binary.
{app} = {{ path = ".." }}
pocopine-native-tauri = {{ version = "0.1", features = ["tauri"] }}
tauri = {{ version = "2", features = [] }}
"#
    )
}

fn render_main_rs(app_ident: &str, title: &str) -> String {
    format!(
        r#"//! Native (Tauri) entry point — generated by `pocopine native init`.
//!
//! Host-only binary. Links the app rlib so its `#[server]` inventory is
//! present, then opens the native window. See RFC-104.

use {app_ident} as _;

fn main() {{
    pocopine_native_tauri::run!(
        pocopine_native_tauri::NativeApp::new()
            .title("{title}")
            .inner_size(1100.0, 720.0)
    );
}}
"#
    )
}

fn render_tauri_conf(app_ident: &str, title: &str) -> String {
    format!(
        r#"{{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "{title}",
  "version": "0.1.0",
  "identifier": "com.pocopine.{app_ident}",
  "build": {{
    "beforeDevCommand": "",
    "beforeBuildCommand": ""
  }},
  "app": {{
    "withGlobalTauri": false,
    "windows": [],
    "security": {{
      "csp": null
    }}
  }},
  "bundle": {{
    "active": true,
    "targets": "all",
    "icon": [],
    "resources": {{
      "../index.html": "index.html",
      "../pkg": "pkg"
    }}
  }}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_writes_host_crate_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"my-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let native = NativeConfig::default();

        let created = scaffold(project, &native).unwrap();
        assert_eq!(
            created.len(),
            5,
            "Cargo.toml, build.rs, conf, main.rs, gitignore"
        );

        let main_rs = std::fs::read_to_string(project.join("src-tauri/src/main.rs")).unwrap();
        // crate name `my-app` → ident `my_app` in the `use … as _;` link.
        assert!(main_rs.contains("use my_app as _;"));
        assert!(main_rs.contains("pocopine_native_tauri::run!"));

        let cargo = std::fs::read_to_string(project.join("src-tauri/Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"my-app-native\""));
        assert!(cargo.contains("pocopine-native-tauri"));
        assert!(cargo.contains(r#"my-app = { path = ".." }"#));

        let conf = std::fs::read_to_string(project.join("src-tauri/tauri.conf.json")).unwrap();
        assert!(conf.contains("\"identifier\": \"com.pocopine.my_app\""));

        // Re-running writes nothing new.
        let again = scaffold(project, &native).unwrap();
        assert!(
            again.is_empty(),
            "scaffold must not overwrite existing files"
        );
    }

    #[test]
    fn scaffold_honours_configured_title_and_dir() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"keep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let native = NativeConfig {
            src_tauri: "desktop".into(),
            title: Some("Keep Notes".into()),
            ..NativeConfig::default()
        };

        scaffold(project, &native).unwrap();
        let main_rs = std::fs::read_to_string(project.join("desktop/src/main.rs")).unwrap();
        assert!(main_rs.contains(r#".title("Keep Notes")"#));
    }
}
