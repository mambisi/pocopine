//! Credentials store for host API tokens (RFC 080 §5.5).
//!
//! Tokens live in `~/.pocopine/credentials.toml` with mode `0600`. Env
//! vars (`POCOPINE_<HOST>_TOKEN`) take precedence over the file so CI
//! runs without touching disk.
//!
//! Layout:
//!
//! ```toml
//! [fly]
//! token = "fly_..."
//!
//! [railway]
//! token = "rw_..."
//! ```
//!
//! Hosts not present in this file fall back to env, then to a hard
//! error pointing the user at `pocopine deploy auth <host>`.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Path of the credentials file relative to `$HOME`.
pub const REL_PATH: &str = ".pocopine/credentials.toml";

/// Where the token resolved from. Returned by [`list`] so
/// `pocopine deploy auth --list` can show the user which source wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    File,
    EnvOverridesFile,
}

/// Look up the token for `host`. Returns the env-var value if set
/// (`POCOPINE_<HOST>_TOKEN`), otherwise the entry in
/// `~/.pocopine/credentials.toml`, otherwise an error suggesting the
/// auth command.
pub fn load(host: &str) -> Result<String> {
    load_inner(&home()?, host, env_lookup)
}

/// Persist `token` for `host` to the credentials file with mode `0600`.
/// Idempotent: other hosts' entries are preserved.
pub fn store(host: &str, token: &str) -> Result<()> {
    store_inner(&home()?, host, token)
}

/// Remove the entry for `host`. Returns `Ok(())` even if it didn't
/// exist (so `pocopine deploy auth --revoke <host>` is idempotent).
pub fn revoke(host: &str) -> Result<()> {
    revoke_inner(&home()?, host)
}

/// `(host, source)` pairs for the configured hosts, merging file and
/// env. Order is host-alphabetical.
pub fn list() -> Result<Vec<(String, Source)>> {
    list_inner(&home()?, env_lookup)
}

/// Built-in adapter host names. `list` enumerates candidate
/// env-var-only entries from this list because the
/// `POCOPINE_<HOST>_TOKEN` encoding loses the distinction between `-`
/// and `_` in the host name (e.g. `cf-pages` and `cf_pages` would both
/// encode to `POCOPINE_CF_PAGES_TOKEN`).
pub const KNOWN_HOSTS: &[&str] = &[
    "cf-pages",
    "cloud-run",
    "fly",
    "gh-pages",
    "netlify",
    "railway",
    "render",
    "vercel",
];

// ─── Internal seams (used by tests) ─────────────────────────────────────

fn load_inner<F>(home: &Path, host: &str, env: F) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(t) = env(&env_var_name(host)) {
        if !t.is_empty() {
            return Ok(t);
        }
    }

    let path = home.join(REL_PATH);
    if !path.exists() {
        bail!(
            "no token for `{host}`. Set ${env} or run `pocopine deploy auth {host}` to store one.",
            env = env_var_name(host),
        );
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("reading credentials file {}", path.display()))?;
    let store: Store = toml::from_str(&raw)
        .with_context(|| format!("parsing credentials file {}", path.display()))?;
    let entry = store.hosts.get(host).with_context(|| {
        format!(
            "no `[{host}]` entry in {}. Run `pocopine deploy auth {host}` to add one.",
            path.display(),
        )
    })?;
    Ok(entry.token.clone())
}

fn store_inner(home: &Path, host: &str, token: &str) -> Result<()> {
    let path = home.join(REL_PATH);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating directory {}", dir.display()))?;
    }

    let mut store = read_or_default(&path)?;
    store.hosts.insert(
        host.to_owned(),
        Entry {
            token: token.to_owned(),
        },
    );
    let raw = toml::to_string_pretty(&store).context("serialising credentials")?;
    write_secure(&path, raw.as_bytes())
}

fn revoke_inner(home: &Path, host: &str) -> Result<()> {
    let path = home.join(REL_PATH);
    if !path.exists() {
        return Ok(());
    }
    let mut store = read_or_default(&path)?;
    if store.hosts.remove(host).is_none() {
        return Ok(());
    }
    let raw = toml::to_string_pretty(&store).context("serialising credentials")?;
    write_secure(&path, raw.as_bytes())
}

fn list_inner<F>(home: &Path, env: F) -> Result<Vec<(String, Source)>>
where
    F: Fn(&str) -> Option<String>,
{
    use std::collections::HashSet;
    let mut out: Vec<(String, Source)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // File entries keep their canonical host name (e.g. `cf-pages`) and
    // are promoted to `EnvOverridesFile` when the matching env var is set.
    let path = home.join(REL_PATH);
    if path.exists() {
        let store = read_or_default(&path)?;
        for host in store.hosts.keys() {
            let in_env = env(&env_var_name(host))
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let source = if in_env {
                Source::EnvOverridesFile
            } else {
                Source::File
            };
            seen.insert(host.clone());
            out.push((host.clone(), source));
        }
    }

    // Env-only entries: probe each known host because the
    // `POCOPINE_<HOST>_TOKEN` encoding is not losslessly reversible
    // (hyphens and underscores collapse).
    for host in KNOWN_HOSTS {
        if seen.contains(*host) {
            continue;
        }
        if env(&env_var_name(host))
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            out.push(((*host).to_owned(), Source::Env));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn read_or_default(path: &Path) -> Result<Store> {
    if !path.exists() {
        return Ok(Store::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading credentials file {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing credentials file {}", path.display()))
}

/// `POCOPINE_<HOST>_TOKEN` with `<host>` upper-cased and any
/// non-alphanumeric byte replaced with `_` (so `cf-pages` →
/// `POCOPINE_CF_PAGES_TOKEN`).
pub fn env_var_name(host: &str) -> String {
    let normalised: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("POCOPINE_{normalised}_TOKEN")
}

fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .context("could not determine home directory ($HOME / %USERPROFILE% unset)")
}

/// Atomic, 0600-on-unix write: writes to `<path>.tmp` then renames.
fn write_secure(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");

    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("opening {}", tmp.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.sync_all()
        .with_context(|| format!("fsync {}", tmp.display()))?;
    drop(f);

    #[cfg(unix)]
    {
        // Belt-and-braces: if the file already existed without `O_CREAT`
        // taking effect on permissions, set them explicitly.
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        fs::set_permissions(&tmp, perms)
            .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
    }

    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

// ─── On-disk schema ────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Store {
    #[serde(flatten)]
    hosts: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    token: String,
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Returns a fresh temp dir to use as `$HOME`. Caller drops the dir
    /// to clean up.
    fn temp_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn env_var_name_uppercases_and_normalises() {
        assert_eq!(env_var_name("fly"), "POCOPINE_FLY_TOKEN");
        assert_eq!(env_var_name("Cloud-Run"), "POCOPINE_CLOUD_RUN_TOKEN");
        assert_eq!(env_var_name("cf-pages"), "POCOPINE_CF_PAGES_TOKEN");
    }

    #[test]
    fn known_hosts_match_canonical_adapter_names() {
        // If this fails because a new adapter exists, add its canonical
        // host name to KNOWN_HOSTS. The list is what `list_inner` probes
        // for env-only tokens.
        assert!(KNOWN_HOSTS.contains(&"fly"));
        assert!(KNOWN_HOSTS.contains(&"cf-pages"));
        let mut sorted: Vec<&str> = KNOWN_HOSTS.to_vec();
        sorted.sort();
        assert_eq!(sorted, KNOWN_HOSTS, "KNOWN_HOSTS must stay sorted");
    }

    #[test]
    fn store_then_load_roundtrip_with_no_env() {
        let home = temp_home();
        store_inner(home.path(), "fly", "tok-1").unwrap();
        let got = load_inner(home.path(), "fly", no_env).unwrap();
        assert_eq!(got, "tok-1");
    }

    #[test]
    fn stored_file_has_0600_permissions_on_unix() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let home = temp_home();
            store_inner(home.path(), "fly", "tok-1").unwrap();
            let mode = fs::metadata(home.path().join(REL_PATH))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "credentials file must be 0600 on unix");
        }
    }

    #[test]
    fn store_preserves_other_hosts() {
        let home = temp_home();
        store_inner(home.path(), "fly", "fly-tok").unwrap();
        store_inner(home.path(), "railway", "rw-tok").unwrap();
        assert_eq!(load_inner(home.path(), "fly", no_env).unwrap(), "fly-tok");
        assert_eq!(
            load_inner(home.path(), "railway", no_env).unwrap(),
            "rw-tok"
        );
    }

    #[test]
    fn env_overrides_file() {
        let home = temp_home();
        store_inner(home.path(), "fly", "file-tok").unwrap();

        // Cell so the closure can capture without lifetime gymnastics.
        let env = RefCell::new(true);
        let got = load_inner(home.path(), "fly", |k| {
            if *env.borrow() && k == "POCOPINE_FLY_TOKEN" {
                Some("env-tok".into())
            } else {
                None
            }
        })
        .unwrap();
        assert_eq!(got, "env-tok");
    }

    #[test]
    fn missing_host_with_no_file_errors_with_hint() {
        let home = temp_home();
        let err = load_inner(home.path(), "fly", no_env)
            .unwrap_err()
            .to_string();
        assert!(err.contains("POCOPINE_FLY_TOKEN"));
        assert!(err.contains("pocopine deploy auth fly"));
    }

    #[test]
    fn missing_host_with_file_present_errors_with_hint() {
        let home = temp_home();
        store_inner(home.path(), "railway", "rw").unwrap();
        let err = load_inner(home.path(), "fly", no_env)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no `[fly]` entry"));
        assert!(err.contains("pocopine deploy auth fly"));
    }

    #[test]
    fn revoke_removes_only_named_host_and_is_idempotent() {
        let home = temp_home();
        store_inner(home.path(), "fly", "f").unwrap();
        store_inner(home.path(), "railway", "r").unwrap();
        revoke_inner(home.path(), "fly").unwrap();
        assert!(load_inner(home.path(), "fly", no_env).is_err());
        assert_eq!(load_inner(home.path(), "railway", no_env).unwrap(), "r");
        // Second revoke on the same host: no-op.
        revoke_inner(home.path(), "fly").unwrap();
    }

    #[test]
    fn revoke_with_no_file_is_ok() {
        let home = temp_home();
        revoke_inner(home.path(), "fly").unwrap();
    }

    #[test]
    fn list_merges_file_and_env() {
        let home = temp_home();
        store_inner(home.path(), "fly", "ft").unwrap();
        store_inner(home.path(), "railway", "rt").unwrap();

        let env = std::collections::BTreeMap::from([
            ("POCOPINE_FLY_TOKEN".to_owned(), "env-fly".to_owned()),
            ("POCOPINE_RENDER_TOKEN".to_owned(), "env-render".to_owned()),
            ("UNRELATED".to_owned(), "noise".to_owned()),
        ]);
        let entries = list_inner(home.path(), |k| env.get(k).cloned()).unwrap();

        // Alphabetical by host: fly, railway, render.
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("fly".into(), Source::EnvOverridesFile));
        assert_eq!(entries[1], ("railway".into(), Source::File));
        assert_eq!(entries[2], ("render".into(), Source::Env));
    }

    #[test]
    fn list_ignores_empty_env_values() {
        let home = temp_home();
        let env =
            std::collections::BTreeMap::from([("POCOPINE_FLY_TOKEN".to_owned(), "".to_owned())]);
        let entries = list_inner(home.path(), |k| env.get(k).cloned()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_preserves_hyphenated_host_names() {
        // Regression for the cf-pages / cf_pages encoding collision:
        // a stored `[cf-pages]` token + the matching env var should
        // report as a single EnvOverridesFile entry, not two rows.
        let home = temp_home();
        store_inner(home.path(), "cf-pages", "file-tok").unwrap();
        let env = std::collections::BTreeMap::from([(
            "POCOPINE_CF_PAGES_TOKEN".to_owned(),
            "env-tok".to_owned(),
        )]);
        let entries = list_inner(home.path(), |k| env.get(k).cloned()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], ("cf-pages".into(), Source::EnvOverridesFile));
    }

    #[test]
    fn list_reports_env_only_known_host_with_canonical_name() {
        let home = temp_home();
        let env = std::collections::BTreeMap::from([(
            "POCOPINE_CF_PAGES_TOKEN".to_owned(),
            "env-tok".to_owned(),
        )]);
        let entries = list_inner(home.path(), |k| env.get(k).cloned()).unwrap();
        assert_eq!(entries.len(), 1);
        // Canonical host name preserved (the hyphen survives).
        assert_eq!(entries[0], ("cf-pages".into(), Source::Env));
    }
}
