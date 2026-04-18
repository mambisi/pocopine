//! `pocopine` — project CLI. Three subcommands:
//!   * `build` — wraps `wasm-pack build --target web`
//!   * `run`   — build + serve the project directory as static files
//!   * `dev`   — run + rebuild on source changes

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Parser, Debug)]
#[command(name = "pocopine", about = "pocopine project CLI", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Build the wasm bundle with wasm-pack.
    Build(BuildArgs),
    /// Build once, then serve the project directory on a local port.
    Run(ServeArgs),
    /// Serve the project directory and rebuild on source changes.
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
    /// Path to the crate (defaults to current dir). Static files are served
    /// from this directory too — pair it with an `index.html` next to the
    /// generated `pkg/`.
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Port to listen on. If the port is taken, the next available port is
    /// tried (up to `port + 20`).
    #[arg(long, default_value_t = 5243)]
    port: u16,
    /// Build in release mode.
    #[arg(long)]
    release: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build(a) => build(&a.path, a.release),
        Cmd::Run(a) => {
            build(&a.path, a.release)?;
            serve(&a.path, a.port)
        }
        Cmd::Dev(a) => dev(&a.path, a.port, a.release),
    }
}

fn build(path: &Path, release: bool) -> Result<()> {
    let path = path.canonicalize().with_context(|| {
        format!("could not resolve project path: {}", path.display())
    })?;
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

fn serve(path: &Path, port: u16) -> Result<()> {
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

/// Try to bind the requested port; if taken, step up to `port + 20`. Returns
/// the server and the port it actually bound to.
fn bind_port(start: u16) -> Result<(tiny_http::Server, u16)> {
    const ATTEMPTS: u16 = 21;
    let mut last_err: Option<String> = None;
    for offset in 0..ATTEMPTS {
        let port = match start.checked_add(offset) {
            Some(p) => p,
            None => break,
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

fn dev(path: &Path, port: u16, release: bool) -> Result<()> {
    let project = path.canonicalize()?;
    // Initial build — fail fast if it doesn't compile.
    build(&project, release)?;

    // Serve in a background thread so we can keep watching in the main.
    let serve_path = project.clone();
    thread::spawn(move || {
        if let Err(e) = serve(&serve_path, port) {
            eprintln!("server error: {e}");
        }
    });

    // Debounce-ish: coalesce events that arrive within 250ms.
    let (tx, rx) = channel::<()>();
    let latest: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));

    let tx_w = tx.clone();
    let latest_w = latest.clone();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                use notify::EventKind::*;
                if matches!(ev.kind, Modify(_) | Create(_) | Remove(_)) {
                    *latest_w.lock().unwrap() = Some(std::time::Instant::now());
                    let _ = tx_w.send(());
                }
            }
        })?;
    let src_dir = project.join("src");
    watcher.watch(&src_dir, RecursiveMode::Recursive)?;
    println!("👀 watching {}", src_dir.display());

    loop {
        // Block until a first event arrives.
        if rx.recv().is_err() {
            break;
        }
        // Drain any further events that came in close together.
        thread::sleep(Duration::from_millis(250));
        while rx.try_recv().is_ok() {}

        println!("↻ rebuilding…");
        if let Err(e) = build(&project, release) {
            eprintln!("build failed: {e:#}");
        }
    }
    Ok(())
}

fn handle(root: &Path, request: tiny_http::Request) {
    let url = request.url().to_string();
    let rel = url.split('?').next().unwrap_or("/").trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };

    // Refuse anything that climbs out of the served directory.
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
