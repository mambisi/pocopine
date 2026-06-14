//! `pocopine native` — the RFC-104 desktop target.
//!
//! Convention over config: the Tauri host crate always lives in
//! `src-tauri/` next to the app, with a single binary that already
//! enables the `tauri` feature. There is no `[package.metadata.pocopine
//! .native]` block — everything is driven by flags.
//!
//! * `dev` exports [`DEV_DIR_ENV`] pointing at the live project directory
//!   and `cargo run`s the host crate, so the window serves the on-disk
//!   `pkg/` + `index.html` and a rebuild is picked up on reload.
//! * `build` builds the wasm in release, then bundles with `cargo tauri
//!   build` when the Tauri CLI is available, else a plain `cargo build`.
//! * `init` (and `dev`/`build` when `src-tauri/` is missing) scaffolds
//!   the host crate.
//!
//! Backend selection is a single flag: `--backend <url>` forwards the
//! app's `#[server]` calls to a deployed server ("server"); omitting it
//! runs them in-process ("standalone").

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::args::{NativeArgs, NativeBuildArgs, NativeCmd, NativeDevArgs};
use crate::config::{self, PocopineConfig};
use crate::{build, client_modules, stylekit, tailwind};

/// Environment variable the native shell reads for the dev static root.
/// Must match `pocopine_native::DEV_DIR_ENV` (RFC-104 contract); the CLI
/// does not depend on the native crate, so the string is duplicated here
/// deliberately.
const DEV_DIR_ENV: &str = "POCOPINE_NATIVE_DEV_DIR";

/// Environment variable carrying the server backend URL. When set, the
/// native shell forwards `#[server]` calls there ("server"); unset →
/// standalone (in-process). Matches the shell's `BACKEND_ENV`.
const BACKEND_ENV: &str = "POCOPINE_NATIVE_BACKEND";

/// Convention: the Tauri host crate lives here, relative to the project.
const SRC_TAURI: &str = "src-tauri";

pub fn run(args: NativeArgs) -> Result<()> {
    let project = args
        .path
        .canonicalize()
        .with_context(|| format!("could not resolve project path {}", args.path.display()))?;
    // Loaded only for the shared wasm + CSS stages (tailwind / stylekit).
    let cfg = config::load(&args.path)?;

    match &args.cmd {
        NativeCmd::Init => {
            let created = scaffold(&project)?;
            report_scaffold(&created);
            Ok(())
        }
        NativeCmd::Dev(dev_args) => dev(&project, &cfg, dev_args),
        NativeCmd::Build(build_args) => build_native(&project, &cfg, build_args),
    }
}

fn dev(project: &Path, cfg: &PocopineConfig, args: &NativeDevArgs) -> Result<()> {
    ensure_src_tauri(project)?;
    let backend = args.backend.as_deref().map(normalize_backend);
    wasm_and_css(project, cfg, args.release, args.stylekit, args.no_stylekit)?;
    cargo_drive(
        &project.join(SRC_TAURI),
        "run",
        args.release,
        Some(project),
        backend.as_deref(),
    )
}

fn build_native(project: &Path, cfg: &PocopineConfig, args: &NativeBuildArgs) -> Result<()> {
    ensure_src_tauri(project)?;
    let backend = args.backend.as_deref().map(normalize_backend);
    let release = !args.debug;
    wasm_and_css(project, cfg, release, args.stylekit, args.no_stylekit)?;
    let src_tauri = project.join(SRC_TAURI);

    if !args.no_bundle && tauri_cli_available() {
        tauri_cli_build(&src_tauri, release, backend.as_deref())
    } else {
        if !args.no_bundle {
            println!(
                "ℹ tauri CLI not found; building the host binary only. \
                 Install it with `cargo install tauri-cli` to produce installers."
            );
        }
        cargo_drive(&src_tauri, "build", release, None, backend.as_deref())
    }
}

/// Strip a trailing slash so `{base}{path}` joins cleanly in the shell.
fn normalize_backend(url: &str) -> String {
    url.trim_end_matches('/').to_string()
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

/// `cargo run`/`cargo build` the host crate (its single default bin).
/// `dev_dir`, when set, is exported as [`DEV_DIR_ENV`]; `backend`, when
/// set, as [`BACKEND_ENV`].
fn cargo_drive(
    src_tauri: &Path,
    subcommand: &str,
    release: bool,
    dev_dir: Option<&Path>,
    backend: Option<&str>,
) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg(subcommand);
    if release {
        cmd.arg("--release");
    }
    cmd.current_dir(src_tauri);
    if let Some(dir) = dev_dir {
        cmd.env(DEV_DIR_ENV, dir);
    }
    if let Some(url) = backend {
        cmd.env(BACKEND_ENV, url);
    }

    let verb = if dev_dir.is_some() {
        "running"
    } else {
        "building"
    };
    println!(
        "▶ {verb} native host crate ({}, {})",
        src_tauri.display(),
        backend_label(backend),
    );
    let status = cmd.status().with_context(|| {
        "failed to invoke cargo for the native host crate (is the Rust toolchain installed?)"
    })?;
    if !status.success() {
        bail!("native host crate `cargo {subcommand}` failed with {status}");
    }
    Ok(())
}

fn tauri_cli_build(src_tauri: &Path, release: bool, backend: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg("tauri").arg("build");
    if !release {
        cmd.arg("--debug");
    }
    cmd.current_dir(src_tauri);
    if let Some(url) = backend {
        cmd.env(BACKEND_ENV, url);
    }

    println!(
        "▶ cargo tauri build ({}, {})",
        src_tauri.display(),
        backend_label(backend),
    );
    let status = cmd
        .status()
        .context("failed to invoke `cargo tauri build`")?;
    if !status.success() {
        bail!("`cargo tauri build` failed with {status}");
    }
    Ok(())
}

/// Human-readable backend mode for build logs.
fn backend_label(backend: Option<&str>) -> String {
    match backend {
        Some(url) => format!("server → {url}"),
        None => "standalone".to_string(),
    }
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
fn ensure_src_tauri(project: &Path) -> Result<()> {
    if project.join(SRC_TAURI).join("Cargo.toml").exists() {
        return Ok(());
    }
    println!("ℹ no `{SRC_TAURI}/` found — scaffolding the Tauri host crate");
    let created = scaffold(project)?;
    report_scaffold(&created);
    Ok(())
}

/// Write the host-crate files that don't already exist, returning the
/// paths created. Never overwrites — re-running is safe.
fn scaffold(project: &Path) -> Result<Vec<PathBuf>> {
    let app = crate_name(project)?;
    let app_ident = app.replace('-', "_");
    // Convention: window title defaults to the crate name; edit the
    // generated `main.rs` / `tauri.conf.json` to change it.
    let title = app.clone();

    let dir = project.join(SRC_TAURI);
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

    // Tauri's `generate_context!` embeds a window icon at compile time and
    // fails if none exists, so ship a placeholder. Replace it (and add the
    // platform formats) with `cargo tauri icon <your-icon.png>`.
    let icon = dir.join("icons/icon.png");
    if !icon.exists() {
        std::fs::create_dir_all(dir.join("icons"))
            .with_context(|| format!("create {}", dir.join("icons").display()))?;
        std::fs::write(&icon, NATIVE_ICON).with_context(|| format!("write {}", icon.display()))?;
        created.push(icon);
    }

    Ok(created)
}

fn report_scaffold(created: &[PathBuf]) {
    if created.is_empty() {
        println!("✓ `{SRC_TAURI}/` already scaffolded; nothing to write");
        return;
    }
    println!("✓ scaffolded native host crate at `{SRC_TAURI}/`:");
    for path in created {
        println!("   + {}", path.display());
    }
    println!(
        "Next:\n  \
         • add an app icon and list it under `bundle.icon` in {SRC_TAURI}/tauri.conf.json\n  \
         • run `pocopine native dev` to launch the window"
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

/// Placeholder window icon written into a scaffolded `src-tauri/icons/`.
/// Tauri's `generate_context!` requires one at compile time; users swap
/// in their own with `cargo tauri icon`.
const NATIVE_ICON: &[u8] = include_bytes!("../assets/native-icon.png");

/// `src-tauri/Cargo.toml` for an external project (published crates). The
/// in-repo `examples/file-browser/src-tauri` uses path dependencies.
///
/// The empty `[workspace]` table makes the crate its own workspace so it
/// builds standalone even when the app lives inside a larger Cargo
/// workspace (otherwise Cargo errors "believes it's in a workspace when
/// it's not" when `pocopine native` builds from this directory).
fn render_cargo_toml(app: &str) -> String {
    format!(
        r#"[workspace]

[package]
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
    "icon": ["icons/icon.png"],
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
    fn normalize_backend_trims_trailing_slash() {
        assert_eq!(
            normalize_backend("https://api.example.com/"),
            "https://api.example.com"
        );
        assert_eq!(
            normalize_backend("https://api.example.com"),
            "https://api.example.com"
        );
    }

    #[test]
    fn backend_label_distinguishes_modes() {
        assert_eq!(backend_label(None), "standalone");
        assert_eq!(
            backend_label(Some("https://api.example.com")),
            "server → https://api.example.com"
        );
    }

    #[test]
    fn scaffold_writes_host_crate_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"my-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let created = scaffold(project).unwrap();
        assert_eq!(
            created.len(),
            6,
            "Cargo.toml, build.rs, conf, main.rs, gitignore, icons/icon.png"
        );

        // A window icon is shipped so `generate_context!` can embed one.
        let icon = project.join("src-tauri/icons/icon.png");
        assert!(icon.is_file());
        assert_eq!(&std::fs::read(&icon).unwrap()[..8], b"\x89PNG\r\n\x1a\n");

        let main_rs = std::fs::read_to_string(project.join("src-tauri/src/main.rs")).unwrap();
        // crate name `my-app` → ident `my_app` in the `use … as _;` link.
        assert!(main_rs.contains("use my_app as _;"));
        assert!(main_rs.contains("pocopine_native_tauri::run!"));
        // Title defaults to the crate name.
        assert!(main_rs.contains(r#".title("my-app")"#));

        let cargo = std::fs::read_to_string(project.join("src-tauri/Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"my-app-native\""));
        assert!(cargo.contains("pocopine-native-tauri"));
        assert!(cargo.contains(r#"my-app = { path = ".." }"#));
        // Own workspace so it builds standalone inside any parent repo.
        assert!(cargo.contains("[workspace]"));

        let conf = std::fs::read_to_string(project.join("src-tauri/tauri.conf.json")).unwrap();
        assert!(conf.contains("\"identifier\": \"com.pocopine.my_app\""));

        // Re-running writes nothing new.
        let again = scaffold(project).unwrap();
        assert!(
            again.is_empty(),
            "scaffold must not overwrite existing files"
        );
    }
}
