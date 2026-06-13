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

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::args::{NativeArgs, NativeBuildArgs, NativeCmd, NativeDevArgs};
use crate::config::{self, NativeConfig, PocopineConfig};
use crate::{build, client_modules, stylekit, tailwind};

/// Environment variable the native shell reads for the dev static root.
/// Must match `pocopine_native::DEV_DIR_ENV` (RFC-104 contract); the CLI
/// does not depend on the native crate, so the string is duplicated here
/// deliberately.
const DEV_DIR_ENV: &str = "POCOPINE_NATIVE_DEV_DIR";

/// Environment variable carrying the selected channel's server backend
/// URL. When set, the native shell forwards `#[server]` calls there
/// (RFC-104 "server" channel); unset → standalone (in-process).
const BACKEND_ENV: &str = "POCOPINE_NATIVE_BACKEND";

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
    let backend = resolve_backend(native, args.channel.as_deref(), args.backend.as_deref())?;
    wasm_and_css(project, cfg, args.release, args.stylekit, args.no_stylekit)?;
    let src_tauri = project.join(&native.src_tauri);
    cargo_drive(
        &src_tauri,
        native,
        "run",
        args.release,
        Some(project),
        backend.as_deref(),
    )
}

fn build_native(
    project: &Path,
    cfg: &PocopineConfig,
    native: &NativeConfig,
    args: &NativeBuildArgs,
) -> Result<()> {
    ensure_src_tauri(project, native)?;
    let backend = resolve_backend(native, args.channel.as_deref(), args.backend.as_deref())?;
    let release = !args.debug;
    wasm_and_css(project, cfg, release, args.stylekit, args.no_stylekit)?;
    let src_tauri = project.join(&native.src_tauri);

    if !args.no_bundle && tauri_cli_available() {
        tauri_cli_build(&src_tauri, native, release, backend.as_deref())
    } else {
        if !args.no_bundle {
            println!(
                "ℹ tauri CLI not found; building the host binary only. \
                 Install it with `cargo install tauri-cli` to produce installers."
            );
        }
        cargo_drive(
            &src_tauri,
            native,
            "build",
            release,
            None,
            backend.as_deref(),
        )
    }
}

/// Resolve the server backend URL for this run: the `--backend` override
/// wins; otherwise the selected (or `default-channel`) channel's
/// `backend`; otherwise `None` (standalone, in-process). Errors if a
/// named channel doesn't exist.
fn resolve_backend(
    native: &NativeConfig,
    channel: Option<&str>,
    backend_override: Option<&str>,
) -> Result<Option<String>> {
    if let Some(url) = backend_override {
        return Ok(Some(normalize_backend(url)));
    }
    let Some(name) = channel.or(native.default_channel.as_deref()) else {
        return Ok(None);
    };
    let channel = native.channels.get(name).ok_or_else(|| {
        let known: Vec<&str> = native.channels.keys().map(String::as_str).collect();
        let known = if known.is_empty() {
            "(none defined)".to_string()
        } else {
            known.join(", ")
        };
        anyhow!("unknown native channel `{name}`. Defined channels: {known}")
    })?;
    Ok(channel.backend.as_deref().map(normalize_backend))
}

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

/// `cargo run`/`cargo build` the host crate in `src_tauri`. `dev_dir`,
/// when set, is exported as [`DEV_DIR_ENV`] so the shell serves the live
/// project directory.
fn cargo_drive(
    src_tauri: &Path,
    native: &NativeConfig,
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

fn tauri_cli_build(
    src_tauri: &Path,
    native: &NativeConfig,
    release: bool,
    backend: Option<&str>,
) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg("tauri").arg("build");
    if !release {
        cmd.arg("--debug");
    }
    if !native.features.is_empty() {
        cmd.arg("--features").arg(native.features.join(","));
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

/// Human-readable channel mode for build logs.
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

        let cargo = std::fs::read_to_string(project.join("src-tauri/Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"my-app-native\""));
        assert!(cargo.contains("pocopine-native-tauri"));
        assert!(cargo.contains(r#"my-app = { path = ".." }"#));
        // Own workspace so it builds standalone inside any parent repo.
        assert!(cargo.contains("[workspace]"));

        let conf = std::fs::read_to_string(project.join("src-tauri/tauri.conf.json")).unwrap();
        assert!(conf.contains("\"identifier\": \"com.pocopine.my_app\""));

        // Re-running writes nothing new.
        let again = scaffold(project, &native).unwrap();
        assert!(
            again.is_empty(),
            "scaffold must not overwrite existing files"
        );
    }

    fn channel(backend: Option<&str>) -> config::NativeChannel {
        config::NativeChannel {
            backend: backend.map(str::to_string),
        }
    }

    #[test]
    fn resolve_backend_is_standalone_by_default() {
        let native = NativeConfig::default();
        assert_eq!(resolve_backend(&native, None, None).unwrap(), None);
    }

    #[test]
    fn resolve_backend_uses_named_channel_and_trims_slash() {
        let mut native = NativeConfig::default();
        native
            .channels
            .insert("server".into(), channel(Some("https://api.example.com/")));
        assert_eq!(
            resolve_backend(&native, Some("server"), None)
                .unwrap()
                .as_deref(),
            Some("https://api.example.com"),
        );
    }

    #[test]
    fn resolve_backend_falls_back_to_default_channel() {
        let mut channels = std::collections::BTreeMap::new();
        channels.insert(
            "server".to_string(),
            channel(Some("https://api.example.com")),
        );
        let native = NativeConfig {
            channels,
            default_channel: Some("server".into()),
            ..NativeConfig::default()
        };
        assert_eq!(
            resolve_backend(&native, None, None).unwrap().as_deref(),
            Some("https://api.example.com"),
        );
    }

    #[test]
    fn resolve_backend_override_beats_channel() {
        let mut native = NativeConfig::default();
        native
            .channels
            .insert("server".into(), channel(Some("https://api.example.com")));
        assert_eq!(
            resolve_backend(&native, Some("server"), Some("http://localhost:3024/"))
                .unwrap()
                .as_deref(),
            Some("http://localhost:3024"),
        );
    }

    #[test]
    fn resolve_backend_channel_without_url_is_standalone() {
        let mut native = NativeConfig::default();
        native.channels.insert("offline".into(), channel(None));
        assert_eq!(
            resolve_backend(&native, Some("offline"), None).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_backend_unknown_channel_errors() {
        let native = NativeConfig::default();
        let err = resolve_backend(&native, Some("nope"), None).unwrap_err();
        assert!(err.to_string().contains("unknown native channel `nope`"));
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
