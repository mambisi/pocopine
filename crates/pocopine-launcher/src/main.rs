//! Procfile-style entrypoint for pocopine runtime images (RFC 080 §5.3).
//!
//! Reads the process name from argv[1] or `$POCOPINE_PROCESS` and
//! `exec()`s the matching binary so PID 1 inside the container becomes
//! the real process — signal handling, exit codes, `/proc/1` all
//! behave as expected.
//!
//! Dispatch resolution order:
//!
//!   1. `POCOPINE_PROC_<UPPER>` env var (e.g. `POCOPINE_PROC_WEB`),
//!      set by `pocopine-deploy`'s generated Dockerfile from
//!      `[deploy.processes]` (RFC 080 Phase 6).
//!   2. Hard-coded fallback for the standard `web` / `worker` layout
//!      so the binary still works in projects that haven't regenerated
//!      their Dockerfile.

#![forbid(unsafe_code)]

// Host-only PID 1 shim. wasm32 has no `std::os::unix` or `Command`;
// stub `main` keeps the crate in workspace builds.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::env;
#[cfg(not(target_arch = "wasm32"))]
use std::os::unix::process::CommandExt;
#[cfg(not(target_arch = "wasm32"))]
use std::process::{Command, ExitCode};

/// Resolve a process name to a binary path. Env var first, then the
/// legacy hard-coded fallback. `bin_for_with_env` exists as a testing
/// seam — it takes an explicit lookup closure so tests don't mutate
/// process env.
#[cfg(not(target_arch = "wasm32"))]
fn bin_for(process: &str) -> Option<String> {
    bin_for_with_env(process, env_lookup)
}

#[cfg(not(target_arch = "wasm32"))]
fn bin_for_with_env<F>(process: &str, env: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let key = env_var_name(process);
    if let Some(path) = env(&key) {
        if !path.is_empty() {
            return Some(path);
        }
    }
    // Legacy fallback so images built before Phase 6 still launch.
    match process {
        "web" => Some("/usr/local/bin/server".into()),
        "worker" => Some("/usr/local/bin/worker".into()),
        _ => None,
    }
}

/// Process name → env var name: `web` → `POCOPINE_PROC_WEB`,
/// `my-api` → `POCOPINE_PROC_MY_API`. Non-alphanumeric bytes collapse
/// to `_`, matching the credentials store's encoding.
#[cfg(not(target_arch = "wasm32"))]
fn env_var_name(process: &str) -> String {
    let normalised: String = process
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("POCOPINE_PROC_{normalised}")
}

#[cfg(not(target_arch = "wasm32"))]
fn env_lookup(key: &str) -> Option<String> {
    env::var(key).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> ExitCode {
    let mut args = env::args().skip(1);

    let process = match args.next().or_else(|| env::var("POCOPINE_PROCESS").ok()) {
        Some(p) => p,
        None => {
            eprintln!("pocopine-launcher: usage: pocopine-launcher <process> [args…]");
            return ExitCode::from(64); // EX_USAGE
        }
    };

    let bin = match bin_for(&process) {
        Some(b) => b,
        None => {
            eprintln!(
                "pocopine-launcher: unknown process `{process}` (no `{}` env var set, and no built-in fallback)",
                env_var_name(&process),
            );
            return ExitCode::from(64);
        }
    };

    // `exec` replaces this process with the target binary on success
    // and only returns on failure (the io::Error explains why).
    let err = Command::new(&bin).args(args).exec();
    eprintln!("pocopine-launcher: failed to exec `{bin}`: {err}");
    ExitCode::from(126) // EX_NOPERM-ish: binary exists by build invariant but we couldn't run it.
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env_from(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |k| map.get(k).map(|s| (*s).to_owned())
    }

    #[test]
    fn env_var_name_uppercases_and_normalises() {
        assert_eq!(env_var_name("web"), "POCOPINE_PROC_WEB");
        assert_eq!(env_var_name("Worker"), "POCOPINE_PROC_WORKER");
        assert_eq!(env_var_name("my-api"), "POCOPINE_PROC_MY_API");
        assert_eq!(env_var_name("a.b"), "POCOPINE_PROC_A_B");
    }

    #[test]
    fn bin_for_falls_back_to_legacy_web_worker() {
        assert_eq!(
            bin_for_with_env("web", no_env).as_deref(),
            Some("/usr/local/bin/server"),
        );
        assert_eq!(
            bin_for_with_env("worker", no_env).as_deref(),
            Some("/usr/local/bin/worker"),
        );
        assert!(bin_for_with_env("scheduler", no_env).is_none());
    }

    #[test]
    fn bin_for_prefers_env_var_over_fallback() {
        let env = env_from(HashMap::from([("POCOPINE_PROC_WEB", "/opt/api/bin/api")]));
        assert_eq!(
            bin_for_with_env("web", env).as_deref(),
            Some("/opt/api/bin/api"),
        );
    }

    #[test]
    fn bin_for_resolves_custom_process_names() {
        // No legacy fallback for `api`; env var unlocks it.
        let env = env_from(HashMap::from([("POCOPINE_PROC_API", "/usr/local/bin/api")]));
        assert_eq!(
            bin_for_with_env("api", env).as_deref(),
            Some("/usr/local/bin/api"),
        );
    }

    #[test]
    fn bin_for_ignores_empty_env_value() {
        let env = env_from(HashMap::from([("POCOPINE_PROC_WEB", "")]));
        // Empty value should fall through to the hard-coded fallback.
        assert_eq!(
            bin_for_with_env("web", env).as_deref(),
            Some("/usr/local/bin/server"),
        );
    }
}
