//! Shared `docker` shell-out helper used by every full-stack adapter
//! (RFC 080 §4.2 + Phase 1 §10). Only `docker` is permitted as an
//! external binary on the deploy path; everything else talks to host
//! APIs directly.
//!
//! Design rules:
//!
//! - Argument construction is pure (see [`build_args`], [`push_args`],
//!   [`login_args`]). The execution functions just join those args
//!   with `std::process::Command` so unit tests can assert on argv
//!   shape without invoking docker.
//! - Passwords go in through `--password-stdin`; never `--password`
//!   on the command line and never logged.
//! - Stdout/stderr from docker are streamed line-by-line through
//!   `tracing::info!(target: "pocopine.log")` per RFC 069. No raw
//!   `println!` / `eprintln!`.
//! - `DOCKER_HOST` is honored implicitly — docker CLI reads it on
//!   its own; we never inspect or override it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use tracing::info;

/// Thin wrapper around the `docker` CLI. `new()` resolves the binary
/// on `$PATH` lazily — each call to `build`/`push`/`login` spawns its
/// own child process.
pub struct DockerClient {
    bin: PathBuf,
}

impl DockerClient {
    /// Use the `docker` binary on `$PATH`.
    pub fn new() -> Self {
        Self {
            bin: PathBuf::from("docker"),
        }
    }

    /// Override the binary path (for vendored installs or tests).
    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        Self { bin: bin.into() }
    }

    /// Probe `docker info`. Surfaces a clear error if the daemon is
    /// unreachable or the binary is missing — used by
    /// `pocopine deploy doctor`.
    ///
    /// On failure we capture docker's own stderr so the message
    /// surfaces the actionable cause (socket permissions, TLS,
    /// bad `$DOCKER_HOST`, …) instead of a generic "not reachable".
    pub fn check_available(&self) -> Result<()> {
        let output = Command::new(&self.bin)
            .args(["info", "--format", "{{.ServerVersion}}"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| {
                "`docker info` failed to spawn — is the `docker` CLI installed and on $PATH?"
                    .to_string()
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let status = output.status;
        if stderr.is_empty() {
            bail!(
                "`docker info` exited with {status} — daemon not reachable (check $DOCKER_HOST or the local socket)",
            );
        }
        bail!(
            "`docker info` exited with {status} — daemon not reachable (check $DOCKER_HOST or the local socket)\n--- docker stderr ---\n{stderr}",
        );
    }

    /// `docker build -t <tag> [-f <dockerfile>] <context>`. Streams
    /// child output through `pocopine.log`.
    pub fn build(&self, ctx: &Path, tag: &str, dockerfile: Option<&Path>) -> Result<()> {
        let args = build_args(ctx, tag, dockerfile);
        info!(target: "pocopine.log", image = %tag, "docker build");
        run(&self.bin, &args, None)
    }

    /// `docker login <registry> --username <u> --password-stdin`.
    /// The password is fed via stdin so it never appears in argv.
    pub fn login(&self, registry: &str, username: &str, password: &str) -> Result<()> {
        let args = login_args(registry, username);
        info!(target: "pocopine.log", registry = %registry, user = %username, "docker login");
        run(&self.bin, &args, Some(password.as_bytes()))
    }

    /// `docker push <tag>`.
    pub fn push(&self, tag: &str) -> Result<()> {
        let args = push_args(tag);
        info!(target: "pocopine.log", image = %tag, "docker push");
        run(&self.bin, &args, None)
    }

    /// Best-effort check: does the local Docker config carry credentials
    /// usable for `registry_host`? Adapters call this to decide whether
    /// a `docker login` is needed before `docker push` — if the user is
    /// already logged in, the push just works (RFC 080 §11 q1).
    pub fn has_login(&self, registry_host: &str) -> bool {
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        else {
            return false;
        };
        let cfg = PathBuf::from(home).join(".docker").join("config.json");
        match std::fs::read_to_string(&cfg) {
            Ok(raw) => docker_config_has_auth(&raw, registry_host),
            Err(_) => false,
        }
    }
}

impl Default for DockerClient {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Pure argument construction (tests live here) ──────────────────────

/// `docker build -t <tag> [-f <dockerfile>] <context>`. The args are
/// returned as owned `String`s so adapters can also use this helper to
/// log the planned invocation under `--dry-run`.
pub fn build_args(ctx: &Path, tag: &str, dockerfile: Option<&Path>) -> Vec<String> {
    let mut args = vec!["build".to_owned(), "-t".to_owned(), tag.to_owned()];
    if let Some(df) = dockerfile {
        args.push("-f".to_owned());
        args.push(df.display().to_string());
    }
    args.push(ctx.display().to_string());
    args
}

/// `docker login <registry> --username <u> --password-stdin`.
pub fn login_args(registry: &str, username: &str) -> Vec<String> {
    vec![
        "login".to_owned(),
        registry.to_owned(),
        "--username".to_owned(),
        username.to_owned(),
        "--password-stdin".to_owned(),
    ]
}

/// `docker push <tag>`.
pub fn push_args(tag: &str) -> Vec<String> {
    vec!["push".to_owned(), tag.to_owned()]
}

// ─── Docker credential detection ───────────────────────────────────────

/// Pure: does this `~/.docker/config.json` content carry credentials
/// usable for `registry_host`? True when there is a global `credsStore`
/// (covers every registry), a per-registry `credHelpers` entry, or an
/// `auths` entry whose key resolves to the host.
pub fn docker_config_has_auth(config_json: &str, registry_host: &str) -> bool {
    let Ok(cfg) = serde_json::from_str::<serde_json::Value>(config_json) else {
        return false;
    };
    if cfg
        .get("credsStore")
        .and_then(|s| s.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    for field in ["credHelpers", "auths"] {
        if let Some(map) = cfg.get(field).and_then(|m| m.as_object()) {
            if map.keys().any(|k| registry_key_matches(k, registry_host)) {
                return true;
            }
        }
    }
    false
}

/// A Docker config registry key can be a bare host (`ghcr.io`) or a URL
/// (`https://ghcr.io/v1/`); both should match the bare `host`. Docker
/// Hub's aliases are collapsed so a Hub login is detected whichever
/// form the config / spec uses.
fn registry_key_matches(key: &str, host: &str) -> bool {
    let k = key
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let k = k.split('/').next().unwrap_or(k);
    norm_registry_host(k) == norm_registry_host(host)
}

/// Collapse Docker Hub's host aliases to `docker.io`.
fn norm_registry_host(host: &str) -> &str {
    match host {
        "index.docker.io" | "registry-1.docker.io" => "docker.io",
        other => other,
    }
}

// ─── Execution helper ──────────────────────────────────────────────────

/// Run `<bin> <args>` with optional stdin payload. Streams stdout and
/// stderr line-by-line through `tracing::info!(target = "pocopine.log")`.
/// On non-zero exit, returns an error containing the captured tail of
/// stderr.
fn run(bin: &Path, args: &[String], stdin_payload: Option<&[u8]>) -> Result<()> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin_payload.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning `{} {}`", bin.display(), args.join(" ")))?;

    if let (Some(payload), Some(mut stdin)) = (stdin_payload, child.stdin.take()) {
        stdin
            .write_all(payload)
            .context("writing to docker stdin")?;
        // Drop closes the pipe so docker stops reading.
    }

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let stdout_handle = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            info!(target: "pocopine.log", "docker: {line}");
        }
    });

    // Mirror stderr through tracing AND capture the tail so we can
    // attach it to the error on non-zero exit.
    let stderr_handle = std::thread::spawn(move || {
        let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            info!(target: "pocopine.log", "docker: {line}");
            tail.push_back(line);
            if tail.len() > 32 {
                tail.pop_front();
            }
        }
        tail.into_iter().collect::<Vec<_>>().join("\n")
    });

    let _ = stdout_handle.join();
    let stderr_tail = stderr_handle.join().unwrap_or_default();

    let status = child
        .wait()
        .with_context(|| format!("waiting for `{}`", bin.display()))?;
    if !status.success() {
        if stderr_tail.is_empty() {
            bail!(
                "`{} {}` exited with {status}",
                bin.display(),
                args.join(" ")
            );
        }
        bail!(
            "`{} {}` exited with {status}\n--- stderr tail ---\n{stderr_tail}",
            bin.display(),
            args.join(" "),
        );
    }
    Ok(())
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn build_args_without_dockerfile() {
        let args = build_args(Path::new("."), "myapp:sha", None);
        assert_eq!(args, vec!["build", "-t", "myapp:sha", "."]);
    }

    #[test]
    fn build_args_with_dockerfile_flag() {
        let args = build_args(
            Path::new("."),
            "myapp:sha",
            Some(Path::new("docker/build/Dockerfile")),
        );
        assert_eq!(
            args,
            vec![
                "build",
                "-t",
                "myapp:sha",
                "-f",
                "docker/build/Dockerfile",
                "."
            ]
        );
    }

    #[test]
    fn login_args_use_password_stdin_never_password_flag() {
        let args = login_args("registry.fly.io", "x");
        assert_eq!(
            args,
            vec![
                "login",
                "registry.fly.io",
                "--username",
                "x",
                "--password-stdin"
            ]
        );
        // The password itself is not in argv. Regression: never use
        // `--password` since it leaks the secret to `ps aux`.
        assert!(!args.iter().any(|a| a == "--password"));
    }

    #[test]
    fn push_args_is_minimal() {
        let args = push_args("registry.fly.io/myapp:sha");
        assert_eq!(args, vec!["push", "registry.fly.io/myapp:sha"]);
    }

    #[test]
    fn with_bin_overrides_binary_path() {
        let client = DockerClient::with_bin("/opt/podman/bin/podman");
        assert_eq!(client.bin, PathBuf::from("/opt/podman/bin/podman"));
    }

    #[test]
    fn docker_config_detects_auths_entry_by_bare_host() {
        let cfg = r#"{"auths":{"ghcr.io":{}}}"#;
        assert!(docker_config_has_auth(cfg, "ghcr.io"));
        assert!(!docker_config_has_auth(cfg, "registry.gitlab.com"));
    }

    #[test]
    fn docker_config_detects_auths_entry_by_url_key() {
        // Docker writes Docker Hub as a v1 URL; URL-form keys must still
        // match the bare host.
        let cfg = r#"{"auths":{"https://index.docker.io/v1/":{"auth":"x"}}}"#;
        assert!(docker_config_has_auth(cfg, "index.docker.io"));
    }

    #[test]
    fn docker_config_global_creds_store_covers_every_host() {
        let cfg = r#"{"credsStore":"desktop","auths":{}}"#;
        assert!(docker_config_has_auth(cfg, "ghcr.io"));
        assert!(docker_config_has_auth(cfg, "anything.example.com"));
    }

    #[test]
    fn docker_config_detects_per_registry_cred_helper() {
        let cfg = r#"{"credHelpers":{"ghcr.io":"gh"}}"#;
        assert!(docker_config_has_auth(cfg, "ghcr.io"));
        assert!(!docker_config_has_auth(cfg, "registry.gitlab.com"));
    }

    #[test]
    fn docker_config_no_match_or_garbage_is_false() {
        assert!(!docker_config_has_auth(r#"{"auths":{}}"#, "ghcr.io"));
        assert!(!docker_config_has_auth("not json", "ghcr.io"));
        assert!(!docker_config_has_auth("", "ghcr.io"));
    }
}
