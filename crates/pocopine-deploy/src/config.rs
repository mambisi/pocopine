//! Per-user host config (`~/.pocopine/config.toml`).
//!
//! Holds **non-secret** per-host defaults like `owner_id` /
//! `workspace_id` / `region`. Mirrors [`crate::credentials`] but
//! without the 0600 secrecy gate — these values are machine-local
//! convenience, not credentials.
//!
//! Adapters resolve every host-override field via [`resolve`]
//! (env → project Cargo.toml → this file). The three-tier order is
//! the same pattern as `cargo` / `gh` / `kubectl` / `flyctl`: a
//! per-user file is the default, the project's metadata overrides
//! it for that crate, and env vars override both for CI.
//!
//! Layout:
//!
//! ```toml
//! [default.render]
//! owner_id = "tea-..."
//! region   = "oregon"
//!
//! [default.railway]
//! workspace_id = "ws-..."
//! ```
//!
//! Fields are plain strings; nested structures live in Cargo.toml.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Path of the host-config file relative to `$HOME`.
pub const REL_PATH: &str = ".pocopine/config.toml";

/// Where a resolved value came from. Returned by [`resolve_with_source`]
/// so `pocopine deploy doctor` / `config list` can show the user which
/// tier won.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `POCOPINE_<HOST>_<FIELD>` environment variable.
    Env,
    /// `[package.metadata.pocopine.deploy.<host>] <field> = "..."` in
    /// the project's Cargo.toml.
    Project,
    /// `[default.<host>] <field> = "..."` in `~/.pocopine/config.toml`.
    File,
}

/// Three-tier field resolution: env → project Cargo.toml → user
/// config file. Returns the first non-empty value, or `None` if all
/// tiers are silent. The caller decides whether `None` is a hard
/// error (e.g. `owner_id` is required) or falls back to a default.
pub fn resolve(host: &str, field: &str, project: Option<&str>) -> Option<String> {
    resolve_with_source(host, field, project).map(|(v, _)| v)
}

/// Same as [`resolve`] but also returns the tier the value came from.
pub fn resolve_with_source(
    host: &str,
    field: &str,
    project: Option<&str>,
) -> Option<(String, Source)> {
    // Tier 1: env var.
    if let Ok(v) = std::env::var(env_var_name(host, field)) {
        if !v.is_empty() {
            return Some((v, Source::Env));
        }
    }
    // Tier 2: project Cargo.toml.
    if let Some(v) = project {
        if !v.is_empty() {
            return Some((v.to_owned(), Source::Project));
        }
    }
    // Tier 3: ~/.pocopine/config.toml.
    if let Ok(fields) = load_host(host) {
        if let Some(v) = fields.get(field) {
            if !v.is_empty() {
                return Some((v.clone(), Source::File));
            }
        }
    }
    None
}

/// Read all stored fields for `host` from `~/.pocopine/config.toml`.
/// Returns an empty map if the file is missing or the host has no
/// entries. Errors only on parse failures.
pub fn load_host(host: &str) -> Result<BTreeMap<String, String>> {
    load_host_inner(&home()?, host)
}

/// Persist a single `field=value` pair under `[default.<host>]`.
/// Idempotent: other hosts' and fields' entries are preserved.
pub fn store_field(host: &str, field: &str, value: &str) -> Result<()> {
    store_field_inner(&home()?, host, field, value)
}

/// Remove `field` from `[default.<host>]`. Returns `Ok(())` even if
/// it didn't exist.
pub fn revoke_field(host: &str, field: &str) -> Result<()> {
    revoke_field_inner(&home()?, host, field)
}

/// `(host, field, source)` tuples for every (host, field) we can see —
/// stored on disk plus probed env vars for `(host, KNOWN_FIELDS)`.
/// Ordering: host-alphabetical, then field-alphabetical.
pub fn list() -> Result<Vec<(String, String, Source)>> {
    list_inner(&home()?, env_lookup)
}

/// Convert a host name and field name into the env-var key that wins
/// over both tiers. Both segments are upper-cased and non-alphanumeric
/// bytes collapse to `_` — so `(railway, workspace_id)` →
/// `POCOPINE_RAILWAY_WORKSPACE_ID`, `(cf-pages, project_id)` →
/// `POCOPINE_CF_PAGES_PROJECT_ID`.
pub fn env_var_name(host: &str, field: &str) -> String {
    let h = normalise(host);
    let f = normalise(field);
    format!("POCOPINE_{h}_{f}")
}

/// Field names probed when listing env-only entries. Adapters extend
/// this list as new override fields are added; the list is not the
/// source of truth for the resolver itself (`resolve` accepts any
/// field name), only for the `list` command.
pub const KNOWN_FIELDS: &[&str] = &[
    "image_registry",
    "owner_id",
    "plan",
    "primary_region",
    "project",
    "region",
    "registry_username",
    "workspace_id",
];

// ─── Internal seams (used by tests) ─────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default, rename = "default")]
    defaults: BTreeMap<String, BTreeMap<String, toml::Value>>,
}

fn load_host_inner(home: &Path, host: &str) -> Result<BTreeMap<String, String>> {
    let path = home.join(REL_PATH);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let store = read_or_default(&path)?;
    let Some(host_table) = store.defaults.get(host) else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for (k, v) in host_table {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_owned());
        }
    }
    Ok(out)
}

fn store_field_inner(home: &Path, host: &str, field: &str, value: &str) -> Result<()> {
    let path = home.join(REL_PATH);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating directory {}", dir.display()))?;
    }
    let mut store = read_or_default(&path)?;
    store
        .defaults
        .entry(host.to_owned())
        .or_default()
        .insert(field.to_owned(), toml::Value::String(value.to_owned()));
    let raw = toml::to_string_pretty(&store).context("serialising host config")?;
    fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))
}

fn revoke_field_inner(home: &Path, host: &str, field: &str) -> Result<()> {
    let path = home.join(REL_PATH);
    if !path.exists() {
        return Ok(());
    }
    let mut store = read_or_default(&path)?;
    if let Some(host_table) = store.defaults.get_mut(host) {
        if host_table.remove(field).is_none() {
            return Ok(());
        }
        if host_table.is_empty() {
            store.defaults.remove(host);
        }
    } else {
        return Ok(());
    }
    let raw = toml::to_string_pretty(&store).context("serialising host config")?;
    fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))
}

fn list_inner<F>(home: &Path, env: F) -> Result<Vec<(String, String, Source)>>
where
    F: Fn(&str) -> Option<String>,
{
    use crate::credentials;
    use std::collections::HashSet;

    let mut out: Vec<(String, String, Source)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    let path = home.join(REL_PATH);
    if path.exists() {
        let store = read_or_default(&path)?;
        for (host, fields) in &store.defaults {
            for field in fields.keys() {
                let in_env = env(&env_var_name(host, field))
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                let source = if in_env { Source::Env } else { Source::File };
                seen.insert((host.clone(), field.clone()));
                out.push((host.clone(), field.clone(), source));
            }
        }
    }

    // Probe env vars for (KNOWN_HOSTS × KNOWN_FIELDS) we haven't already
    // surfaced via the file. As with `credentials::list`, the env-var
    // encoding isn't losslessly reversible (`cf-pages` and `cf_pages`
    // both encode to `POCOPINE_CF_PAGES_*`) so we enumerate from the
    // known list instead of scanning the environment.
    for host in credentials::KNOWN_HOSTS {
        for field in KNOWN_FIELDS {
            let key = (host.to_string(), field.to_string());
            if seen.contains(&key) {
                continue;
            }
            if env(&env_var_name(host, field))
                .map(|v| !v.is_empty())
                .unwrap_or(false)
            {
                out.push((key.0, key.1, Source::Env));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    Ok(out)
}

fn read_or_default(path: &Path) -> Result<Store> {
    if !path.exists() {
        return Ok(Store::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading host-config file {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing host-config file {}", path.display()))
}

fn normalise(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("$HOME is unset; cannot resolve ~/.pocopine/config.toml location")
}

fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tmp_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("create tempdir")
    }

    #[test]
    fn env_var_name_normalises_host_and_field() {
        assert_eq!(
            env_var_name("render", "owner_id"),
            "POCOPINE_RENDER_OWNER_ID"
        );
        assert_eq!(
            env_var_name("cf-pages", "project_id"),
            "POCOPINE_CF_PAGES_PROJECT_ID"
        );
        assert_eq!(
            env_var_name("railway", "workspace_id"),
            "POCOPINE_RAILWAY_WORKSPACE_ID"
        );
    }

    #[test]
    fn load_host_returns_empty_when_file_missing() {
        let home = tmp_home();
        assert!(load_host_inner(home.path(), "render").unwrap().is_empty());
    }

    #[test]
    fn store_and_load_field_roundtrip() {
        let home = tmp_home();
        store_field_inner(home.path(), "render", "owner_id", "tea-abc").unwrap();
        store_field_inner(home.path(), "render", "region", "oregon").unwrap();
        store_field_inner(home.path(), "railway", "workspace_id", "ws-xyz").unwrap();

        let render = load_host_inner(home.path(), "render").unwrap();
        assert_eq!(render.get("owner_id").map(String::as_str), Some("tea-abc"));
        assert_eq!(render.get("region").map(String::as_str), Some("oregon"));

        let railway = load_host_inner(home.path(), "railway").unwrap();
        assert_eq!(
            railway.get("workspace_id").map(String::as_str),
            Some("ws-xyz")
        );
    }

    #[test]
    fn revoke_field_removes_only_that_field() {
        let home = tmp_home();
        store_field_inner(home.path(), "render", "owner_id", "tea-abc").unwrap();
        store_field_inner(home.path(), "render", "region", "oregon").unwrap();
        revoke_field_inner(home.path(), "render", "owner_id").unwrap();

        let render = load_host_inner(home.path(), "render").unwrap();
        assert!(!render.contains_key("owner_id"));
        assert_eq!(render.get("region").map(String::as_str), Some("oregon"));
    }

    #[test]
    fn revoke_last_field_drops_empty_host_table() {
        let home = tmp_home();
        store_field_inner(home.path(), "render", "owner_id", "tea-abc").unwrap();
        revoke_field_inner(home.path(), "render", "owner_id").unwrap();

        let raw = fs::read_to_string(home.path().join(REL_PATH)).unwrap();
        // Empty `[default]` table is fine; an empty `[default.render]`
        // would re-appear on next list — make sure we collapsed it.
        assert!(
            !raw.contains("[default.render]"),
            "empty host table left behind: {raw}",
        );
    }

    #[test]
    fn list_merges_file_and_env_with_correct_source() {
        let home = tmp_home();
        store_field_inner(home.path(), "render", "owner_id", "tea-abc").unwrap();

        let env: HashMap<&str, &str> =
            HashMap::from([("POCOPINE_RAILWAY_WORKSPACE_ID", "ws-from-env")]);
        let envfn = |k: &str| env.get(k).map(|s| s.to_string());

        let listed = list_inner(home.path(), envfn).unwrap();
        // Host-then-field alphabetical.
        let render_entry = listed
            .iter()
            .find(|(h, f, _)| h == "render" && f == "owner_id")
            .expect("render/owner_id present");
        assert_eq!(render_entry.2, Source::File);
        let railway_entry = listed
            .iter()
            .find(|(h, f, _)| h == "railway" && f == "workspace_id")
            .expect("railway/workspace_id present via env");
        assert_eq!(railway_entry.2, Source::Env);
    }

    #[test]
    fn list_marks_env_overriding_file() {
        let home = tmp_home();
        store_field_inner(home.path(), "render", "owner_id", "tea-abc").unwrap();
        let env: HashMap<&str, &str> =
            HashMap::from([("POCOPINE_RENDER_OWNER_ID", "tea-from-env")]);
        let envfn = |k: &str| env.get(k).map(|s| s.to_string());

        let listed = list_inner(home.path(), envfn).unwrap();
        let render_entry = listed
            .iter()
            .find(|(h, f, _)| h == "render" && f == "owner_id")
            .unwrap();
        assert_eq!(render_entry.2, Source::Env);
    }
}
