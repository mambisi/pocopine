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

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;

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
            build_entry(&a.path, a.release, a.split)?;
            if let Some(tw) = cfg.tailwind.as_ref() {
                let project = a.path.canonicalize()?;
                run_tailwind_once(&project, tw, a.release)?;
            }
            Ok(())
        }
        Cmd::Run(a) => {
            let cfg = load_config(&a.path)?;
            build_entry(&a.path, a.release, a.split)?;
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

fn build_entry(path: &Path, release: bool, split: bool) -> Result<()> {
    if split {
        build_split(path, release)
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

fn build_split(path: &Path, release: bool) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("could not resolve project path: {}", path.display()))?;
    let base = package_name(&path)?;
    println!("▶ split wasm-pack build ({})", path.display());
    clean_split_artifacts(&path, &base)?;
    let build_id = split_build_id()?;
    let route_count_path = std::env::temp_dir().join(format!("{base}-pocopine-routes.txt"));
    let _ = std::fs::remove_file(&route_count_path);
    run_split_wasm_pack(
        &path,
        release,
        &base,
        "shell",
        &base,
        Some(&route_count_path),
        &build_id,
    )?;

    let route_count_text = std::fs::read_to_string(&route_count_path)
        .with_context(|| format!("read {}", route_count_path.display()))?;
    let route_count = route_count_text.trim().parse::<usize>().with_context(|| {
        format!(
            "parse split route count from {}",
            route_count_path.display()
        )
    })?;
    if route_count == 0 {
        bail!("split build found no routes; use plain `pocopine build` for non-routed apps");
    }

    for idx in 0..route_count {
        let mode = format!("route:{idx}");
        let out_name = format!("{base}_route_{idx}");
        run_split_wasm_pack(&path, release, &base, &mode, &out_name, None, &build_id)?;
    }
    write_split_loader(&path, &base)?;
    println!("✓ split build emitted shell + {route_count} route artifact(s)");
    Ok(())
}

fn run_split_wasm_pack(
    path: &Path,
    release: bool,
    base: &str,
    mode: &str,
    out_name: &str,
    route_count_out: Option<&Path>,
    build_id: &str,
) -> Result<()> {
    let mut cmd = Command::new("wasm-pack");
    cmd.arg("build")
        .arg("--target")
        .arg("web")
        .arg("--out-dir")
        .arg("pkg")
        .arg("--out-name")
        .arg(out_name);
    if release {
        cmd.arg("--release");
    } else {
        cmd.arg("--dev");
    }
    cmd.env("POCOPINE_SPLIT_MODE", mode)
        .env("POCOPINE_SPLIT_BASE", base)
        .current_dir(path);
    if let Some(path) = route_count_out {
        cmd.env("POCOPINE_SPLIT_ROUTE_COUNT_OUT", path);
    }
    let cfg_name = format!("pocopine_split_{}_{}", split_cfg_suffix(mode), build_id);
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
  mod.mount_pocopine_route(outlet, location.pathname);
  activeRoute = mod;
}}

export async function startPocopineSplitApp() {{
  const shell = await import("/pkg/{base}.js");
  await shell.default();
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
    build_entry(&project, args.release, args.split)?;

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
        if let Err(e) = build_entry(&project, args.release, args.split) {
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
