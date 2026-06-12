//! Credentials store for host API tokens (RFC 080 §5.5) and the
//! asset-bucket access keys (RFC 100 §5).
//!
//! Tokens live in `~/.pocopine/credentials.toml` with mode `0600`. Env
//! vars (`POCOPINE_<HOST>_TOKEN`) take precedence over the file so CI
//! runs without touching disk.
//!
//! Layout:
//!
//! ```toml
//! [render]
//! token = "rnd_..."
//!
//! [railway]
//! token = "rw_..."
//!
//! # RFC 100 — asset-bucket keys (`pocopine assets auth`). The
//! # `assets` table name is reserved: it is not a deploy host.
//! [assets]
//! access_key_id = "AKIA..."
//! secret_access_key = "..."
//! ```
//!
//! Hosts not present in this file fall back to env, then to a hard
//! error pointing the user at `pocopine deploy auth <host>`. Asset
//! keys fall back to `POCOPINE_ASSETS_ACCESS_KEY_ID` /
//! `POCOPINE_ASSETS_SECRET_ACCESS_KEY` (the same env vars the Mode B
//! serving proxy reads), then to a hard error pointing at
//! `pocopine assets auth`.

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
    "gh-pages",
    "netlify",
    "railway",
    "render",
    "vercel",
];

// ─── RFC 100 — asset-bucket access keys ────────────────────────────────

/// Static S3 access keys for the RFC-100 asset bucket. Deploy-time
/// auth only — `pocopine assets push` signs uploads with these; app
/// runtime secrets never live in `credentials.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetsCredentials {
    /// S3 access key id.
    pub access_key_id: String,
    /// S3 secret access key.
    pub secret_access_key: String,
}

/// Env var overriding the stored asset access key id. Shared with the
/// Mode B serving proxy (`pocopine-server`), so CI and the deployed
/// web service speak the same names.
pub const ASSETS_ACCESS_KEY_ID_ENV: &str = "POCOPINE_ASSETS_ACCESS_KEY_ID";
/// Env var overriding the stored asset secret access key.
pub const ASSETS_SECRET_ACCESS_KEY_ENV: &str = "POCOPINE_ASSETS_SECRET_ACCESS_KEY";

/// Look up the asset-bucket access keys. Both env vars set and
/// non-empty win over the file (mixed env/file is rejected as a
/// misconfiguration rather than silently half-applied); otherwise the
/// `[assets]` entry in `~/.pocopine/credentials.toml`; otherwise an
/// error suggesting `pocopine assets auth`.
pub fn load_assets() -> Result<AssetsCredentials> {
    load_assets_inner(&home()?, env_lookup)
}

/// Persist the asset-bucket keys to the credentials file (mode
/// `0600`). Host token entries are preserved.
pub fn store_assets(credentials: &AssetsCredentials) -> Result<()> {
    store_assets_inner(&home()?, credentials)
}

/// Remove the stored asset-bucket keys. Idempotent.
pub fn revoke_assets() -> Result<()> {
    revoke_assets_inner(&home()?)
}

fn load_assets_inner<F>(home: &Path, env: F) -> Result<AssetsCredentials>
where
    F: Fn(&str) -> Option<String>,
{
    let env_id = env(ASSETS_ACCESS_KEY_ID_ENV).filter(|v| !v.is_empty());
    let env_secret = env(ASSETS_SECRET_ACCESS_KEY_ENV).filter(|v| !v.is_empty());
    match (env_id, env_secret) {
        (Some(access_key_id), Some(secret_access_key)) => {
            return Ok(AssetsCredentials {
                access_key_id,
                secret_access_key,
            });
        }
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => bail!(
            "only one of ${ASSETS_ACCESS_KEY_ID_ENV} / ${ASSETS_SECRET_ACCESS_KEY_ENV} is set — \
             set both (or neither, to use ~/.pocopine/credentials.toml)."
        ),
    }

    let path = home.join(REL_PATH);
    let hint = format!(
        "Set ${ASSETS_ACCESS_KEY_ID_ENV} + ${ASSETS_SECRET_ACCESS_KEY_ENV} or run \
         `pocopine assets auth` to store keys."
    );
    if !path.exists() {
        bail!("no asset-bucket credentials. {hint}");
    }
    let store = read_or_default(&path)?;
    let entry = store
        .assets
        .with_context(|| format!("no `[assets]` entry in {}. {hint}", path.display()))?;
    Ok(AssetsCredentials {
        access_key_id: entry.access_key_id,
        secret_access_key: entry.secret_access_key,
    })
}

fn store_assets_inner(home: &Path, credentials: &AssetsCredentials) -> Result<()> {
    let path = home.join(REL_PATH);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating directory {}", dir.display()))?;
    }
    let mut store = read_or_default(&path)?;
    store.assets = Some(AssetsEntry {
        access_key_id: credentials.access_key_id.clone(),
        secret_access_key: credentials.secret_access_key.clone(),
    });
    let raw = toml::to_string_pretty(&store).context("serialising credentials")?;
    write_secure(&path, raw.as_bytes())
}

fn revoke_assets_inner(home: &Path) -> Result<()> {
    let path = home.join(REL_PATH);
    if !path.exists() {
        return Ok(());
    }
    let mut store = read_or_default(&path)?;
    if store.assets.take().is_none() {
        return Ok(());
    }
    let raw = toml::to_string_pretty(&store).context("serialising credentials")?;
    write_secure(&path, raw.as_bytes())
}

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

    // Scope the file so it is closed (dropped) before the rename below —
    // an open handle can block a rename on Windows. A block, rather than
    // an explicit `drop(f)`, because `std::fs::File` is a no-op stub on
    // wasm where `drop()` of a non-`Drop` type is a clippy error.
    {
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
    }

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
    /// RFC 100 — asset-bucket access keys. A named field so the
    /// reserved `[assets]` table never lands in the flattened host
    /// map (its schema differs from a host token entry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assets: Option<AssetsEntry>,
    #[serde(flatten)]
    hosts: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AssetsEntry {
    access_key_id: String,
    secret_access_key: String,
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
        assert_eq!(env_var_name("render"), "POCOPINE_RENDER_TOKEN");
        assert_eq!(env_var_name("Cloud-Run"), "POCOPINE_CLOUD_RUN_TOKEN");
        assert_eq!(env_var_name("cf-pages"), "POCOPINE_CF_PAGES_TOKEN");
    }

    #[test]
    fn known_hosts_match_canonical_adapter_names() {
        // If this fails because a new adapter exists, add its canonical
        // host name to KNOWN_HOSTS. The list is what `list_inner` probes
        // for env-only tokens.
        assert!(KNOWN_HOSTS.contains(&"render"));
        assert!(KNOWN_HOSTS.contains(&"cf-pages"));
        let mut sorted: Vec<&str> = KNOWN_HOSTS.to_vec();
        sorted.sort();
        assert_eq!(sorted, KNOWN_HOSTS, "KNOWN_HOSTS must stay sorted");
    }

    #[test]
    fn store_then_load_roundtrip_with_no_env() {
        let home = temp_home();
        store_inner(home.path(), "render", "tok-1").unwrap();
        let got = load_inner(home.path(), "render", no_env).unwrap();
        assert_eq!(got, "tok-1");
    }

    #[test]
    fn stored_file_has_0600_permissions_on_unix() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let home = temp_home();
            store_inner(home.path(), "render", "tok-1").unwrap();
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
        store_inner(home.path(), "render", "render-tok").unwrap();
        store_inner(home.path(), "railway", "rw-tok").unwrap();
        assert_eq!(
            load_inner(home.path(), "render", no_env).unwrap(),
            "render-tok"
        );
        assert_eq!(
            load_inner(home.path(), "railway", no_env).unwrap(),
            "rw-tok"
        );
    }

    #[test]
    fn env_overrides_file() {
        let home = temp_home();
        store_inner(home.path(), "render", "file-tok").unwrap();

        // Cell so the closure can capture without lifetime gymnastics.
        let env = RefCell::new(true);
        let got = load_inner(home.path(), "render", |k| {
            if *env.borrow() && k == "POCOPINE_RENDER_TOKEN" {
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
        let err = load_inner(home.path(), "render", no_env)
            .unwrap_err()
            .to_string();
        assert!(err.contains("POCOPINE_RENDER_TOKEN"));
        assert!(err.contains("pocopine deploy auth render"));
    }

    #[test]
    fn missing_host_with_file_present_errors_with_hint() {
        let home = temp_home();
        store_inner(home.path(), "railway", "rw").unwrap();
        let err = load_inner(home.path(), "render", no_env)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no `[render]` entry"));
        assert!(err.contains("pocopine deploy auth render"));
    }

    #[test]
    fn revoke_removes_only_named_host_and_is_idempotent() {
        let home = temp_home();
        store_inner(home.path(), "render", "f").unwrap();
        store_inner(home.path(), "railway", "r").unwrap();
        revoke_inner(home.path(), "render").unwrap();
        assert!(load_inner(home.path(), "render", no_env).is_err());
        assert_eq!(load_inner(home.path(), "railway", no_env).unwrap(), "r");
        // Second revoke on the same host: no-op.
        revoke_inner(home.path(), "render").unwrap();
    }

    #[test]
    fn revoke_with_no_file_is_ok() {
        let home = temp_home();
        revoke_inner(home.path(), "render").unwrap();
    }

    #[test]
    fn list_merges_file_and_env() {
        let home = temp_home();
        store_inner(home.path(), "netlify", "ft").unwrap();
        store_inner(home.path(), "railway", "rt").unwrap();

        let env = std::collections::BTreeMap::from([
            (
                "POCOPINE_NETLIFY_TOKEN".to_owned(),
                "env-netlify".to_owned(),
            ),
            ("POCOPINE_RENDER_TOKEN".to_owned(), "env-render".to_owned()),
            ("UNRELATED".to_owned(), "noise".to_owned()),
        ]);
        let entries = list_inner(home.path(), |k| env.get(k).cloned()).unwrap();

        // Alphabetical by host: netlify, railway, render.
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("netlify".into(), Source::EnvOverridesFile));
        assert_eq!(entries[1], ("railway".into(), Source::File));
        assert_eq!(entries[2], ("render".into(), Source::Env));
    }

    #[test]
    fn list_ignores_empty_env_values() {
        let home = temp_home();
        let env =
            std::collections::BTreeMap::from([("POCOPINE_RENDER_TOKEN".to_owned(), "".to_owned())]);
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

    // ─── RFC 100 — asset-bucket access keys ────────────────────────────

    fn assets_creds() -> AssetsCredentials {
        AssetsCredentials {
            access_key_id: "AKIA-test".into(),
            secret_access_key: "shh-test".into(),
        }
    }

    #[test]
    fn assets_store_then_load_roundtrip() {
        let home = temp_home();
        store_assets_inner(home.path(), &assets_creds()).unwrap();
        let got = load_assets_inner(home.path(), no_env).unwrap();
        assert_eq!(got, assets_creds());
    }

    #[test]
    fn assets_entry_coexists_with_host_tokens() {
        let home = temp_home();
        store_inner(home.path(), "railway", "rw-tok").unwrap();
        store_assets_inner(home.path(), &assets_creds()).unwrap();
        // Both survive each other's writes...
        assert_eq!(
            load_inner(home.path(), "railway", no_env).unwrap(),
            "rw-tok"
        );
        assert_eq!(
            load_assets_inner(home.path(), no_env)
                .unwrap()
                .access_key_id,
            "AKIA-test"
        );
        // ...and the reserved `assets` table does not leak into the
        // host list.
        let hosts = list_inner(home.path(), no_env).unwrap();
        assert_eq!(hosts, vec![("railway".into(), Source::File)]);
    }

    #[test]
    fn assets_env_pair_overrides_file() {
        let home = temp_home();
        store_assets_inner(home.path(), &assets_creds()).unwrap();
        let env = std::collections::BTreeMap::from([
            (ASSETS_ACCESS_KEY_ID_ENV.to_owned(), "env-id".to_owned()),
            (
                ASSETS_SECRET_ACCESS_KEY_ENV.to_owned(),
                "env-secret".to_owned(),
            ),
        ]);
        let got = load_assets_inner(home.path(), |k| env.get(k).cloned()).unwrap();
        assert_eq!(got.access_key_id, "env-id");
        assert_eq!(got.secret_access_key, "env-secret");
    }

    #[test]
    fn assets_half_set_env_pair_is_an_error() {
        let home = temp_home();
        store_assets_inner(home.path(), &assets_creds()).unwrap();
        let env = std::collections::BTreeMap::from([(
            ASSETS_ACCESS_KEY_ID_ENV.to_owned(),
            "env-id".to_owned(),
        )]);
        let err = load_assets_inner(home.path(), |k| env.get(k).cloned())
            .unwrap_err()
            .to_string();
        assert!(err.contains("only one of"));
    }

    #[test]
    fn assets_missing_errors_with_auth_hint() {
        let home = temp_home();
        let err = load_assets_inner(home.path(), no_env)
            .unwrap_err()
            .to_string();
        assert!(err.contains("pocopine assets auth"));
        assert!(err.contains(ASSETS_ACCESS_KEY_ID_ENV));
    }

    #[test]
    fn assets_revoke_is_idempotent_and_preserves_hosts() {
        let home = temp_home();
        store_inner(home.path(), "render", "tok").unwrap();
        store_assets_inner(home.path(), &assets_creds()).unwrap();
        revoke_assets_inner(home.path()).unwrap();
        assert!(load_assets_inner(home.path(), no_env).is_err());
        assert_eq!(load_inner(home.path(), "render", no_env).unwrap(), "tok");
        revoke_assets_inner(home.path()).unwrap(); // second time: no-op
    }
}
