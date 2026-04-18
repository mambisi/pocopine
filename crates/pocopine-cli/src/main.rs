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
use std::time::Duration;

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build(a) => build(&a.path, a.release),
        Cmd::Run(a) => {
            let cfg = load_config(&a.path)?;
            build(&a.path, a.release)?;
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

    let manifest: Manifest = toml::from_str(&text)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    Ok(manifest.package.metadata.pocopine)
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
    println!("▶ spawning `{bin}` (cargo run --bin {bin} in {})", project.display());
    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn server bin `{bin}`"))?;
    Ok(BinChild { child, bin: bin.into() })
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
    let canonical = match candidate.canonicalize() {
        Ok(p) if p.starts_with(root) => p,
        _ => {
            let _ = request.respond(
                tiny_http::Response::from_string("not found").with_status_code(404),
            );
            return;
        }
    };

    let target = if canonical.is_dir() {
        canonical.join("index.html")
    } else {
        canonical
    };

    match std::fs::read(&target) {
        Ok(body) => {
            let mime = mime_of(&target);
            let header =
                tiny_http::Header::from_bytes(&b"Content-Type"[..], mime.as_bytes()).unwrap();
            let _ = request.respond(tiny_http::Response::from_data(body).with_header(header));
        }
        Err(_) => {
            let _ = request.respond(
                tiny_http::Response::from_string("not found").with_status_code(404),
            );
        }
    }
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
    build(&project, args.release)?;

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
        if let Err(e) = build(&project, args.release) {
            eprintln!("build failed: {e:#}");
        }
    };

    if let Some(child) = bin_child {
        child.kill();
    }
    result
}
