//! Railway deploy adapter for pocopine (RFC 080 §6).
//!
//! Railway's model: a **project** holds **services** (one per process),
//! each running in an **environment** (default `production`). Our
//! two-process spec maps cleanly:
//!
//! * `web` (with `port`)  → a Railway service with a public domain
//! * `worker` (no port)   → a Railway service held warm
//!
//! Every operation — project lookup, service create, variable upsert,
//! deploy trigger — is one GraphQL call against
//! `backboard.railway.com/graphql/v2`. No `railway` CLI is shelled out
//! (RFC 080 §2.3, §11 q7); only `docker` is invoked, to build and push
//! the image. See [`client`] for how the GraphQL surface is pinned.
//!
//! Auth: API token via `pocopine deploy auth railway`, or the
//! `RAILWAY_API_TOKEN` / `RAILWAY_TOKEN` env vars.
//!
//! Backing services: unlike Fly/Render, the adapter does **not**
//! provision databases — Railway has no documented public mutation for
//! it. The user adds a Postgres/Redis database to the project in the
//! Railway dashboard (Railway injects its connection variables), or
//! declares `DATABASE_URL` / `REDIS_URL` in `[deploy.env]`.
//!
//! Image source: Railway pulls the image from a registry the user
//! controls (GHCR, Docker Hub, …). Set
//! `[package.metadata.pocopine.deploy.railway] image_registry =
//! "ghcr.io/owner"` so the adapter knows where to push and what URL to
//! point the service at. Make sure Railway has pull credentials for a
//! private registry before the first deploy.

#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub mod client;

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Context;
use anyhow::Result;
use pocopine_deploy::{
    common, AdapterMode, Artefact, Constraint, DeployAdapter, DeployOutcome, DeploySpec, Hint,
    Mode, StagedFiles,
};
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
struct RailwayOverride {
    /// Railway project name. The adapter resolves a project with this
    /// name, creating it on first deploy. Defaults to the app name.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    #[serde(default)]
    project: Option<String>,
    /// Railway environment name. Defaults to `production`.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    #[serde(default)]
    environment: Option<String>,
    /// Railway workspace (team) ID. When set, a freshly-created project
    /// is placed in this workspace instead of the personal account.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    #[serde(default)]
    workspace_id: Option<String>,
    /// Container registry namespace (e.g. `ghcr.io/myorg`). The adapter
    /// pushes the image to `{registry}/{app}:{sha}` and points Railway's
    /// service at that URL. Optional — when unset it is derived from the
    /// git remote (GitHub → GHCR, GitLab → GitLab Container Registry);
    /// see [`pocopine_deploy::resolve_registry`].
    #[serde(default)]
    image_registry: Option<String>,
    /// Railway region slug (e.g. `us-west1`). Optional — Railway picks a
    /// default region when unset.
    #[serde(default)]
    region: Option<String>,
    /// Username for the registry's pull credentials. Optional — defaults
    /// to the GitHub owner for GHCR; needed for other private registries
    /// (GitLab, Docker Hub) where no default can be derived.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    #[serde(default)]
    registry_username: Option<String>,
}

/// Safety cap on `scale.min`. Railway bills per replica; a runaway
/// value in `Pocopine.toml` should fail fast rather than spin up a
/// fleet. Lift this in a follow-up once budget guardrails exist.
const SCALE_MIN_CAP: u32 = 20;

pub struct RailwayAdapter;

impl DeployAdapter for RailwayAdapter {
    fn name(&self) -> &'static str {
        "railway"
    }

    fn mode(&self) -> AdapterMode {
        AdapterMode::Fullstack
    }

    fn tested_against(&self) -> semver::VersionReq {
        // Railway's public GraphQL is "v2"; we track schema generations
        // as semver majors. `tests/spec_drift.rs` introspects the live
        // schema and fails when a mutation/field we use is renamed.
        semver::VersionReq::parse(">=2.0.0, <3.0.0").expect("static range parses")
    }

    fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint> {
        let mut out = Vec::new();

        if spec.mode == Mode::Static {
            out.push(Constraint::Refuse(
                "railway adapter is fullstack-only; for static-site mode use `cf-pages`".into(),
            ));
        }

        if !spec.has_process("web") {
            out.push(Constraint::Warn(
                "no `web` process declared — Railway needs a service with a port to expose a public domain".into(),
            ));
        }

        for (name, proc) in spec.processes() {
            if proc.bin.is_empty() {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` has empty `bin` — declare the cargo bin name to launch.",
                )));
            }
            if !is_railway_safe_service_name(name) {
                out.push(Constraint::Refuse(format!(
                    "process name `{name}` would render to an invalid Railway service name. Allowed: lowercase a-z, 0-9, `-`, `_`; must start with a letter or digit.",
                )));
            }
            if proc.is_public() && proc.port.is_none() {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` is public but declares no port"
                )));
            }
        }

        // Two process names that collapse to the same
        // `POCOPINE_PROC_<KEY>` would silently exec each other's binary
        // (Docker keeps the later `ENV`). Refuse before deploy.
        for (env, names) in common::process_env_collisions(spec) {
            out.push(Constraint::Refuse(format!(
                "process names {names:?} collide on launcher env var `{env}`. Rename so they normalise distinctly.",
            )));
        }

        // Resolve the container registry — explicit `image_registry`,
        // else derived from the git remote (Phase 18). An unresolvable
        // registry halts here, before the image build.
        if let Err(e) = resolved_registry(spec) {
            out.push(Constraint::Refuse(e.to_string()));
        }

        // Secret env values: must be present in this process's env so we
        // can push them into Railway's variable store at deploy time.
        {
            use pocopine_deploy::{EnvSource, EnvValue};
            let missing: Vec<&str> = spec
                .env
                .iter()
                .filter_map(|(k, v)| match v {
                    EnvValue::Indirect {
                        from: EnvSource::Secret,
                    } => match std::env::var(k) {
                        Ok(s) if !s.is_empty() => None,
                        _ => Some(k.as_str()),
                    },
                    _ => None,
                })
                .collect();
            if !missing.is_empty() {
                out.push(Constraint::Refuse(format!(
                    "railway needs secret values to push at deploy time: keys declared `{{ from = \"secret\" }}` aren't set in this process: {}. Export them and re-run.",
                    missing.join(", "),
                )));
            }
        }

        // Scale: Railway holds non-web services warm, so scale-to-zero
        // isn't expressible — floor `scale.min = 0` to 1 with a warning
        // (RFC 080 §11 q2). Cap large values until budget guardrails land.
        for (name, proc) in spec.processes() {
            if proc.scale.min == 0 {
                out.push(Constraint::Warn(format!(
                    "process `{name}` scale.min = 0; flooring to 1 (Railway holds services warm — scale-to-zero is not wired)",
                )));
            }
            if proc.scale.min > SCALE_MIN_CAP {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` scale.min = {} exceeds the safety cap ({SCALE_MIN_CAP}).",
                    proc.scale.min,
                )));
            }
        }

        // Backing services. The Railway adapter does NOT provision
        // databases (no documented public mutation for it). A missing
        // connection URL is a Warn, not a Refuse: Railway can inject one
        // via a dashboard-attached database's reference variables, which
        // this spec can't see — so blocking the deploy would be wrong.
        if spec.requires_postgres() && !env_provides(spec, "DATABASE_URL") {
            out.push(Constraint::Warn(
                "postgres is required but no `DATABASE_URL` in [deploy.env]. The railway adapter does not provision databases — add a Postgres database to the project in the Railway dashboard (it injects a connection variable), or declare `DATABASE_URL` in [deploy.env]."
                    .into(),
            ));
        }
        if spec.requires_redis() && !env_provides(spec, "REDIS_URL") {
            out.push(Constraint::Warn(
                "redis is required but no `REDIS_URL` in [deploy.env]. The railway adapter does not provision databases — add a Redis database to the project in the Railway dashboard, or declare `REDIS_URL` in [deploy.env]."
                    .into(),
            ));
        }

        out.push(Constraint::Hint(
            "railway pulls image-backed services from your registry. Make sure Railway has pull credentials configured for a private registry before the first deploy.".into(),
        ));

        out
    }

    fn render_config(&self, spec: &DeploySpec, out: &mut StagedFiles) {
        out.write("Dockerfile", common::render_dockerfile(spec));
        out.write(".dockerignore", common::DOCKERIGNORE);
        out.write("railway.json", render_railway_json(spec));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn default_artefact(&self, spec: &DeploySpec) -> Artefact {
        Artefact::OciImage {
            tag: image_tag(spec),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn build_artefact(&self, spec: &DeploySpec) -> Result<Artefact> {
        use pocopine_deploy::docker::DockerClient;
        use std::path::Path;

        let tag = image_tag(spec);
        let docker = DockerClient::new();
        docker
            .build(Path::new("."), &tag, Some(Path::new("Dockerfile")))
            .context("railway: docker build failed")?;
        Ok(Artefact::OciImage { tag })
    }

    #[cfg(target_arch = "wasm32")]
    fn build_artefact(&self, _spec: &DeploySpec) -> Result<Artefact> {
        anyhow::bail!("railway adapter not available on wasm32 target")
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn deploy(&self, spec: &DeploySpec, artefact: &Artefact) -> Result<DeployOutcome> {
        use pocopine_deploy::docker::DockerClient;
        use std::collections::BTreeMap;

        let Artefact::OciImage { tag } = artefact else {
            anyhow::bail!("railway: deploy requires an OCI image artefact");
        };

        let token = load_railway_token()?;
        let overrides = railway_override(spec);
        let project_name = overrides
            .project
            .clone()
            .unwrap_or_else(|| spec.app_name.clone());

        // Resolve every `[deploy.env]` entry before touching Railway —
        // a missing `{ from = "env" }` value must fail before we push.
        let env_vars = resolve_env_for_railway(spec)?;

        // Resolve the registry and any private-image pull credentials
        // once — used for the push and for every service instance.
        let registry = resolved_registry(spec)?;
        let registry_creds = pocopine_deploy::resolve_registry_credentials(
            &registry,
            overrides.registry_username.as_deref(),
            spec.git_remote.as_deref(),
        );
        if registry_creds.is_none() {
            tracing::warn!(
                target: "pocopine.log",
                "railway: no registry pull credentials resolved for `{}` — a private image will fail to pull. Run `pocopine deploy auth {}` if the image is private.",
                registry.host, registry.host,
            );
        }

        // 1. Push the locally-built image to the resolved registry,
        //    reusing the user's existing `docker login`; fail with a
        //    clear next step if absent. Skipped under `--skip-build`.
        if !spec.skip_build {
            let docker = DockerClient::new();
            if !docker.has_login(&registry.host) {
                anyhow::bail!(
                    "railway: not authenticated to container registry `{}` — `docker push` would fail. Run `docker login {}` and re-deploy.",
                    registry.host,
                    registry.host,
                );
            }
            docker.push(tag).context("railway: docker push failed")?;
        }

        let client = client::RailwayClient::new(&token);

        // 2. Resolve (or create) the project, then pick the environment.
        let project = client.ensure_project(&project_name, overrides.workspace_id.as_deref())?;
        let environment = match overrides.environment.as_deref() {
            Some(name) => project.environment(name).cloned().ok_or_else(|| {
                anyhow::anyhow!("railway: project `{project_name}` has no environment `{name}`")
            })?,
            None => project.default_environment().cloned().ok_or_else(|| {
                anyhow::anyhow!("railway: project `{project_name}` has no environments")
            })?,
        };

        // 3. One service per declared process: create if absent, push
        //    variables + per-environment config, then trigger a deploy.
        let mut public_url: Option<String> = None;
        for (proc_name, proc) in spec.processes() {
            let service_name = format!("{}-{proc_name}", spec.app_name);

            let service = match project.service(&service_name) {
                Some(s) => s.clone(),
                None => client.create_service(&project.id, &service_name, tag)?,
            };

            // The shared launcher reads `POCOPINE_PROCESS` (RFC 080 §5.3)
            // to dispatch to the right bin. Inject it per service so each
            // Railway service starts its own process — without it every
            // service would boot the container and exit with usage.
            let mut variables: BTreeMap<String, String> = env_vars.iter().cloned().collect();
            variables.insert("POCOPINE_PROCESS".into(), proc_name.to_owned());
            client.upsert_variables(&project.id, &environment.id, &service.id, &variables)?;

            let config = client::ServiceInstanceConfig {
                image: tag.clone(),
                num_replicas: proc.scale.min.max(1),
                healthcheck_path: proc.healthcheck.clone(),
                region: overrides.region.clone(),
                registry_credentials: registry_creds.clone(),
            };
            client.update_service_instance(&service.id, &environment.id, &config)?;
            client.deploy_service_instance(&service.id, &environment.id)?;

            // Generate a public domain for the web-facing service.
            if proc.is_public() {
                if let Some(domain) = client.create_service_domain(&service.id, &environment.id) {
                    public_url = Some(domain);
                }
            }
        }

        let url = match public_url {
            Some(d) if d.starts_with("http") => d,
            Some(d) => format!("https://{d}"),
            // No public service, or domain generation failed — point at
            // the project dashboard rather than guessing a URL.
            None => format!("https://railway.com/project/{}", project.id),
        };

        Ok(DeployOutcome {
            url,
            host_ids: vec![project.id],
        })
    }

    #[cfg(target_arch = "wasm32")]
    fn deploy(&self, _spec: &DeploySpec, _artefact: &Artefact) -> Result<DeployOutcome> {
        anyhow::bail!("railway adapter not available on wasm32 target")
    }

    fn post_deploy_hint(&self, spec: &DeploySpec, outcome: &DeployOutcome) -> Vec<Hint> {
        let mut hints = vec![Hint::Info(format!("deployed to {}", outcome.url))];

        // The adapter doesn't provision databases — if the app needs one
        // and the user hasn't supplied a URL, surface the manual step.
        let mut needs: Vec<&str> = Vec::new();
        if spec.requires_postgres() && !env_provides(spec, "DATABASE_URL") {
            needs.push("Postgres (DATABASE_URL)");
        }
        if spec.requires_redis() && !env_provides(spec, "REDIS_URL") {
            needs.push("Redis (REDIS_URL)");
        }
        if !needs.is_empty() {
            hints.push(Hint::OneTime(format!(
                "This app needs {}. The railway adapter does not provision databases — \
                 add them to the project in the Railway dashboard and reference their \
                 connection variables into each service.",
                needs.join(" and "),
            )));
        }

        hints
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────

/// Railway service names: lowercase `[a-z0-9]`, `-`, `_`; must start
/// with a letter or digit. We re-check the process name here so the
/// adapter rejects bad identifiers before any GraphQL call.
fn is_railway_safe_service_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn railway_override(spec: &DeploySpec) -> RailwayOverride {
    spec.host_override::<RailwayOverride>("railway")
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Resolve the container registry for this deploy: explicit
/// `[deploy.railway].image_registry`, else derived from the git remote.
fn resolved_registry(spec: &DeploySpec) -> Result<pocopine_deploy::ResolvedRegistry> {
    let overrides = railway_override(spec);
    pocopine_deploy::resolve_registry(
        overrides.image_registry.as_deref(),
        spec.git_remote.as_deref(),
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn image_tag(spec: &DeploySpec) -> String {
    // detect_constraints refuses an unresolvable registry before we get
    // here; the sentinel only shows if a caller skipped that check.
    let namespace = resolved_registry(spec)
        .map(|r| r.namespace)
        .unwrap_or_else(|_| "unresolved-registry".to_owned());
    format!("{namespace}/{}:{}", spec.app_name, spec.git_sha)
}

/// `true` iff `[deploy.env]` declares `key` (any flavour counts —
/// literal, `from = "env"`, or `from = "secret"`).
fn env_provides(spec: &DeploySpec, key: &str) -> bool {
    spec.env.contains_key(key)
}

/// Look up the Railway API token: `~/.pocopine/credentials.toml`
/// (or `$POCOPINE_RAILWAY_TOKEN`), then `$RAILWAY_API_TOKEN`.
///
/// Must be an **account or team token** — the client sends it as
/// `Authorization: Bearer`. Project-scoped tokens (`$RAILWAY_TOKEN`)
/// use a different header and cannot create projects, so they are not
/// accepted here.
#[cfg(not(target_arch = "wasm32"))]
fn load_railway_token() -> Result<String> {
    use pocopine_deploy::credentials;
    if let Ok(t) = credentials::load("railway") {
        return Ok(t);
    }
    if let Ok(t) = std::env::var("RAILWAY_API_TOKEN") {
        if !t.is_empty() {
            return Ok(t);
        }
    }
    anyhow::bail!(
        "railway: no API token. Run `pocopine deploy auth railway`, or export $POCOPINE_RAILWAY_TOKEN or $RAILWAY_API_TOKEN (an account or team token — project-scoped tokens are not supported).",
    )
}

/// Resolve `[deploy.env]` into a `(key, value)` list ready to push as
/// Railway service variables. Railway encrypts its variable store, so
/// secrets and plain env collapse into one list — no separate secrets
/// endpoint. Missing `{ from = "env" }` / `{ from = "secret" }` values
/// fail the deploy fast with a clear list.
#[cfg(not(target_arch = "wasm32"))]
fn resolve_env_for_railway(spec: &DeploySpec) -> Result<Vec<(String, String)>> {
    use pocopine_deploy::{EnvSource, EnvValue};

    let mut out = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for (k, v) in &spec.env {
        match v {
            EnvValue::Literal(s) => out.push((k.clone(), s.clone())),
            EnvValue::Indirect {
                from: EnvSource::Env,
            }
            | EnvValue::Indirect {
                from: EnvSource::Secret,
            } => match std::env::var(k) {
                Ok(s) if !s.is_empty() => out.push((k.clone(), s)),
                _ => missing.push(k.clone()),
            },
        }
    }

    if !missing.is_empty() {
        anyhow::bail!(
            "railway: env values not set in this process: {}. Export them or change to a literal in Pocopine.toml [deploy.env].",
            missing.join(", "),
        );
    }
    Ok(out)
}

/// Audit-only `railway.json` (RFC 080 §7). Railway is deployed over the
/// GraphQL API, not from this file; it exists so `pocopine deploy diff`
/// can show the user what the adapter sent. The `_generated` field
/// carries `pocopine-deploy`'s overwrite marker so a re-render isn't
/// mistaken for a hand-edited file.
fn render_railway_json(spec: &DeploySpec) -> String {
    let overrides = railway_override(spec);

    let services: Vec<serde_json::Value> = spec
        .processes()
        .map(|(name, p)| {
            let mut svc = serde_json::json!({
                "name": format!("{}-{name}", spec.app_name),
                "numReplicas": p.scale.min.max(1),
            });
            if let Some(hc) = &p.healthcheck {
                svc["healthcheckPath"] = serde_json::Value::String(hc.clone());
            }
            svc
        })
        .collect();

    let mut doc = serde_json::json!({
        "$schema": "https://railway.com/railway.schema.json",
        "_generated": "Generated by pocopine-deploy-railway. Do not hand-edit; \
                       changes go in Pocopine.toml [deploy] or [deploy.railway]. \
                       This file is an audit artefact for `pocopine deploy diff`; \
                       the actual deploy uses Railway's GraphQL API directly.",
        "build": {
            "builder": "DOCKERFILE",
            "dockerfilePath": "Dockerfile",
        },
        "services": services,
    });
    if let Some(region) = overrides.region.as_deref() {
        doc["deploy"] = serde_json::json!({ "region": region });
    }

    serde_json::to_string_pretty(&doc).expect("railway.json serialisation is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_deploy::{ProcessSpec, Scale, ServiceSpec};
    use std::collections::BTreeMap;

    fn fullstack_spec_with_railway_config() -> DeploySpec {
        let mut processes = BTreeMap::new();
        processes.insert(
            "web".into(),
            ProcessSpec {
                bin: "server".into(),
                port: Some(8080),
                healthcheck: Some("/healthz".into()),
                scale: Scale { min: 1, max: 3 },
                public: None,
            },
        );
        processes.insert(
            "worker".into(),
            ProcessSpec {
                bin: "worker".into(),
                port: None,
                healthcheck: None,
                scale: Scale { min: 1, max: 2 },
                public: None,
            },
        );
        let mut services = BTreeMap::new();
        services.insert("postgres".into(), ServiceSpec { required: true });
        services.insert("redis".into(), ServiceSpec { required: true });

        let railway_block = toml::toml! {
            image_registry = "ghcr.io/myorg"
            region = "us-west1"
        };
        let mut host_overrides: BTreeMap<String, toml::Value> = BTreeMap::new();
        host_overrides.insert("railway".into(), toml::Value::from(railway_block));

        DeploySpec {
            app_name: "test-app".into(),
            git_sha: "abc1234".into(),
            git_remote: None,
            mode: Mode::Fullstack,
            processes,
            services,
            env: Default::default(),
            host_overrides,
            uses_jobs: true,
            uses_collab: false,
            uses_storage: true,
            uses_websocket: false,
            first_deploy: true,
            skip_build: false,
            environment: None,
        }
    }

    #[test]
    fn refuses_static_mode() {
        let mut spec = fullstack_spec_with_railway_config();
        spec.mode = Mode::Static;
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(c, Constraint::Refuse(_))));
    }

    #[test]
    fn refuses_unresolvable_registry() {
        // No explicit image_registry and no git remote — nothing to
        // derive a registry from.
        let mut spec = fullstack_spec_with_railway_config();
        let railway_block = toml::toml! {
            region = "us-west1"
        };
        spec.host_overrides
            .insert("railway".into(), toml::Value::from(railway_block));
        spec.git_remote = None;
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("image_registry"),
        )));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn auto_resolves_registry_from_github_remote() {
        // No explicit image_registry — derive it from the GitHub remote.
        let mut spec = fullstack_spec_with_railway_config();
        let railway_block = toml::toml! {
            region = "us-west1"
        };
        spec.host_overrides
            .insert("railway".into(), toml::Value::from(railway_block));
        spec.git_remote = Some("git@github.com:acme/myapp.git".into());

        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(!cs
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(s) if s.contains("image_registry"))));
        assert_eq!(image_tag(&spec), "ghcr.io/acme/test-app:abc1234");
    }

    #[test]
    fn warns_when_no_web_process() {
        let mut spec = fullstack_spec_with_railway_config();
        spec.processes.remove("web");
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(c, Constraint::Warn(_))));
    }

    #[test]
    fn warns_when_scale_min_is_zero() {
        let mut spec = fullstack_spec_with_railway_config();
        spec.processes.get_mut("worker").unwrap().scale.min = 0;
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs
            .iter()
            .any(|c| matches!(c, Constraint::Warn(s) if s.contains("`worker` scale.min = 0"))));
    }

    #[test]
    fn refuses_excessive_scale_min() {
        let mut spec = fullstack_spec_with_railway_config();
        spec.processes.get_mut("web").unwrap().scale.min = 50;
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(
            |c| matches!(c, Constraint::Refuse(s) if s.contains("scale.min = 50") && s.contains("safety cap"))
        ));
    }

    #[test]
    fn refuses_empty_bin() {
        let mut spec = fullstack_spec_with_railway_config();
        spec.processes.get_mut("web").unwrap().bin = String::new();
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(s) if s.contains("empty `bin`"))));
    }

    #[test]
    fn missing_database_url_warns_does_not_refuse() {
        // Railway can inject a connection variable from a dashboard-
        // attached database, so an absent URL is a Warn — never a Refuse
        // like Fly/Render. The adapter itself does not provision DBs.
        let spec = fullstack_spec_with_railway_config();
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(
            c,
            Constraint::Warn(s) if s.contains("DATABASE_URL") && s.contains("does not provision"),
        )));
        assert!(!cs.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("DATABASE_URL"),
        )));
    }

    #[test]
    fn refuses_secret_source_with_no_value_in_env() {
        use pocopine_deploy::{EnvSource, EnvValue};
        let mut spec = fullstack_spec_with_railway_config();
        let key = "POCOPINE_RAILWAY_SECRET_REFUSE_TEST";
        spec.env.insert(
            key.into(),
            EnvValue::Indirect {
                from: EnvSource::Secret,
            },
        );
        std::env::remove_var(key);
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(
            |c| matches!(c, Constraint::Refuse(s) if s.contains(key) && s.contains("secret values")),
        ));
    }

    #[test]
    fn refuses_process_env_var_collision() {
        use pocopine_deploy::ProcessSpec;
        let mut spec = fullstack_spec_with_railway_config();
        let p = ProcessSpec {
            bin: "api".into(),
            port: None,
            healthcheck: None,
            scale: Default::default(),
            public: None,
        };
        spec.processes.insert("api-worker".into(), p.clone());
        spec.processes.insert("api_worker".into(), p);
        let cs = RailwayAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("collide") && s.contains("POCOPINE_PROC_API_WORKER"),
        )));
    }

    #[test]
    fn renders_railway_json_with_processes_and_dockerfile_builder() {
        let spec = fullstack_spec_with_railway_config();
        let mut staged = StagedFiles::new();
        RailwayAdapter.render_config(&spec, &mut staged);

        let j = staged.get("railway.json").expect("railway.json emitted");
        let v: serde_json::Value = serde_json::from_str(j).expect("railway.json is valid JSON");
        assert_eq!(v["build"]["builder"], "DOCKERFILE");
        assert_eq!(v["build"]["dockerfilePath"], "Dockerfile");
        assert_eq!(v["deploy"]["region"], "us-west1");
        let names: Vec<&str> = v["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"test-app-web"));
        assert!(names.contains(&"test-app-worker"));
        // The overwrite marker must survive into the JSON.
        assert!(j.contains("Generated by pocopine-deploy"));

        assert!(staged.get("Dockerfile").is_some());
        assert!(staged.get(".dockerignore").is_some());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn image_tag_uses_registry_app_name_and_sha() {
        let spec = fullstack_spec_with_railway_config();
        assert_eq!(image_tag(&spec), "ghcr.io/myorg/test-app:abc1234");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn default_artefact_matches_image_tag_for_skip_build_path() {
        let spec = fullstack_spec_with_railway_config();
        match RailwayAdapter.default_artefact(&spec) {
            Artefact::OciImage { tag } => assert_eq!(tag, image_tag(&spec)),
            other => panic!("expected OciImage artefact, got {other:?}"),
        }
    }

    #[test]
    fn is_railway_safe_service_name_matches_rule() {
        assert!(is_railway_safe_service_name("web"));
        assert!(is_railway_safe_service_name("test-app-worker"));
        assert!(is_railway_safe_service_name("1abc"));
        assert!(!is_railway_safe_service_name(""));
        assert!(!is_railway_safe_service_name("Web"));
        assert!(!is_railway_safe_service_name("api.v2"));
        assert!(!is_railway_safe_service_name("-web"));
    }

    #[test]
    fn post_deploy_hint_includes_database_setup_step() {
        let spec = fullstack_spec_with_railway_config();
        let outcome = DeployOutcome {
            url: "https://test-app.up.railway.app".into(),
            host_ids: vec!["proj_1".into()],
        };
        let hints = RailwayAdapter.post_deploy_hint(&spec, &outcome);
        assert!(hints.iter().any(|h| matches!(h, Hint::Info(_))));
        assert!(hints.iter().any(|h| matches!(
            h,
            Hint::OneTime(s) if s.contains("DATABASE_URL") && s.contains("REDIS_URL"),
        )));
    }

    #[test]
    fn post_deploy_hint_skips_database_step_when_urls_supplied() {
        use pocopine_deploy::EnvValue;
        let mut spec = fullstack_spec_with_railway_config();
        spec.env.insert(
            "DATABASE_URL".into(),
            EnvValue::Literal("postgres://x".into()),
        );
        spec.env
            .insert("REDIS_URL".into(), EnvValue::Literal("redis://x".into()));
        let outcome = DeployOutcome {
            url: "https://test-app.up.railway.app".into(),
            host_ids: vec!["proj_1".into()],
        };
        let hints = RailwayAdapter.post_deploy_hint(&spec, &outcome);
        assert!(!hints.iter().any(|h| matches!(h, Hint::OneTime(_))));
    }
}
