//! Parsed `[deploy]` block from `Pocopine.toml` plus computed values.
//!
//! The CLI fills in `app_name` + `git_sha` + the `uses_*` build-artefact
//! flags (RFC 080 §9). Everything else comes straight from TOML with
//! sensible defaults.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Static,
    Fullstack,
}

#[derive(Debug, Clone)]
pub struct DeploySpec {
    pub app_name: String,
    /// Unsuffixed `[package].name` for `cargo build -p <pkg>`. Distinct
    /// from `app_name`, which `--prod` suffixes with `-production`.
    pub package_name: String,
    pub git_sha: String,
    /// Project's `origin` URL, used to derive a default container
    /// registry — see [`crate::resolve_registry`].
    pub git_remote: Option<String>,
    pub mode: Mode,
    pub processes: BTreeMap<String, ProcessSpec>,
    pub services: BTreeMap<String, ServiceSpec>,
    pub env: BTreeMap<String, EnvValue>,
    /// Host-namespaced overrides (`[deploy.render]`, …). Read via
    /// [`DeploySpec::host_override`].
    pub host_overrides: BTreeMap<String, toml::Value>,

    // Build-artefact metadata (RFC 080 §9 — populated by `pocopine build`).
    pub uses_jobs: bool,
    pub uses_collab: bool,
    pub uses_storage: bool,
    pub uses_websocket: bool,

    pub first_deploy: bool,

    /// `--skip-build` was passed; reuse the host's pushed image.
    pub skip_build: bool,

    /// `Some("production")` for `--prod`. When set, `parse` merges
    /// `[deploy.<env>]` into the base spec and suffixes `app_name`
    /// with `-<env>` so envs deploy to distinct host apps.
    pub environment: Option<String>,

    /// Project path relative to the artefact-build context (the cargo
    /// workspace root). Empty for standalone repos, `"examples/keep"`
    /// for workspace members. Docker COPYs and static-dist assembly use
    /// this same prefix.
    pub workspace_subpath: String,

    /// `true` when `rust-toolchain.toml` exists at the workspace root.
    /// Triggers a cached-layer rustup pre-install in the Dockerfile —
    /// `pocopine-macros`'s `proc_macro_span` needs the pinned nightly.
    pub has_rust_toolchain: bool,

    /// Project-relative entries copied into `$POCOPINE_DIST`. Default
    /// `["index.html", "pkg"]`; users add `app.css`, `public/`, etc.
    /// via `[deploy] static_files`.
    pub static_files: Vec<String>,

    /// Override for the build-artefact output directory (where the
    /// Dockerfile, dockerignore, and host-specific config files are
    /// written, relative to the project root). When `None` the default
    /// [`crate::common::BUILD_DIR`] (`".pocopine/build"`) is used. Set
    /// via `[package.metadata.pocopine.deploy] build_dir = "..."` in
    /// Cargo.toml.
    pub build_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub bin: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub healthcheck: Option<String>,
    #[serde(default)]
    pub scale: Scale,
    #[serde(default)]
    pub public: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scale {
    #[serde(default = "default_scale_min")]
    pub min: u32,
    #[serde(default = "default_scale_max")]
    pub max: u32,
}

impl Default for Scale {
    fn default() -> Self {
        Self {
            min: default_scale_min(),
            max: default_scale_max(),
        }
    }
}

fn default_scale_min() -> u32 {
    1
}
fn default_scale_max() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSpec {
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvValue {
    /// `KEY = "literal value"` — baked into the image.
    Literal(String),
    /// `KEY = { from = "secret" }` / `{ from = "env" }`.
    Indirect { from: EnvSource },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvSource {
    Secret,
    Env,
}

impl ProcessSpec {
    /// A process is public iff it has a port or is explicitly `public = true`.
    pub fn is_public(&self) -> bool {
        self.public.unwrap_or(self.port.is_some())
    }
}

impl DeploySpec {
    /// Postgres is required if the spec declares it, or if the build artefact
    /// reports it touches storage / collab (RFC 080 §4.1 rules).
    pub fn requires_postgres(&self) -> bool {
        self.services.get("postgres").is_some_and(|s| s.required)
            || self.uses_storage
            || self.uses_collab
    }

    /// Redis is required if the spec declares it, or if there's a worker /
    /// collab handler that needs Streams + Pub/Sub.
    pub fn requires_redis(&self) -> bool {
        self.services.get("redis").is_some_and(|s| s.required)
            || self.has_process("worker")
            || self.uses_collab
    }

    pub fn has_process(&self, name: &str) -> bool {
        self.processes.contains_key(name)
    }

    pub fn process(&self, name: &str) -> Option<&ProcessSpec> {
        self.processes.get(name)
    }

    pub fn processes(&self) -> impl Iterator<Item = (&str, &ProcessSpec)> {
        self.processes.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn has_secret(&self, key: &str) -> bool {
        matches!(self.env.get(key), Some(EnvValue::Indirect { .. }))
    }

    /// Decode the `[deploy.<host>]` override block as a typed adapter config.
    pub fn host_override<T: serde::de::DeserializeOwned>(
        &self,
        host: &str,
    ) -> anyhow::Result<Option<T>> {
        match self.host_overrides.get(host) {
            None => Ok(None),
            Some(v) => Ok(Some(v.clone().try_into()?)),
        }
    }

    /// Literal env-pairs to pass to a service. `from = "secret"` / `"env"`
    /// values are resolved by the deploy step against the host's secret
    /// store / the user's shell env, not baked into the spec.
    pub fn env_for_service(&self, _process: &str) -> Vec<(String, String)> {
        self.env
            .iter()
            .filter_map(|(k, v)| match v {
                EnvValue::Literal(s) => Some((k.clone(), s.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn is_first_deploy(&self) -> bool {
        self.first_deploy
    }

    /// Effective build directory (project-root-relative). The default
    /// is `.pocopine/build` (see [`crate::common::BUILD_DIR`]); projects
    /// override via `[package.metadata.pocopine.deploy] build_dir =
    /// "..."`. Mirrors how `cargo` honours `[build] target-dir` /
    /// `CARGO_TARGET_DIR`.
    pub fn build_dir(&self) -> &str {
        self.build_dir
            .as_deref()
            .unwrap_or(crate::common::BUILD_DIR)
    }

    /// Path of the rendered `Dockerfile` relative to the docker-build
    /// context (the workspace root). The CLI writes generated artefacts
    /// under each project's [build dir](Self::build_dir) (default
    /// `.pocopine/build/`), so the build-context-relative path is
    /// `<subpath>/<build_dir>/Dockerfile` for workspace members and
    /// `<build_dir>/Dockerfile` for standalone projects. Adapters pass
    /// this to `docker build -f`.
    pub fn dockerfile_path(&self) -> String {
        let build_dir = self.build_dir();
        if self.workspace_subpath.is_empty() {
            format!("{build_dir}/Dockerfile")
        } else {
            format!("{}/{}/Dockerfile", self.workspace_subpath, build_dir)
        }
    }
}

/// Parse a `[deploy]` table value (already extracted from `Pocopine.toml`)
/// into a [`DeploySpec`]. `app_name` and `git_sha` come from the CLI's
/// own discovery; build-artefact flags are filled in later.
///
/// When `environment` is `Some(env)`:
///
/// 1. Any `[deploy.<env>.env]` entries override the base `[deploy.env]`
///    map (per-environment env vars — `LOG_LEVEL`, feature flags, …).
/// 2. Any `[deploy.<env>.<host>]` blocks replace the corresponding
///    base `[deploy.<host>]` block for adapter overrides
///    (`org`, regions, etc.).
/// 3. `app_name` is suffixed with `-<env>` so production and staging
///    target distinct host apps.
pub fn parse(
    table: toml::Value,
    app_name: String,
    git_sha: String,
    environment: Option<String>,
) -> anyhow::Result<DeploySpec> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default = "default_mode")]
        mode: Mode,
        #[serde(default)]
        processes: BTreeMap<String, ProcessSpec>,
        #[serde(default)]
        services: BTreeMap<String, ServiceSpec>,
        #[serde(default)]
        env: BTreeMap<String, EnvValue>,
        #[serde(default)]
        static_files: Option<Vec<String>>,
        /// `[deploy] build_dir = "..."` — override where rendered
        /// artefacts (Dockerfile, host config) land relative to the
        /// project root. Default `.pocopine/build`.
        #[serde(default)]
        build_dir: Option<String>,
        #[serde(flatten)]
        host_overrides: BTreeMap<String, toml::Value>,
    }
    fn default_mode() -> Mode {
        Mode::Fullstack
    }

    let raw: Raw = table.try_into()?;
    let static_files = raw
        .static_files
        .unwrap_or_else(|| vec!["index.html".to_string(), "pkg".to_string()]);
    for entry in &static_files {
        if entry.is_empty() || entry.starts_with('/') || entry.split('/').any(|seg| seg == "..") {
            anyhow::bail!(
                "[deploy] static_files entries must be project-relative and contain no `..` segments; got `{entry}`",
            );
        }
    }
    if let Some(dir) = raw.build_dir.as_deref() {
        // Same validation as static_files — keep it inside the project
        // tree, no traversal, no absolute paths.
        if dir.is_empty() || dir.starts_with('/') || dir.split('/').any(|seg| seg == "..") {
            anyhow::bail!(
                "[deploy] build_dir must be project-relative and contain no `..` segments; got `{dir}`",
            );
        }
    }

    let package_name = app_name.clone();
    let final_app_name = match environment.as_deref() {
        Some(env) => format!("{app_name}-{env}"),
        None => app_name,
    };

    let mut spec = DeploySpec {
        app_name: final_app_name,
        package_name,
        git_sha,
        git_remote: None,
        mode: raw.mode,
        processes: raw.processes,
        services: raw.services,
        env: raw.env,
        host_overrides: raw.host_overrides,
        uses_jobs: false,
        uses_collab: false,
        uses_storage: false,
        uses_websocket: false,
        first_deploy: false,
        skip_build: false,
        environment: environment.clone(),
        workspace_subpath: String::new(),
        has_rust_toolchain: false,
        static_files,
        build_dir: raw.build_dir,
    };

    if let Some(env) = environment.as_deref()
        && let Some(env_block) = spec.host_overrides.remove(env)
    {
        apply_env_overrides(&mut spec, env_block)?;
    }

    Ok(spec)
}

/// Merge a `[deploy.<env>]` sub-table into the base spec:
/// - `env_block.env.<k>` overrides `spec.env.<k>`.
/// - `env_block.<host>` replaces `spec.host_overrides.<host>`.
///
/// Unknown fields are accepted but ignored (future-compatible).
fn apply_env_overrides(spec: &mut DeploySpec, env_block: toml::Value) -> anyhow::Result<()> {
    let toml::Value::Table(map) = env_block else {
        anyhow::bail!("[deploy.<env>] override must be a table, got {env_block:?}");
    };

    for (key, value) in map {
        match key.as_str() {
            "env" => {
                let env_overrides: BTreeMap<String, EnvValue> = value
                    .try_into()
                    .map_err(|e| anyhow::anyhow!("parsing [deploy.<env>.env]: {e}"))?;
                for (k, v) in env_overrides {
                    spec.env.insert(k, v);
                }
            }
            // Per-env overrides for the process graph / service set /
            // mode are not yet implemented. Refuse loudly instead of
            // silently routing them into host_overrides — otherwise a
            // production deploy could ship the base topology while
            // the user thinks they overrode it.
            "processes" | "services" | "mode" => {
                anyhow::bail!(
                    "[deploy.<env>.{key}] overrides are not yet implemented (Phase 11 follow-up). Move the field to the base [deploy] block, or split prod/staging into separate apps for now.",
                );
            }
            // Anything else is a host-override block (e.g.
            // [deploy.production.render]) that replaces the base
            // [deploy.<host>] block for the duration of this deploy.
            _ => {
                spec.host_overrides.insert(key, value);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_table() -> toml::Value {
        toml::toml! {
            mode = "fullstack"

            [processes.web]
            bin = "server"
            port = 8080

            [services.postgres]
            required = true

            [env]
            LOG_LEVEL = "info"

            [render]
            org = "personal"
            regions = ["fra"]

            [production]
            [production.env]
            LOG_LEVEL = "warn"
            SENTRY_DSN = "https://prod-only"

            [production.render]
            org = "myorg"
            regions = ["fra", "iad"]
        }
        .into()
    }

    #[test]
    fn no_environment_keeps_app_name_and_base_env() {
        let spec = parse(base_table(), "myapp".into(), "sha".into(), None).unwrap();
        assert_eq!(spec.app_name, "myapp");
        assert!(matches!(
            spec.env.get("LOG_LEVEL"),
            Some(EnvValue::Literal(v)) if v == "info",
        ));
        // The `production` block survives unprocessed (no env scope).
        assert!(spec.host_overrides.contains_key("production"));
    }

    #[test]
    fn production_environment_suffixes_app_name() {
        let spec = parse(
            base_table(),
            "myapp".into(),
            "sha".into(),
            Some("production".into()),
        )
        .unwrap();
        assert_eq!(spec.app_name, "myapp-production");
        assert_eq!(spec.environment.as_deref(), Some("production"));
    }

    #[test]
    fn env_block_merges_env_vars_over_base() {
        let spec = parse(
            base_table(),
            "myapp".into(),
            "sha".into(),
            Some("production".into()),
        )
        .unwrap();
        assert!(matches!(
            spec.env.get("LOG_LEVEL"),
            Some(EnvValue::Literal(v)) if v == "warn",
        ));
        assert!(matches!(
            spec.env.get("SENTRY_DSN"),
            Some(EnvValue::Literal(v)) if v == "https://prod-only",
        ));
    }

    #[test]
    fn env_block_replaces_host_override() {
        let spec = parse(
            base_table(),
            "myapp".into(),
            "sha".into(),
            Some("production".into()),
        )
        .unwrap();
        // [deploy.production.render] replaces [deploy.render].
        let render_block = spec
            .host_overrides
            .get("render")
            .expect("render block present after env merge");
        assert_eq!(
            render_block.get("org").and_then(|v| v.as_str()),
            Some("myorg"),
        );
        // The `production` key itself is consumed (removed) so adapters
        // looking up `host_overrides["production"]` get None.
        assert!(!spec.host_overrides.contains_key("production"));
    }

    #[test]
    fn unsupported_env_override_keys_are_refused() {
        // [deploy.production.processes] / .services / .mode are not yet
        // wired through. The merge must fail loudly so users don't think
        // a typo'd or unsupported override silently took effect.
        let table = toml::toml! {
            mode = "fullstack"

            [processes.web]
            bin = "server"
            port = 8080

            [production]
            [production.processes.web]
            bin = "server-prod"
        };
        let err = parse(
            table.into(),
            "myapp".into(),
            "sha".into(),
            Some("production".into()),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("[deploy.<env>.processes]"));
        assert!(err.contains("not yet implemented"));
    }

    #[test]
    fn parse_defaults_static_files_to_index_html_and_pkg() {
        let mut t = toml::value::Table::new();
        t.insert("mode".into(), toml::Value::String("fullstack".into()));
        let spec = parse(toml::Value::Table(t), "myapp".into(), "sha".into(), None).unwrap();
        assert_eq!(spec.static_files, vec!["index.html", "pkg"]);
    }

    #[test]
    fn parse_accepts_explicit_static_files() {
        let table = toml::toml! {
            mode = "fullstack"
            static_files = ["index.html", "pkg", "app.css", "public"]
        };
        let spec = parse(table.into(), "myapp".into(), "sha".into(), None).unwrap();
        assert_eq!(
            spec.static_files,
            vec!["index.html", "pkg", "app.css", "public"],
        );
    }

    #[test]
    fn parse_rejects_static_files_with_parent_segment() {
        let table = toml::toml! {
            mode = "fullstack"
            static_files = ["index.html", "../secrets"]
        };
        let err = parse(table.into(), "myapp".into(), "sha".into(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("static_files"));
        assert!(err.contains(".."));
    }

    #[test]
    fn parse_rejects_static_files_with_absolute_path() {
        let table = toml::toml! {
            mode = "fullstack"
            static_files = ["/etc/passwd"]
        };
        let err = parse(table.into(), "myapp".into(), "sha".into(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("static_files"));
    }

    #[test]
    fn missing_env_block_is_not_an_error() {
        // Asking for `--prod` on a spec with no [deploy.production]
        // is fine — base config is used as-is, app name still suffixed.
        let mut t = toml::value::Table::new();
        t.insert("mode".into(), toml::Value::String("fullstack".into()));
        let spec = parse(
            toml::Value::Table(t),
            "myapp".into(),
            "sha".into(),
            Some("production".into()),
        )
        .unwrap();
        assert_eq!(spec.app_name, "myapp-production");
        assert_eq!(spec.environment.as_deref(), Some("production"));
        assert!(spec.env.is_empty());
    }
}
