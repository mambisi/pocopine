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
            let _ = request.respond(tiny_http::Response::from_data(body).with_header(header));
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
            let _ = request.respond(tiny_http::Response::from_data(body).with_header(header));
            return;
        }
    }

    let _ = request.respond(tiny_http::Response::from_string("not found").with_status_code(404));
}

/// True when the last URL segment has a file extension. Used to decide
/// whether an unmatched path should 404 or fall back to index.html:
/// `/pkg/spa.js` -> 404, `/blog/42` -> index.html.
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
