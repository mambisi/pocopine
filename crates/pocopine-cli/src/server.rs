use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::client_modules;
use crate::config::PocopineConfig;
use crate::tools;

pub struct BinChild {
    child: Child,
    bin: String,
    role: BinRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinRole {
    Server,
    Worker,
}

impl BinChild {
    pub(crate) fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl BinRole {
    pub(crate) fn label(self) -> &'static str {
        match self {
            BinRole::Server => "server",
            BinRole::Worker => "worker",
        }
    }
}

pub fn spawn_bin(
    path: &Path,
    bin: &str,
    release: bool,
    role: BinRole,
    default_redis_url: bool,
) -> Result<BinChild> {
    spawn_bin_with_env(
        path,
        bin,
        release,
        role,
        default_redis_url,
        &BTreeMap::new(),
    )
}

/// Like [`spawn_bin`] but lets the caller inject extra `KEY=VALUE` pairs
/// onto the child process. Vars in `extra_env` win over the parent
/// environment but lose to anything the caller explicitly sets on `cmd`
/// after this returns. Dev-mode `.env` loading flows through here.
pub fn spawn_bin_with_env(
    path: &Path,
    bin: &str,
    release: bool,
    role: BinRole,
    default_redis_url: bool,
    extra_env: &BTreeMap<String, String>,
) -> Result<BinChild> {
    let project = path
        .canonicalize()
        .with_context(|| format!("resolve {}", path.display()))?;
    let project_tools = tools::ProjectTools::load(&project)?;
    let executable = built_bin_path(&project, &project_tools, bin, release)?;
    let mut cmd = Command::new(&executable);
    cmd.current_dir(&project);
    if default_redis_url {
        ensure_redis_env(&mut cmd);
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    println!("▶ spawning `{bin}` ({})", profile_name(release));
    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn configured bin `{}`", executable.display()))?;
    Ok(BinChild {
        child,
        bin: bin.into(),
        role,
    })
}

fn built_bin_path(
    project: &Path,
    project_tools: &tools::ProjectTools,
    bin: &str,
    release: bool,
) -> Result<PathBuf> {
    let target_dir = cargo_target_dir(project, project_tools)?;
    let path = bin_executable_path(&target_dir, bin, release);
    if path.is_file() {
        return Ok(path);
    }
    bail!(
        "configured bin `{bin}` was not found at {}; run `pocopine build` first",
        path.display()
    )
}

fn cargo_target_dir(project: &Path, project_tools: &tools::ProjectTools) -> Result<PathBuf> {
    let mut cmd = project_tools.cargo().command();
    cmd.arg("metadata")
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1")
        .current_dir(project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cmd.output().context("invoke cargo metadata")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "cargo metadata failed with {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }
    let metadata: Value =
        serde_json::from_slice(&output.stdout).context("parse cargo metadata JSON")?;
    let target_dir = metadata
        .get("target_directory")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("cargo metadata did not include target_directory"))?;
    Ok(PathBuf::from(target_dir))
}

fn bin_executable_path(target_dir: &Path, bin: &str, release: bool) -> PathBuf {
    let profile = profile_name(release);
    let executable = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    };
    target_dir.join(profile).join(executable)
}

fn profile_name(release: bool) -> &'static str {
    if release {
        "release"
    } else {
        "debug"
    }
}

pub fn check_configured_port_available(cfg: &PocopineConfig) -> Result<()> {
    if cfg.bin.is_some() {
        if let Some(port) = cfg.port {
            ensure_port_available(port)?;
        }
    }
    Ok(())
}

fn ensure_port_available(port: u16) -> Result<()> {
    if port == 0 {
        return Ok(());
    }
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(err) if err.kind() == ErrorKind::AddrInUse => {
            let owner = port_owner(port)
                .map(|owner| format!(" by `{}` (pid {})", owner.command, owner.pid))
                .unwrap_or_default();
            bail!(
                "port {port} is already in use{owner}; stop the existing server or change `[package.metadata.pocopine].port`"
            );
        }
        Err(err) => Err(err).with_context(|| format!("check configured port {port}")),
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PortOwner {
    pid: u32,
    command: String,
}

#[cfg(target_os = "linux")]
fn port_owner(port: u16) -> Option<PortOwner> {
    let inodes = listening_socket_inodes(port);
    if inodes.is_empty() {
        return None;
    }

    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|value| value.strip_suffix(']'))
            else {
                continue;
            };
            if inodes.contains(inode) {
                return Some(PortOwner {
                    pid,
                    command: process_command(pid),
                });
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn port_owner(port: u16) -> Option<PortOwner> {
    let output = Command::new("lsof")
        .arg("-nP")
        .arg(format!("-iTCP:{port}"))
        .arg("-sTCP:LISTEN")
        .arg("-Fpc")
        .output()
        .ok()?;
    parse_lsof_owner(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(windows)]
fn port_owner(port: u16) -> Option<PortOwner> {
    let output = Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .output()
        .ok()?;
    let pid = parse_netstat_owner_pid(&String::from_utf8_lossy(&output.stdout), port)?;
    let command = windows_process_command(pid).unwrap_or_else(|| "unknown".into());
    Some(PortOwner { pid, command })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn port_owner(_port: u16) -> Option<PortOwner> {
    None
}

#[cfg(target_os = "linux")]
fn listening_socket_inodes(port: u16) -> std::collections::HashSet<String> {
    let mut inodes = std::collections::HashSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(contents) = std::fs::read_to_string(table) else {
            continue;
        };
        for line in contents.lines().skip(1) {
            if let Some(inode) = listening_inode_from_proc_net_line(line, port) {
                inodes.insert(inode.to_string());
            }
        }
    }
    inodes
}

#[cfg(target_os = "linux")]
fn process_command(pid: u32) -> String {
    let path = PathBuf::from("/proc").join(pid.to_string()).join("comm");
    std::fs::read_to_string(path)
        .ok()
        .map(|command| command.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|command| !command.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(any(target_os = "linux", test))]
fn listening_inode_from_proc_net_line(line: &str, port: u16) -> Option<&str> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    let local = parts.get(1)?;
    let state = parts.get(3)?;
    if *state != "0A" {
        return None;
    }
    let (_, port_hex) = local.rsplit_once(':')?;
    let local_port = u16::from_str_radix(port_hex, 16).ok()?;
    if local_port == port {
        parts.get(9).copied()
    } else {
        None
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_lsof_owner(output: &str) -> Option<PortOwner> {
    let mut pid = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok();
            continue;
        }
        if let Some(command) = line.strip_prefix('c') {
            let command = command.trim();
            if !command.is_empty() {
                return Some(PortOwner {
                    pid: pid?,
                    command: command.into(),
                });
            }
        }
    }
    None
}

#[cfg(any(windows, test))]
fn parse_netstat_owner_pid(output: &str, port: u16) -> Option<u32> {
    for line in output.lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() < 5 || !parts[0].eq_ignore_ascii_case("tcp") {
            continue;
        }
        if !parts[3].eq_ignore_ascii_case("LISTENING") {
            continue;
        }
        if address_port(parts[1]) == Some(port) {
            return parts[4].parse().ok();
        }
    }
    None
}

#[cfg(any(windows, test))]
fn address_port(address: &str) -> Option<u16> {
    let (_, port) = address.rsplit_once(':')?;
    port.parse().ok()
}

#[cfg(windows)]
fn windows_process_command(pid: u32) -> Option<String> {
    let filter = format!("PID eq {pid}");
    let output = Command::new("tasklist")
        .args(["/FI", &filter, "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    parse_tasklist_command(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(any(windows, test))]
fn parse_tasklist_command(output: &str) -> Option<String> {
    let line = output
        .lines()
        .find(|line| !line.trim().is_empty() && !line.contains("No tasks are running"))?;
    let command = line.trim().strip_prefix('"')?.split_once("\",")?.0.trim();
    if command.is_empty() {
        None
    } else {
        Some(command.into())
    }
}

pub fn validate_worker_backend_for_separate_process(default_redis_url: bool) -> Result<()> {
    let backend = std::env::var("POCOPINE_JOB_BACKEND")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase());
    let redis_url = std::env::var("POCOPINE_REDIS_URL").ok();
    let has_redis_url = redis_url
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());

    match backend.as_deref() {
        Some("memory") => bail!(
            "`worker-bin` runs in a separate process, but POCOPINE_JOB_BACKEND=memory is process-local; use Redis for a separate worker binary or embed the worker in the server process"
        ),
        Some("redis") if has_redis_url => Ok(()),
        Some("redis") if default_redis_url => Ok(()),
        Some("redis") => bail!("`worker-bin` needs POCOPINE_REDIS_URL when POCOPINE_JOB_BACKEND=redis"),
        Some("") => bail!("POCOPINE_JOB_BACKEND was set but empty; use `memory` or `redis`"),
        Some(other) => bail!("unsupported POCOPINE_JOB_BACKEND `{other}`; use `memory` or `redis`"),
        None if has_redis_url => Ok(()),
        None if default_redis_url => Ok(()),
        None => bail!(
            "`worker-bin` runs in a separate process; set POCOPINE_REDIS_URL for Redis-backed jobs, or embed the worker in the server process to use the memory backend"
        ),
    }
}

pub fn run_project(path: &Path, cfg: &PocopineConfig, release: bool, port: u16) -> Result<()> {
    check_configured_port_available(cfg)?;
    if cfg.worker_bin.is_some() {
        validate_worker_backend_for_separate_process(false)?;
    }

    let worker = cfg
        .worker_bin
        .as_deref()
        .map(|bin| spawn_bin(path, bin, release, BinRole::Worker, false))
        .transpose()?;

    match cfg.bin.as_deref() {
        Some(bin) => {
            let server = spawn_bin(path, bin, release, BinRole::Server, false)?;
            let mut children = Vec::new();
            children.push(server);
            if let Some(worker) = worker {
                children.push(worker);
            }
            wait_for_children(children)
        }
        None => {
            if let Some(worker) = worker {
                let serve_path = path
                    .canonicalize()
                    .with_context(|| format!("bad serve dir: {}", path.display()))?;
                thread::spawn(move || {
                    if let Err(e) = serve_static(&serve_path, port) {
                        eprintln!("server error: {e}");
                    }
                });
                wait_for_children(vec![worker])
            } else {
                serve_static(path, port)
            }
        }
    }
}

pub fn serve_static(path: &Path, port: u16) -> Result<()> {
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

pub fn poll_children(children: &mut Vec<BinChild>) -> Result<Option<String>> {
    for index in 0..children.len() {
        if let Some(status) = children[index]
            .child
            .try_wait()
            .with_context(|| format!("poll `{}`", children[index].bin))?
        {
            let exited = children.remove(index);
            return Ok(Some(format!(
                "{} bin `{}` exited with {status}",
                exited.role.label(),
                exited.bin
            )));
        }
    }
    Ok(None)
}

fn ensure_redis_env(cmd: &mut Command) {
    if std::env::var_os("POCOPINE_REDIS_URL").is_none() {
        cmd.env("POCOPINE_REDIS_URL", "redis://127.0.0.1/");
    }
}

fn wait_for_children(mut children: Vec<BinChild>) -> Result<()> {
    loop {
        for index in 0..children.len() {
            if let Some(status) = children[index]
                .child
                .try_wait()
                .with_context(|| format!("poll `{}`", children[index].bin))?
            {
                let exited = children.swap_remove(index);
                for child in children {
                    child.kill();
                }
                if status.success() && exited.role == BinRole::Server {
                    return Ok(());
                }
                bail!(
                    "{} bin `{}` exited with {status}",
                    exited.role.label(),
                    exited.bin
                );
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
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

    // RFC-100 — content-addressed asset route:
    // GET /assets/<hash>/<path...> serves `assets/<path...>` as
    // immutable after verifying <hash> against the file bytes.
    if let Some(response) = asset_route_response(root, rel) {
        let _ = request.respond(response);
        return;
    }

    let candidate = root.join(rel);
    let looks_like_asset = looks_like_asset_path(rel);

    let canonical = candidate
        .canonicalize()
        .ok()
        .filter(|p| p.starts_with(root));

    // Serve the resolved path when it exists.
    if let Some(canonical) = canonical {
        let target = if canonical.is_dir() {
            canonical.join("index.html")
        } else {
            canonical
        };
        if let Ok(body) = std::fs::read(&target) {
            let mime = mime_of(&target);
            let body = client_modules::inject_html_if_needed(root, &target, body);
            let header =
                tiny_http::Header::from_bytes(&b"Content-Type"[..], mime.as_bytes()).unwrap();
            let mut response = tiny_http::Response::from_data(body).with_header(header);
            let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if let Some(cache) = cache_control_for(name, mime) {
                response = response.with_header(cache_control_header(cache));
            }
            let _ = request.respond(response);
            return;
        }
    }

    // Fall back to root index.html for non-asset paths (SPA history fallback).
    // Asset-looking paths 404 so bad imports are not masked.
    if !looks_like_asset {
        let fallback = root.join("index.html");
        if let Ok(body) = std::fs::read(&fallback) {
            let body = client_modules::inject_html_if_needed(root, &fallback, body);
            let header = tiny_http::Header::from_bytes(
                &b"Content-Type"[..],
                &b"text/html; charset=utf-8"[..],
            )
            .unwrap();
            let _ = request.respond(
                tiny_http::Response::from_data(body)
                    .with_header(header)
                    .with_header(cache_control_header(CACHE_NO_CACHE)),
            );
            return;
        }
    }

    let _ = request.respond(tiny_http::Response::from_string("not found").with_status_code(404));
}

/// RFC-100 — match `assets/<hash>/<path...>` and serve
/// `assets/<path...>` from the root with an immutable cache header.
///
/// `None` when the URL is not an asset URL (no 8-hex hash segment)
/// or the file does not exist — both fall through to the normal
/// static handler, so plain `/assets/logo.svg` paths keep working.
/// A hash that does not match the file bytes answers `409` with an
/// explanation: the `asset!` macro hashes at compile time, so
/// editing an asset without recompiling the calling crate leaves a
/// stale hash in the binary (RFC-100 gives `pocopine build` a
/// fingerprint env to own invalidation).
fn asset_route_response(
    root: &Path,
    rel: &str,
) -> Option<tiny_http::Response<std::io::Cursor<Vec<u8>>>> {
    let rest = rel.strip_prefix("assets/")?;
    let (hash, path) = rest.split_once('/')?;
    if !is_asset_hash(hash) || path.is_empty() {
        return None;
    }

    let candidate = root.join("assets").join(path);
    let canonical = candidate
        .canonicalize()
        .ok()
        .filter(|p| p.starts_with(root))?;
    let body = std::fs::read(&canonical).ok()?;

    let actual = asset_hash_prefix(&body);
    if actual != hash {
        let message = format!(
            "stale asset hash: /assets/{hash}/{path} was built against \
             different bytes (the file currently hashes to {actual}).\n\
             The `asset!` macro hashes at compile time and asset edits do \
             not dirty the calling crate, so the binary still holds the \
             old hash. Rebuild the crate that calls asset!(\"{path}\") \
             (touch the .rs or `cargo clean -p <crate>`). RFC-100: \
             `pocopine build` will own invalidation via a fingerprint env."
        );
        return Some(tiny_http::Response::from_string(message).with_status_code(409));
    }

    let content_type =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], mime_of(&canonical).as_bytes())
            .unwrap();
    let cache_control = tiny_http::Header::from_bytes(
        &b"Cache-Control"[..],
        &b"public,max-age=31536000,immutable"[..],
    )
    .unwrap();
    Some(
        tiny_http::Response::from_data(body)
            .with_header(content_type)
            .with_header(cache_control),
    )
}

/// RFC-100 — true for an 8-char lowercase-hex hash segment. Also the
/// hash shape of the content-hashed bundle pair `build::hash_pkg_bundle`
/// writes.
pub(crate) fn is_asset_hash(segment: &str) -> bool {
    segment.len() == 8
        && segment
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

const CACHE_IMMUTABLE: &str = "public,max-age=31536000,immutable";
const CACHE_NO_CACHE: &str = "no-cache";

fn cache_control_header(value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Cache-Control"[..], value.as_bytes()).unwrap()
}

/// Cache policy for a directly served file, mirroring production
/// (`pocopine_server::static_files`): the content-hashed bundle pair
/// (`<name>.<hash8>.js` / `<name>_bg.<hash8>.wasm`, written by
/// `build::hash_pkg_bundle`) never changes under its URL → immutable;
/// HTML is the mutable entry point that names the current pair →
/// revalidate every load. Everything else keeps no explicit header.
fn cache_control_for(file_name: &str, mime: &str) -> Option<&'static str> {
    if is_hashed_bundle_name(file_name) {
        return Some(CACHE_IMMUTABLE);
    }
    if mime.starts_with("text/html") {
        return Some(CACHE_NO_CACHE);
    }
    None
}

/// `website.0a1b2c3d.js` / `website_bg.0a1b2c3d.wasm` → true. Only the
/// bundle-pair extensions count, so a user file with a hex-looking
/// name segment doesn't silently become immutable.
fn is_hashed_bundle_name(file_name: &str) -> bool {
    let Some((stem, ext)) = file_name.rsplit_once('.') else {
        return false;
    };
    if ext != "js" && ext != "wasm" {
        return false;
    }
    stem.rsplit_once('.')
        .is_some_and(|(_, hash)| is_asset_hash(hash))
}

/// RFC-100 — 8-hex-char content hash; same shape as
/// `pocopine_core::assets::asset_hash` (prefix of
/// `pocopine_crypto::sha256_hex`), duplicated because the CLI does
/// not link the wasm runtime crate.
fn asset_hash_prefix(bytes: &[u8]) -> String {
    let mut hex = pocopine_crypto::sha256_hex(bytes);
    hex.truncate(8);
    hex
}

/// True when the last URL segment has a file extension. Used to decide
/// whether an unmatched path should 404 or fall back to index.html:
/// `/pkg/spa.js` -> 404, `/blog/42` -> index.html.
fn looks_like_asset_path(rel: &str) -> bool {
    let last = rel.rsplit('/').next().unwrap_or("");
    last.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
}

// The MIME table lives in `assets_sync` (RFC-100: one canonical table
// for the dev server, the bucket sync, and — via stored content
// types — the Mode B proxy).
use crate::assets_sync::mime_of;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_path_uses_cargo_target_profile_dir() {
        let path = bin_executable_path(Path::new("/tmp/pocopine-target"), "server", false);
        let executable = if cfg!(windows) {
            "server.exe"
        } else {
            "server"
        };
        assert!(path.ends_with(Path::new("debug").join(executable)));

        let path = bin_executable_path(Path::new("/tmp/pocopine-target"), "server", true);
        assert!(path.ends_with(Path::new("release").join(executable)));
    }

    // RFC-100 — content-addressed asset route.
    #[test]
    fn asset_route_serves_immutable_on_hash_match() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("assets/blog")).unwrap();
        std::fs::write(root.join("assets/blog/clip.webm"), b"hello world").unwrap();

        // sha256("hello world") = b94d27b9…
        let response = asset_route_response(&root, "assets/b94d27b9/blog/clip.webm").unwrap();
        assert_eq!(response.status_code().0, 200);
        let headers: Vec<String> = response.headers().iter().map(|h| h.to_string()).collect();
        assert!(headers.contains(&"Content-Type: video/webm".to_string()));
        assert!(headers.contains(&"Cache-Control: public,max-age=31536000,immutable".to_string()));
    }

    #[test]
    fn asset_route_answers_409_on_stale_hash() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/logo.svg"), b"hello world").unwrap();

        let response = asset_route_response(&root, "assets/deadbeef/logo.svg").unwrap();
        assert_eq!(response.status_code().0, 409);
    }

    #[test]
    fn asset_route_ignores_non_hash_and_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/logo.svg"), b"hello world").unwrap();

        // Plain asset path (no hash segment) → normal static handler.
        assert!(asset_route_response(&root, "assets/logo.svg").is_none());
        // Hash-shaped but uppercase / wrong length → not an asset URL.
        assert!(asset_route_response(&root, "assets/DEADBEEF/logo.svg").is_none());
        assert!(asset_route_response(&root, "assets/abc/logo.svg").is_none());
        // Missing file → fall through (404 via looks_like_asset_path).
        assert!(asset_route_response(&root, "assets/b94d27b9/missing.svg").is_none());
        // Traversal out of the root → fall through.
        assert!(asset_route_response(&root, "assets/b94d27b9/../../etc/passwd").is_none());
    }

    #[test]
    fn cache_policy_pins_hashed_bundles_and_revalidates_html() {
        // The hashed pair → immutable.
        assert_eq!(
            cache_control_for("website.0a1b2c3d.js", "text/javascript"),
            Some(CACHE_IMMUTABLE)
        );
        assert_eq!(
            cache_control_for("website_bg.0a1b2c3d.wasm", "application/wasm"),
            Some(CACHE_IMMUTABLE)
        );
        // HTML always revalidates.
        assert_eq!(
            cache_control_for("index.html", "text/html; charset=utf-8"),
            Some(CACHE_NO_CACHE)
        );
        // Unhashed bundle names and other files keep no explicit policy.
        assert_eq!(cache_control_for("website.js", "text/javascript"), None);
        assert_eq!(
            cache_control_for("website_bg.wasm", "application/wasm"),
            None
        );
        assert_eq!(cache_control_for("styles.css", "text/css"), None);
        assert_eq!(cache_control_for("photo.0a1b2c3d.png", "image/png"), None);
        assert_eq!(
            cache_control_for("website.DEADBEEF.js", "text/javascript"),
            None
        );
    }

    #[test]
    fn mime_table_covers_video_types() {
        // Browsers refuse `<video>` sources served with the octet-stream
        // fallback (Chromium never starts muted autoplay for them), so
        // the table must map the video extensions explicitly.
        assert_eq!(mime_of(Path::new("assets/blog/clip.webm")), "video/webm");
        assert_eq!(mime_of(Path::new("assets/blog/clip.mp4")), "video/mp4");
    }

    #[test]
    fn configured_port_check_reports_busy_server_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let cfg = PocopineConfig {
            bin: Some("server".into()),
            port: Some(port),
            ..PocopineConfig::default()
        };

        let err = check_configured_port_available(&cfg).unwrap_err();
        assert!(err.to_string().contains("already in use"));
    }

    #[test]
    fn configured_port_check_ignores_static_mode() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let cfg = PocopineConfig {
            port: Some(port),
            ..PocopineConfig::default()
        };

        check_configured_port_available(&cfg).unwrap();
    }

    #[test]
    fn proc_net_line_reports_listening_port_inode() {
        let line = "0: 0100007F:0BCE 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 7031057 1 0000000000000000";

        assert_eq!(
            listening_inode_from_proc_net_line(line, 3022),
            Some("7031057")
        );
    }

    #[test]
    fn proc_net_line_ignores_non_listening_socket() {
        let line = "0: 0100007F:0BCE 00000000:0000 01 00000000:00000000 00:00000000 00000000 1000 0 7031057 1 0000000000000000";

        assert_eq!(listening_inode_from_proc_net_line(line, 3022), None);
    }

    #[test]
    fn lsof_output_reports_owner() {
        let output = "p1251608\ncserver\n";

        assert_eq!(
            parse_lsof_owner(output),
            Some(PortOwner {
                pid: 1251608,
                command: "server".into()
            })
        );
    }

    #[test]
    fn netstat_output_reports_listening_owner() {
        let output = "\
  Proto  Local Address          Foreign Address        State           PID
  TCP    127.0.0.1:3022         0.0.0.0:0              LISTENING       1251608
";

        assert_eq!(parse_netstat_owner_pid(output, 3022), Some(1251608));
    }

    #[test]
    fn tasklist_output_reports_command() {
        let output = "\"server.exe\",\"1251608\",\"Console\",\"1\",\"12,344 K\"\n";

        assert_eq!(parse_tasklist_command(output), Some("server.exe".into()));
    }
}
