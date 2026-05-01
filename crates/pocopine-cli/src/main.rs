//! `pocopine` — project CLI.
//!
//! Three subcommands:
//!
//! * `build` — wraps `wasm-pack build --target web` (plus `cargo build
//!   --bin <name>` when a server binary is configured).
//! * `run`   — build once, then either spawn the configured server bin
//!   OR serve the project directory as static files.
//! * `dev`   — same routing as `run`, plus a file watcher that rebuilds
//!   the wasm bundle on src changes.
//!
//! Project config lives in the project's `Cargo.toml` under
//! `[package.metadata.pocopine]`. See
//! `examples/blog/Cargo.toml` for a complete server-bin example.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use pocopine_wasm_split::{Dependency, RouteSplitRoot};
use serde::Deserialize;
use wasmparser::{ExternalKind, Parser as WasmParser, Payload};

#[derive(Parser, Debug)]
#[command(name = "pocopine", about = "pocopine project CLI", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Build the wasm bundle (and the server bin, if configured).
    Build(BuildArgs),
    /// Remove generated browser artifacts from the project pkg directory.
    Clean(CleanArgs),
    /// Manage strict-layout route modules.
    Route(RouteArgs),
    /// Build, then serve. Spawns the configured server bin if one exists;
    /// otherwise serves the project directory as static files.
    Run(ServeArgs),
    /// Same as `run`, with src/ watched for changes that retrigger the
    /// wasm build.
    Dev(ServeArgs),
}

#[derive(Parser, Debug, Clone)]
struct BuildArgs {
    /// Path to the crate to build (defaults to current dir).
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Build in release mode.
    #[arg(long)]
    release: bool,
    /// Build `app!` routes as separate wasm artifacts.
    #[arg(long)]
    split: bool,
    /// Enforce split-ready shell/routes/shared ownership conventions.
    #[arg(long, action = clap::ArgAction::SetTrue, conflicts_with = "no_strict")]
    strict: bool,
    /// Disable split ownership enforcement.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_strict: bool,
}

#[derive(Parser, Debug, Clone)]
struct CleanArgs {
    /// Path to the crate to clean (defaults to current dir).
    #[arg(long, default_value = ".")]
    path: PathBuf,
}

#[derive(Parser, Debug, Clone)]
struct RouteArgs {
    #[command(subcommand)]
    cmd: RouteCmd,
}

#[derive(Subcommand, Debug, Clone)]
enum RouteCmd {
    /// Scaffold a strict-layout route module under src/routes/<name>.
    Add(RouteAddArgs),
}

#[derive(Parser, Debug, Clone)]
struct RouteAddArgs {
    /// Route module name, for example `story` or `admin_settings`.
    name: String,
    /// Path to the crate to update (defaults to current dir).
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Route pattern to add to app!, for example `/item/:id`.
    #[arg(long)]
    pattern: String,
    /// Component struct name. Defaults to PascalCase(name).
    #[arg(long)]
    component: Option<String>,
}

#[derive(Parser, Debug, Clone)]
struct ServeArgs {
    /// Path to the crate (defaults to current dir). Static files are
    /// served from this directory, and the server bin (if any) is spawned
    /// from here.
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Port to listen on in static mode. Ignored in server-bin mode —
    /// the bin controls its own addr. If the port is taken, the next
    /// available port is tried (up to `port + 20`).
    #[arg(long, default_value_t = 5243)]
    port: u16,
    /// Build in release mode.
    #[arg(long)]
    release: bool,
    /// Build `app!` routes as separate wasm artifacts.
    #[arg(long)]
    split: bool,
    /// Enforce split-ready shell/routes/shared ownership conventions.
    #[arg(long, action = clap::ArgAction::SetTrue, conflicts_with = "no_strict")]
    strict: bool,
    /// Disable split ownership enforcement.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_strict: bool,
}

/// `[package.metadata.pocopine]` section parsed from a project's
/// `Cargo.toml`. All fields optional — missing = "use defaults".
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct PocopineConfig {
    /// Name of the server binary to spawn in `run` / `dev`. When set,
    /// `pocopine` delegates serving entirely to this bin.
    bin: Option<String>,
    /// Advisory port shown in log output for server-bin mode. The bin
    /// binds whatever it wants; pocopine does not override it.
    #[allow(dead_code)]
    port: Option<u16>,
    /// Opt into bundled Tailwind. When present, `pocopine-cli` runs
    /// the Tailwind standalone CLI alongside `wasm-pack` — one-shot
    /// on `build`/`run`, watch mode on `dev`.
    tailwind: Option<TailwindConfig>,
}

/// `[package.metadata.pocopine.tailwind]` — configure the bundled
/// Tailwind build. All fields optional.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct TailwindConfig {
    /// Entry CSS passed to `tailwindcss -i`. Defaults to `app.css` at
    /// the project root.
    #[serde(default = "default_tw_input")]
    input: String,
    /// Output CSS path passed to `tailwindcss -o`. Defaults to
    /// `pkg/tailwind.css` so it sits alongside the wasm bundle.
    #[serde(default = "default_tw_output")]
    output: String,
    /// Release tag on `tailwindlabs/tailwindcss` to download when the
    /// binary isn't on `$PATH`. Defaults to [`DEFAULT_TW_VERSION`]. Only
    /// consumed when pocopine-cli is built for a host target.
    #[allow(dead_code)]
    #[serde(default = "default_tw_version")]
    version: String,
    /// Explicit path to a Tailwind binary. When set, skips `$PATH`
    /// lookup and auto-download entirely.
    binary: Option<PathBuf>,
}

impl Default for TailwindConfig {
    fn default() -> Self {
        Self {
            input: default_tw_input(),
            output: default_tw_output(),
            version: default_tw_version(),
            binary: None,
        }
    }
}

fn default_tw_input() -> String {
    "app.css".into()
}
fn default_tw_output() -> String {
    "pkg/tailwind.css".into()
}
fn default_tw_version() -> String {
    DEFAULT_TW_VERSION.into()
}

/// Tailwind standalone CLI version used when no `version` override is
/// set in the project config. `"latest"` resolves via GitHub's
/// `/releases/latest/download/` redirect, so we pick up new releases
/// without a code change. Users who need a reproducible build can pin
/// a concrete tag like `"v4.1.2"` in their `Cargo.toml`.
const DEFAULT_TW_VERSION: &str = "latest";

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build(a) => {
            let cfg = load_config(&a.path)?;
            build_entry(
                &a.path,
                a.release,
                a.split,
                split_strict(a.release, a.strict, a.no_strict),
            )?;
            if let Some(tw) = cfg.tailwind.as_ref() {
                let project = a.path.canonicalize()?;
                run_tailwind_once(&project, tw, a.release)?;
            }
            Ok(())
        }
        Cmd::Clean(a) => clean(&a.path),
        Cmd::Route(a) => route_cmd(a),
        Cmd::Run(a) => {
            let cfg = load_config(&a.path)?;
            build_entry(
                &a.path,
                a.release,
                a.split,
                split_strict(a.release, a.strict, a.no_strict),
            )?;
            if let Some(tw) = cfg.tailwind.as_ref() {
                let project = a.path.canonicalize()?;
                run_tailwind_once(&project, tw, a.release)?;
            }
            match cfg.bin.as_deref() {
                Some(bin) => spawn_bin(&a.path, bin, a.release)?.wait_for_exit(),
                None => serve_static(&a.path, a.port),
            }
        }
        Cmd::Dev(a) => dev(&a),
    }
}

// ---------- project config ----------

fn load_config(path: &Path) -> Result<PocopineConfig> {
    let resolved = path
        .canonicalize()
        .with_context(|| format!("resolve project path: {}", path.display()))?;
    let manifest_path = resolved.join("Cargo.toml");
    if !manifest_path.exists() {
        bail!("no Cargo.toml at {}", manifest_path.display());
    }
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;

    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        package: Package,
    }
    #[derive(Default, Deserialize)]
    struct Package {
        #[serde(default)]
        metadata: Metadata,
    }
    #[derive(Default, Deserialize)]
    struct Metadata {
        #[serde(default)]
        pocopine: PocopineConfig,
    }

    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parse {}", manifest_path.display()))?;
    Ok(manifest.package.metadata.pocopine)
}

// ---------- tailwind ----------

/// A running Tailwind watcher child. Dropped via [`TailwindChild::kill`]
/// on CLI exit so the process doesn't outlive us.
struct TailwindChild {
    child: Child,
}

impl TailwindChild {
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Resolve the Tailwind binary: first explicit config path, then
/// `$PATH`, then `<target>/pocopine/bin/tailwindcss(.exe)` — downloading
/// it from GitHub Releases if absent. Uses `version` from the config
/// so projects can pin upstream independently.
fn ensure_tailwind_binary(project: &Path, tw: &TailwindConfig) -> Result<PathBuf> {
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
        return Ok(resolved);
    }

    if let Some(found) = which("tailwindcss") {
        return Ok(found);
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
        return Ok(bin_path);
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
        Ok(bin_path)
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
            "no Tailwind standalone binary known for {os}/{arch} — set \
             `pocopine.tailwind.binary` to a local path"
        ),
    }
}

/// Minimal `which` — walks `$PATH` for an executable by that name.
fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let with_ext = dir.join(format!("{name}.exe"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

/// Run Tailwind once (used by `build` and `run`). `release` enables
/// `--minify`.
fn run_tailwind_once(project: &Path, tw: &TailwindConfig, release: bool) -> Result<()> {
    let bin = ensure_tailwind_binary(project, tw)?;
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
    let mut cmd = Command::new(&bin);
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
fn spawn_tailwind_watch(project: &Path, tw: &TailwindConfig) -> Result<TailwindChild> {
    let bin = ensure_tailwind_binary(project, tw)?;
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
    let child = Command::new(&bin)
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

// ---------- build ----------

fn build(path: &Path, release: bool) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("could not resolve project path: {}", path.display()))?;
    println!("▶ wasm-pack build ({})", path.display());
    let mut cmd = Command::new("wasm-pack");
    cmd.arg("build").arg("--target").arg("web");
    if release {
        cmd.arg("--release");
    } else {
        cmd.arg("--dev");
    }
    cmd.current_dir(&path);
    let status = cmd
        .status()
        .context("failed to invoke wasm-pack (is it on $PATH?)")?;
    if !status.success() {
        bail!("wasm-pack build failed with status {status}");
    }
    Ok(())
}

fn clean(path: &Path) -> Result<()> {
    let project = path
        .canonicalize()
        .with_context(|| format!("could not resolve project path: {}", path.display()))?;
    let pkg = project.join("pkg");
    if !pkg.exists() {
        println!("✓ nothing to clean: {}", pkg.display());
        return Ok(());
    }
    if !pkg.is_dir() {
        bail!("refusing to clean non-directory {}", pkg.display());
    }
    std::fs::remove_dir_all(&pkg).with_context(|| format!("remove {}", pkg.display()))?;
    println!("✓ cleaned {}", pkg.display());
    Ok(())
}

fn route_cmd(args: RouteArgs) -> Result<()> {
    match args.cmd {
        RouteCmd::Add(args) => route_add(args),
    }
}

fn route_add(args: RouteAddArgs) -> Result<()> {
    let project = args
        .path
        .canonicalize()
        .with_context(|| format!("could not resolve project path: {}", args.path.display()))?;
    let route_name = normalize_route_module_name(&args.name)?;
    let component = args
        .component
        .as_deref()
        .map(validate_component_ident)
        .transpose()?
        .unwrap_or_else(|| pascal_case(&route_name));
    let routes_dir = project.join("src").join("routes");
    let route_dir = routes_dir.join(&route_name);
    if route_dir.exists() {
        bail!("route module already exists: {}", route_dir.display());
    }

    std::fs::create_dir_all(&route_dir)
        .with_context(|| format!("create {}", route_dir.display()))?;
    ensure_routes_mod(&routes_dir, &route_name)?;

    let mod_rs = format!(
        r#"use pocopine::prelude::*;
use serde::{{Deserialize, Serialize}};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct {component} {{}}

#[handlers]
impl {component} {{}}
"#
    );
    std::fs::write(route_dir.join("mod.rs"), mod_rs)
        .with_context(|| format!("write {}", route_dir.join("mod.rs").display()))?;
    let title = title_case(&route_name);
    let template = format!("<section>\n  <h1>{title}</h1>\n</section>\n");
    std::fs::write(route_dir.join(format!("{component}.poco")), template).with_context(|| {
        format!(
            "write {}",
            route_dir.join(format!("{component}.poco")).display()
        )
    })?;

    println!("✓ created route module src/routes/{route_name}");
    println!("Add to `pocopine::app!` components:");
    println!("    crate::routes::{route_name}::{component},");
    println!("Add to `pocopine::app!` routes:");
    println!(
        "    (\"{}\", crate::routes::{route_name}::{component}),",
        args.pattern
    );
    Ok(())
}

fn ensure_routes_mod(routes_dir: &Path, route_name: &str) -> Result<()> {
    std::fs::create_dir_all(routes_dir)
        .with_context(|| format!("create {}", routes_dir.display()))?;
    let mod_path = routes_dir.join("mod.rs");
    let line = format!("pub mod {route_name};");
    if !mod_path.exists() {
        std::fs::write(
            &mod_path,
            format!("//! Route-private component clusters.\n\n{line}\n"),
        )
        .with_context(|| format!("write {}", mod_path.display()))?;
        return Ok(());
    }
    let text = std::fs::read_to_string(&mod_path)
        .with_context(|| format!("read {}", mod_path.display()))?;
    if text.lines().any(|existing| existing.trim() == line) {
        return Ok(());
    }
    let mut next = text;
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&line);
    next.push('\n');
    std::fs::write(&mod_path, next).with_context(|| format!("write {}", mod_path.display()))?;
    Ok(())
}

fn normalize_route_module_name(raw: &str) -> Result<String> {
    let name = raw.trim().replace('-', "_");
    if is_valid_snake_ident(&name) {
        Ok(name)
    } else {
        bail!("route name `{raw}` must be a Rust module identifier: use snake_case or kebab-case");
    }
}

fn validate_component_ident(raw: &str) -> Result<String> {
    let ident = raw.trim();
    if is_valid_pascal_ident(ident) {
        Ok(ident.to_string())
    } else {
        bail!("component `{raw}` must be a PascalCase Rust type identifier");
    }
}

fn is_valid_snake_ident(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch == '_' || ch.is_ascii_lowercase())
        && chars.all(|ch| ch == '_' || ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

fn is_valid_pascal_ident(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_uppercase())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn pascal_case(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            let mut out = String::new();
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
            out
        })
        .collect()
}

fn title_case(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            let mut out = String::new();
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_strict(release: bool, strict: bool, no_strict: bool) -> bool {
    strict || (release && !no_strict)
}

fn build_entry(path: &Path, release: bool, split: bool, strict: bool) -> Result<()> {
    if split {
        build_split(path, release, strict)
    } else {
        build(path, release)
    }
}

fn package_name(path: &Path) -> Result<String> {
    let manifest_path = path.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;

    #[derive(Deserialize)]
    struct Manifest {
        package: PackageName,
    }
    #[derive(Deserialize)]
    struct PackageName {
        name: String,
    }

    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parse {}", manifest_path.display()))?;
    Ok(manifest.package.name.replace('-', "_"))
}

fn build_split(path: &Path, release: bool, strict: bool) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("could not resolve project path: {}", path.display()))?;
    let base = package_name(&path)?;
    println!("▶ split wasm-pack build ({})", path.display());
    clean_split_artifacts(&path, &base)?;
    let build_id = split_build_id()?;
    let ctx = SplitBuildCtx {
        release,
        base: &base,
        build_id: &build_id,
        strict,
    };
    let route_ids_path = std::env::temp_dir().join(format!("{base}-pocopine-route-ids.txt"));
    let _ = std::fs::remove_file(&route_ids_path);
    run_split_wasm_pack(&path, &ctx, "shell", &base, Some(&route_ids_path))?;

    let route_ids_text = std::fs::read_to_string(&route_ids_path)
        .with_context(|| format!("read {}", route_ids_path.display()))?;
    let route_ids = route_ids_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if route_ids.is_empty() {
        bail!("split build found no routes; use plain `pocopine build` for non-routed apps");
    }

    emit_post_link_split_chunks(&path, &base, &route_ids)?;

    for route_id in &route_ids {
        if write_descriptor_route_if_static(&path, &base, route_id)? {
            continue;
        }
        write_post_link_route_module(&path, &base, route_id)?;
    }
    write_split_loader(&path, &base)?;
    let route_count = route_ids.len();
    println!("✓ split build emitted shell + {route_count} route artifact(s)");
    Ok(())
}

fn write_post_link_route_module(path: &Path, base: &str, route_id: &str) -> Result<()> {
    let mount_export = format!("pocopine_route_mount_{route_id}");
    let unmount_export = format!("pocopine_route_unmount_{route_id}");
    let module = format!(
        r#"export const chunk = "/pkg/.pocopine-split/route_{route_id}.wasm";
const MOUNT_EXPORT = {mount_export_json};
const UNMOUNT_EXPORT = {unmount_export_json};
let instance = null;
let encoder = new TextEncoder();
let vectorLen = 0;

function passStringToShellWasm(value, shell) {{
  const bytes = encoder.encode(value);
  const ptr = shell.__wbindgen_malloc(bytes.length, 1);
  new Uint8Array(shell.memory.buffer).set(bytes, ptr);
  vectorLen = bytes.length;
  return ptr;
}}

export default async function init() {{
  if (instance) return instance;
  const shell = window.__pocopine_shell?.__pocopine_split_exports?.();
  if (!shell) {{
    throw new Error("pocopine split route loaded before shell exports were ready");
  }}
  const imports = {{ "pocopine:split": shell }};
  const response = fetch(chunk);
  if (WebAssembly.instantiateStreaming) {{
    try {{
      const result = await WebAssembly.instantiateStreaming(response, imports);
      instance = result.instance;
      return instance;
    }} catch (error) {{
      if (!String(error).includes("Content-Type")) throw error;
    }}
  }}
  const bytes = await (await response).arrayBuffer();
  const result = await WebAssembly.instantiate(bytes, imports);
  instance = result.instance;
  return instance;
}}

export function unmount_pocopine_route() {{
  instance?.exports?.[UNMOUNT_EXPORT]?.();
}}

export async function mount_pocopine_route(outlet, path) {{
  await init();
  const shell = window.__pocopine_shell.__pocopine_split_exports();
  const ptr = passStringToShellWasm(path, shell);
  instance.exports[MOUNT_EXPORT](outlet, ptr, vectorLen);
}}
"#,
        route_id = route_id,
        mount_export_json = js_string(&mount_export),
        unmount_export_json = js_string(&unmount_export),
    );
    let out = path.join("pkg").join(format!("{base}_route_{route_id}.js"));
    std::fs::write(&out, module).with_context(|| format!("write {}", out.display()))?;
    println!("  post-link route {route_id} -> {}", out.display());
    Ok(())
}

fn write_descriptor_route_if_static(path: &Path, base: &str, route_id: &str) -> Result<bool> {
    let route_dir = path.join("src").join("routes").join(route_id);
    if !route_dir.is_dir() {
        return Ok(false);
    }
    let templates = std::fs::read_dir(&route_dir)
        .with_context(|| format!("read {}", route_dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "poco"))
        .collect::<Vec<_>>();
    let [template_path] = templates.as_slice() else {
        return Ok(false);
    };
    let html = std::fs::read_to_string(template_path)
        .with_context(|| format!("read {}", template_path.display()))?;
    if !is_static_descriptor_template(&html) {
        return Ok(false);
    }
    let Some(stem) = template_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(false);
    };
    let tag = kebab_case(stem);
    let module = format!(
        r#"const TAG = {tag};
const HTML = {html};
let registered = false;

export default async function init() {{}}

export function unmount_pocopine_route() {{}}

export function mount_pocopine_route(outlet, _path) {{
  if (!registered) {{
    registered = true;
    window.__pocopine_shell.pocopine_host_register_static_component(TAG, HTML);
  }}
  window.__pocopine_shell.pocopine_host_mount_static_component(outlet, TAG);
}}
"#,
        tag = js_string(&tag),
        html = js_string(&html),
    );
    let out = path.join("pkg").join(format!("{base}_route_{route_id}.js"));
    std::fs::write(&out, module).with_context(|| format!("write {}", out.display()))?;
    println!("  descriptor route {route_id} -> {}", out.display());
    Ok(true)
}

fn is_static_descriptor_template(html: &str) -> bool {
    const DYNAMIC_MARKERS: &[&str] = &[
        "pp-text",
        "pp-html",
        "pp-for",
        "pp-if",
        "pp-bind",
        "pp-model",
        "pp-on",
        "pp-ref",
        "pp-show",
        "pp-transition",
        "pp-teleport",
        "pp-anchor",
        "pp-resize",
        "pp-intersect",
        "pp-roving",
        "{{",
    ];
    !DYNAMIC_MARKERS.iter().any(|marker| html.contains(marker))
        && !contains_binding_attr_prefix(html, ':')
        && !contains_binding_attr_prefix(html, '@')
}

fn contains_binding_attr_prefix(html: &str, prefix: char) -> bool {
    let mut prev = None;
    for ch in html.chars() {
        if ch == prefix && matches!(prev, Some(' ' | '\n' | '\r' | '\t')) {
            return true;
        }
        prev = Some(ch);
    }
    false
}

fn kebab_case(value: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if idx != 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' {
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out
}

fn js_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

struct SplitBuildCtx<'a> {
    release: bool,
    base: &'a str,
    build_id: &'a str,
    strict: bool,
}

fn run_split_wasm_pack(
    path: &Path,
    ctx: &SplitBuildCtx<'_>,
    mode: &str,
    out_name: &str,
    route_ids_out: Option<&Path>,
) -> Result<()> {
    let mut cmd = Command::new("wasm-pack");
    cmd.arg("build")
        .arg("--target")
        .arg("web")
        .arg("--out-dir")
        .arg("pkg")
        .arg("--out-name")
        .arg(out_name);
    if ctx.release {
        cmd.arg("--release");
    } else {
        cmd.arg("--dev");
    }
    cmd.env("POCOPINE_SPLIT_MODE", mode)
        .env("POCOPINE_SPLIT_BASE", ctx.base)
        .env("POCOPINE_SPLIT_STRICT", if ctx.strict { "1" } else { "0" })
        .current_dir(path);
    if let Some(path) = route_ids_out {
        cmd.env("POCOPINE_SPLIT_ROUTE_IDS_OUT", path);
    }
    let cfg_name = format!("pocopine_split_{}_{}", split_cfg_suffix(mode), ctx.build_id);
    let rustflags = match std::env::var("RUSTFLAGS") {
        Ok(existing) if !existing.trim().is_empty() => format!("{existing} --cfg={cfg_name}"),
        _ => format!("--cfg={cfg_name}"),
    };
    cmd.env("RUSTFLAGS", rustflags);

    let status = cmd
        .status()
        .context("failed to invoke wasm-pack (is it on $PATH?)")?;
    if !status.success() {
        bail!("wasm-pack split build `{mode}` failed with status {status}");
    }
    Ok(())
}

fn split_build_id() -> Result<String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?;
    Ok(format!("{}", elapsed.as_nanos()))
}

fn split_cfg_suffix(mode: &str) -> String {
    mode.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn clean_split_artifacts(path: &Path, base: &str) -> Result<()> {
    let pkg = path.join("pkg");
    if !pkg.exists() {
        return Ok(());
    }
    let route_prefix = format!("{base}_route_");
    for entry in std::fs::read_dir(&pkg).with_context(|| format!("read {}", pkg.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", pkg.display()))?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name == ".pocopine-split" {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)
                    .with_context(|| format!("remove stale split directory {}", path.display()))?;
            }
            continue;
        }
        if file_name.starts_with(&route_prefix) || file_name == "pocopine-split-loader.js" {
            let path = entry.path();
            if path.is_file() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("remove stale split artifact {}", path.display()))?;
            }
        }
    }
    Ok(())
}

fn emit_post_link_split_chunks(path: &Path, base: &str, route_ids: &[String]) -> Result<()> {
    let pkg = path.join("pkg");
    let wasm_path = pkg.join(format!("{base}_bg.wasm"));
    let wasm =
        std::fs::read(&wasm_path).with_context(|| format!("read {}", wasm_path.display()))?;
    let module = pocopine_wasm_split::analyze(&wasm)
        .with_context(|| format!("analyze {}", wasm_path.display()))?;
    module
        .validate_indices()
        .map_err(|errors| anyhow!("split input validation failed: {errors:?}"))?;

    let routes = route_ids
        .iter()
        .map(|route_id| {
            let mount = format!("pocopine_route_mount_{route_id}");
            let unmount = format!("pocopine_route_unmount_{route_id}");
            let mount_function = exported_function(&module, &mount).ok_or_else(|| {
                anyhow!(
                    "split route mount `{mount}` was not exported by {}",
                    wasm_path.display()
                )
            })?;
            let unmount_function = exported_function(&module, &unmount).ok_or_else(|| {
                anyhow!(
                    "split route unmount `{unmount}` was not exported by {}",
                    wasm_path.display()
                )
            })?;
            Ok(RouteSplitRoot {
                name: route_id.clone(),
                roots: vec![
                    Dependency::Function(mount_function),
                    Dependency::Function(unmount_function),
                ],
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let route_markers = route_ids
        .iter()
        .flat_map(|route_id| {
            [
                format!("pocopine_route_mount_{route_id}"),
                format!("pocopine_route_unmount_{route_id}"),
            ]
        })
        .collect::<std::collections::BTreeSet<_>>();
    let shell_roots = module
        .exports
        .iter()
        .filter(|export| {
            export.kind == ExternalKind::Func
                && !route_markers.contains(&export.name)
                && !export.name.starts_with("pocopine_route_mount_")
                && !export.name.starts_with("pocopine_route_unmount_")
        })
        .map(|export| Dependency::Function(export.index))
        .collect::<Vec<_>>();
    if shell_roots.is_empty() {
        bail!(
            "split shell build exported no shell function roots in {}",
            wasm_path.display()
        );
    }

    let plan = module
        .plan_route_split(shell_roots, &routes)
        .map_err(|error| anyhow!("plan post-link route split: {error:?}"))?;
    let links = module.build_link_plan(&plan);
    module
        .validate_link_plan(&links)
        .map_err(|errors| anyhow!("validate post-link route split: {errors:?}"))?;
    export_split_host_aliases(&wasm_path, &wasm, &module, &links)?;
    patch_shell_js_split_exports(path, base)?;

    let split_dir = pkg.join(".pocopine-split");
    if split_dir.exists() {
        std::fs::remove_dir_all(&split_dir)
            .with_context(|| format!("remove {}", split_dir.display()))?;
    }
    std::fs::create_dir_all(&split_dir)
        .with_context(|| format!("create {}", split_dir.display()))?;

    write_split_chunk(&module, &links.shell, &split_dir.join("shell.wasm"))?;
    for route in &links.routes {
        write_split_chunk(
            &module,
            route,
            &split_dir.join(format!("route_{}.wasm", route.name)),
        )?;
    }
    for shared in &links.shared {
        let name = shared
            .name
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        write_split_chunk(&module, shared, &split_dir.join(format!("{name}.wasm")))?;
    }

    println!(
        "  post-link split chunks -> {} (shell + {} route + {} shared)",
        split_dir.display(),
        links.routes.len(),
        links.shared.len()
    );
    Ok(())
}

fn exported_function(module: &pocopine_wasm_split::ModuleAnalysis, name: &str) -> Option<u32> {
    module
        .exports
        .iter()
        .find(|export| export.kind == ExternalKind::Func && export.name == name)
        .map(|export| export.index)
}

fn export_split_host_aliases(
    wasm_path: &Path,
    wasm: &[u8],
    module: &pocopine_wasm_split::ModuleAnalysis,
    links: &pocopine_wasm_split::SplitLinkPlan,
) -> Result<()> {
    let mut aliases = BTreeSet::new();
    for chunk in links.routes.iter().chain(links.shared.iter()) {
        for dependency in &chunk.external {
            if module.imports.contains(dependency) {
                continue;
            }
            if split_export_kind(*dependency).is_some() {
                aliases.insert(*dependency);
            }
        }
    }
    if aliases.is_empty() {
        return Ok(());
    }

    let patched = add_export_aliases(wasm, &aliases)?;
    wasmparser::Validator::new()
        .validate_all(&patched)
        .with_context(|| format!("validate split host aliases in {}", wasm_path.display()))?;
    std::fs::write(wasm_path, patched)
        .with_context(|| format!("write split host aliases to {}", wasm_path.display()))?;
    Ok(())
}

fn split_export_kind(dependency: Dependency) -> Option<wasm_encoder::ExportKind> {
    match dependency {
        Dependency::Function(_) => Some(wasm_encoder::ExportKind::Func),
        Dependency::Table(_) => Some(wasm_encoder::ExportKind::Table),
        Dependency::Memory(_) => Some(wasm_encoder::ExportKind::Memory),
        Dependency::Global(_) => Some(wasm_encoder::ExportKind::Global),
        Dependency::Tag(_) => Some(wasm_encoder::ExportKind::Tag),
        Dependency::Type(_) | Dependency::Data(_) | Dependency::Element(_) => None,
    }
}

fn split_export_name(dependency: Dependency) -> Option<String> {
    match dependency {
        Dependency::Function(index) => Some(format!("func:{index}")),
        Dependency::Table(index) => Some(format!("table:{index}")),
        Dependency::Memory(index) => Some(format!("memory:{index}")),
        Dependency::Global(index) => Some(format!("global:{index}")),
        Dependency::Tag(index) => Some(format!("tag:{index}")),
        Dependency::Type(_) | Dependency::Data(_) | Dependency::Element(_) => None,
    }
}

fn add_export_aliases(wasm: &[u8], aliases: &BTreeSet<Dependency>) -> Result<Vec<u8>> {
    let mut module_out = wasm_encoder::Module::new();
    let mut export_section_seen = false;

    for payload in WasmParser::new(0).parse_all(wasm) {
        let payload = payload.context("parse wasm for split export aliases")?;
        match payload {
            Payload::Version { .. } | Payload::CodeSectionEntry(_) => {}
            Payload::ExportSection(reader) => {
                export_section_seen = true;
                let mut exports = wasm_encoder::ExportSection::new();
                let mut names = BTreeSet::new();
                for export in reader {
                    let export = export.context("read wasm export")?;
                    names.insert(export.name.to_string());
                    exports.export(export.name, reencode_export_kind(export.kind), export.index);
                }
                for dependency in aliases {
                    let Some(name) = split_export_name(*dependency) else {
                        continue;
                    };
                    if names.contains(&name) {
                        continue;
                    }
                    let Some(kind) = split_export_kind(*dependency) else {
                        continue;
                    };
                    exports.export(&name, kind, dependency_index(*dependency));
                }
                module_out.section(&exports);
            }
            other => {
                if let Some((id, range)) = other.as_section() {
                    module_out.section(&wasm_encoder::RawSection {
                        id,
                        data: &wasm[range],
                    });
                }
            }
        }
    }

    if !export_section_seen {
        bail!("split host alias patch requires an existing export section");
    }

    Ok(module_out.finish())
}

fn reencode_export_kind(kind: ExternalKind) -> wasm_encoder::ExportKind {
    match kind {
        ExternalKind::Func => wasm_encoder::ExportKind::Func,
        ExternalKind::Table => wasm_encoder::ExportKind::Table,
        ExternalKind::Memory => wasm_encoder::ExportKind::Memory,
        ExternalKind::Global => wasm_encoder::ExportKind::Global,
        ExternalKind::Tag => wasm_encoder::ExportKind::Tag,
    }
}

fn dependency_index(dependency: Dependency) -> u32 {
    match dependency {
        Dependency::Function(index)
        | Dependency::Table(index)
        | Dependency::Memory(index)
        | Dependency::Global(index)
        | Dependency::Tag(index)
        | Dependency::Type(index)
        | Dependency::Data(index)
        | Dependency::Element(index) => index,
    }
}

fn patch_shell_js_split_exports(path: &Path, base: &str) -> Result<()> {
    let js_path = path.join("pkg").join(format!("{base}.js"));
    let mut js =
        std::fs::read_to_string(&js_path).with_context(|| format!("read {}", js_path.display()))?;
    if js.contains("function __pocopine_split_exports()") {
        return Ok(());
    }
    let marker = "export { initSync, __wbg_init as default };";
    let Some(pos) = js.rfind(marker) else {
        bail!(
            "could not find wasm-bindgen export marker in {}",
            js_path.display()
        );
    };
    js.insert_str(
        pos,
        "export function __pocopine_split_exports() {\n    return wasm;\n}\n\n",
    );
    std::fs::write(&js_path, js).with_context(|| format!("write {}", js_path.display()))?;
    Ok(())
}

fn write_split_chunk(
    module: &pocopine_wasm_split::ModuleAnalysis,
    chunk: &pocopine_wasm_split::ChunkLinkPlan,
    path: &Path,
) -> Result<()> {
    let wasm = module
        .emit_function_chunk(chunk)
        .with_context(|| format!("emit post-link split chunk `{}`", chunk.name))?;
    wasmparser::Validator::new()
        .validate_all(&wasm)
        .with_context(|| format!("validate post-link split chunk `{}`", chunk.name))?;
    std::fs::write(path, wasm).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn write_split_loader(path: &Path, base: &str) -> Result<()> {
    let pkg = path.join("pkg");
    std::fs::create_dir_all(&pkg).with_context(|| format!("create {}", pkg.display()))?;
    let loader = format!(
        r#"let activeRoute = null;

function routeMatches(pattern, path) {{
  if (pattern === "*") return true;
  const patternParts = pattern.split("/").filter(Boolean);
  const pathParts = path.split("/").filter(Boolean);
  if (patternParts.length !== pathParts.length) return false;
  return patternParts.every((part, idx) => part.startsWith(":") || part === pathParts[idx]);
}}

function matchRoute(routes, path) {{
  return routes.find((route) => route.pattern !== "*" && routeMatches(route.pattern, path))
      || routes.find((route) => route.pattern === "*");
}}

async function mountCurrentRoute(routes) {{
  const outlet = document.querySelector("pp-outlet");
  if (!outlet) return;
  if (activeRoute?.unmount_pocopine_route) {{
    activeRoute.unmount_pocopine_route();
  }}
  const route = matchRoute(routes, location.pathname);
  if (!route) return;
  const mod = await import(route.module);
  await mod.default();
  await mod.mount_pocopine_route(outlet, location.pathname);
  activeRoute = mod;
}}

export async function startPocopineSplitApp() {{
  const shell = await import("/pkg/{base}.js");
  await shell.default();
  window.__pocopine_shell = shell;
  const manifest = JSON.parse(shell.pocopine_split_manifest());
  document.addEventListener("click", (event) => {{
    const anchor = event.target.closest?.("a[pp-route]");
    if (!anchor) return;
    const url = new URL(anchor.getAttribute("href"), location.href);
    if (url.origin !== location.origin) return;
    event.preventDefault();
    if (url.pathname === location.pathname && url.search === location.search) return;
    history.pushState(null, "", url);
    mountCurrentRoute(manifest.routes);
  }});
  window.addEventListener("popstate", () => mountCurrentRoute(manifest.routes));
  await mountCurrentRoute(manifest.routes);
}}
"#
    );
    std::fs::write(pkg.join("pocopine-split-loader.js"), loader)
        .with_context(|| format!("write {}", pkg.join("pocopine-split-loader.js").display()))?;
    Ok(())
}

// ---------- server-bin spawn ----------

struct BinChild {
    child: Child,
    bin: String,
}

impl BinChild {
    fn wait_for_exit(mut self) -> Result<()> {
        let status = self.child.wait().context("waiting on server bin")?;
        if !status.success() {
            bail!("server bin `{}` exited with {status}", self.bin);
        }
        Ok(())
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_bin(path: &Path, bin: &str, release: bool) -> Result<BinChild> {
    let project = path
        .canonicalize()
        .with_context(|| format!("resolve {}", path.display()))?;
    let mut cmd = Command::new("cargo");
    cmd.arg("run").arg("--bin").arg(bin);
    if release {
        cmd.arg("--release");
    }
    cmd.current_dir(&project);
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    println!(
        "▶ spawning `{bin}` (cargo run --bin {bin} in {})",
        project.display()
    );
    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn server bin `{bin}`"))?;
    Ok(BinChild {
        child,
        bin: bin.into(),
    })
}

// ---------- static serving ----------

fn serve_static(path: &Path, port: u16) -> Result<()> {
    let root = path
        .canonicalize()
        .with_context(|| format!("bad serve dir: {}", path.display()))?;
    let (server, bound) = bind_port(port)?;
    if bound != port {
        eprintln!("port {port} unavailable, using {bound}");
    }
    println!("✓ serving {} at http://localhost:{bound}", root.display());
    for request in server.incoming_requests() {
        handle(&root, request);
    }
    Ok(())
}

fn bind_port(start: u16) -> Result<(tiny_http::Server, u16)> {
    const ATTEMPTS: u16 = 21;
    let mut last_err: Option<String> = None;
    for offset in 0..ATTEMPTS {
        let Some(port) = start.checked_add(offset) else {
            break;
        };
        let addr = format!("0.0.0.0:{port}");
        match tiny_http::Server::http(&addr) {
            Ok(s) => return Ok((s, port)),
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    Err(anyhow!(
        "could not bind any port in [{start}, {}]: {}",
        start.saturating_add(ATTEMPTS - 1),
        last_err.unwrap_or_default()
    ))
}

fn handle(root: &Path, request: tiny_http::Request) {
    let url = request.url().to_string();
    let rel = url.split('?').next().unwrap_or("/").trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };

    let candidate = root.join(rel);
    let looks_like_asset = looks_like_asset_path(rel);

    let canonical = candidate
        .canonicalize()
        .ok()
        .filter(|p| p.starts_with(root));

    // Serve the resolved path when it exists …
    if let Some(canonical) = canonical {
        let target = if canonical.is_dir() {
            canonical.join("index.html")
        } else {
            canonical
        };
        if let Ok(body) = std::fs::read(&target) {
            let mime = mime_of(&target);
            let header =
                tiny_http::Header::from_bytes(&b"Content-Type"[..], mime.as_bytes()).unwrap();
            let _ = request.respond(tiny_http::Response::from_data(body).with_header(header));
            return;
        }
    }

    // … otherwise fall back to the root's index.html for *non-asset* paths
    // (SPA history-fallback). Asset-looking paths (anything with a file
    // extension in the last segment) 404 so bad imports aren't masked.
    if !looks_like_asset {
        let fallback = root.join("index.html");
        if let Ok(body) = std::fs::read(&fallback) {
            let header = tiny_http::Header::from_bytes(
                &b"Content-Type"[..],
                &b"text/html; charset=utf-8"[..],
            )
            .unwrap();
            let _ = request.respond(tiny_http::Response::from_data(body).with_header(header));
            return;
        }
    }

    let _ = request.respond(tiny_http::Response::from_string("not found").with_status_code(404));
}

/// True when the last URL segment has a file extension. Used to decide
/// whether an unmatched path should 404 or fall back to index.html:
/// `/pkg/spa.js` → 404, `/blog/42` → index.html.
fn looks_like_asset_path(rel: &str) -> bool {
    let last = rel.rsplit('/').next().unwrap_or("");
    last.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
}

fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

// ---------- dev (watch + rebuild) ----------

fn dev(args: &ServeArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = load_config(&args.path)?;
    let strict = split_strict(args.release, args.strict, args.no_strict);
    build_entry(&project, args.release, args.split, strict)?;

    // Kick off Tailwind in watch mode *before* we start serving so
    // the first page load already sees compiled CSS.
    let tailwind_child = if let Some(tw) = cfg.tailwind.as_ref() {
        // One-shot pre-build so pkg/tailwind.css exists before the
        // watcher spins up and before the first HTTP request lands.
        run_tailwind_once(&project, tw, args.release)?;
        Some(spawn_tailwind_watch(&project, tw)?)
    } else {
        None
    };

    // Start the serving side. In bin mode the child owns its ports + routes.
    // In static mode the CLI owns the socket and runs on a background thread.
    let bin_child = match cfg.bin.as_deref() {
        Some(bin) => Some(spawn_bin(&project, bin, args.release)?),
        None => {
            let serve_path = project.clone();
            let port = args.port;
            thread::spawn(move || {
                if let Err(e) = serve_static(&serve_path, port) {
                    eprintln!("server error: {e}");
                }
            });
            None
        }
    };

    let (tx, rx) = channel::<()>();
    let tx_w = tx.clone();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                use notify::EventKind::*;
                if matches!(ev.kind, Modify(_) | Create(_) | Remove(_)) {
                    let _ = tx_w.send(());
                }
            }
        })?;
    let src_dir = project.join("src");
    watcher.watch(&src_dir, RecursiveMode::Recursive)?;
    println!("👀 watching {}", src_dir.display());

    // Handle Ctrl-C so the spawned server bin gets cleaned up.
    let result = loop {
        if rx.recv().is_err() {
            break Ok(());
        }
        thread::sleep(Duration::from_millis(250));
        while rx.try_recv().is_ok() {}

        println!("↻ rebuilding wasm…");
        if let Err(e) = build_entry(&project, args.release, args.split, strict) {
            eprintln!("build failed: {e:#}");
        }
    };

    if let Some(child) = bin_child {
        child.kill();
    }
    if let Some(child) = tailwind_child {
        child.kill();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::is_static_descriptor_template;

    #[test]
    fn static_descriptor_allows_static_route_links_and_urls() {
        let html = r#"<section>
  <a href="https://example.com/@team" pp-route>Read more</a>
</section>"#;

        assert!(is_static_descriptor_template(html));
    }

    #[test]
    fn static_descriptor_rejects_binding_attributes() {
        assert!(!is_static_descriptor_template(
            r#"<button :disabled="loading">Save</button>"#
        ));
        assert!(!is_static_descriptor_template(
            r#"<button @click="save">Save</button>"#
        ));
    }

    #[test]
    fn static_descriptor_rejects_runtime_directives() {
        assert!(!is_static_descriptor_template(
            r#"<span pp-text="title"></span>"#
        ));
        assert!(!is_static_descriptor_template(
            r#"<template pp-if="ready"><p>Ready</p></template>"#
        ));
    }
}
