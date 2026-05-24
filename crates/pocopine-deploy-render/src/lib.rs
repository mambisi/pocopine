//! Render.com deploy adapter for pocopine (RFC 080 §6).
//!
//! Render's deploy model is fundamentally different from Fly's: there
//! are no Machines — instead you have **Services** (web / background
//! worker / cron / static) and Render handles per-service scaling
//! internally. Our two-process spec (`web`, `worker`) maps cleanly:
//!
//! * `web` (with `port`)  → Render `web_service`
//! * `worker` (no port)   → Render `background_worker`
//!
//! Auth: API token via `pocopine deploy auth render` or
//! `RENDER_API_KEY` env var.
//!
//! Image source: user-pushed image to a registry Render can pull
//! from (GHCR, Docker Hub, …). Set
//! `[package.metadata.pocopine.deploy.render] image_registry =
//! "ghcr.io/owner"` so the adapter knows where to tag.

#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub mod client;

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Context;
use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use pocopine_deploy::DeployState;
use pocopine_deploy::{
    common, AdapterMode, Artefact, Constraint, DeployAdapter, DeployOutcome, DeploySpec, Hint,
    Mode, ProcessStatus, StagedFiles,
};
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
struct RenderOverride {
    /// Render workspace ID (`owner_id` in their API). Required — the
    /// adapter refuses fail-fast if absent.
    #[serde(default)]
    owner_id: Option<String>,
    /// Container registry namespace (e.g. `ghcr.io/myorg`). Adapter
    /// pushes images to `{registry}/{app}:{sha}`. Optional — when unset
    /// it is derived from the git remote (GitHub → GHCR, GitLab → GitLab
    /// Container Registry); see [`pocopine_deploy::resolve_registry`].
    #[serde(default)]
    image_registry: Option<String>,
    /// Render region. Default: `oregon`.
    #[serde(default)]
    region: Option<String>,
    /// Pricing plan slug (`starter`, `standard`, `pro`, …). Default: `starter`.
    #[serde(default)]
    plan: Option<String>,
    /// Username for the registry's pull credentials. Optional — defaults
    /// to the GitHub owner for GHCR; needed for other private registries
    /// (GitLab, Docker Hub) where no default can be derived.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    #[serde(default)]
    registry_username: Option<String>,
}

/// Default wait timeout (seconds) for a Render deploy to reach `live`.
/// Render builds + boots take longer than Fly machine starts because
/// the platform pulls and runs the image on shared infrastructure;
/// 10 minutes covers cold-pull and small services.
#[cfg(not(target_arch = "wasm32"))]
const WAIT_LIVE_SECS: u64 = 600;

pub struct RenderAdapter;

impl DeployAdapter for RenderAdapter {
    fn name(&self) -> &'static str {
        "render"
    }

    fn mode(&self) -> AdapterMode {
        AdapterMode::Fullstack
    }

    fn tested_against(&self) -> semver::VersionReq {
        // Render's REST API is `api.render.com/v1`. Bump on schema
        // major moves; `doctor` would probe the API's `version`
        // endpoint and warn outside this range.
        semver::VersionReq::parse(">=1.0.0, <2.0.0").expect("static range parses")
    }

    fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint> {
        let mut out = Vec::new();

        if spec.mode == Mode::Static {
            out.push(Constraint::Refuse(
                "render adapter targets web services / background workers; for static-site mode use `cf-pages` (Render does have a static-site type — Phase 12 follow-up to wire it)"
                    .into(),
            ));
        }

        if !spec.has_process("web") {
            out.push(Constraint::Warn(
                "no `web` process declared — Render's ingress needs a web service to route HTTP traffic".into(),
            ));
        }

        for (name, proc) in spec.processes() {
            if proc.bin.is_empty() {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` has empty `bin` — declare the cargo bin name to launch.",
                )));
            }
            if !is_render_safe_service_name(name) {
                out.push(Constraint::Refuse(format!(
                    "process name `{name}` would render to an invalid Render service name. Allowed: lowercase a-z, 0-9, `-`, `_`; must start with a letter or digit.",
                )));
            }
            if proc.is_public() && proc.port.is_none() {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` is public but declares no port"
                )));
            }
        }

        for (env, names) in common::process_env_collisions(spec) {
            out.push(Constraint::Refuse(format!(
                "process names {names:?} collide on launcher env var `{env}`. Rename so they normalise distinctly.",
            )));
        }

        // Required Render override fields. Resolve via the three-tier
        // resolver — env var → Cargo.toml → ~/.pocopine/config.toml —
        // so OSS forks can set their own `owner_id` without touching
        // the repo and CI can use `POCOPINE_RENDER_OWNER_ID`.
        let overrides = render_override(spec);
        let raw_owner =
            pocopine_deploy::config::resolve("render", "owner_id", overrides.owner_id.as_deref())
                .unwrap_or_default();
        if raw_owner.is_empty() {
            out.push(Constraint::Refuse(
                "render requires `owner_id` (workspace ID — copy it from your Render dashboard's Account Settings → Workspaces). Set via `pocopine deploy config set render owner_id <id>`, $POCOPINE_RENDER_OWNER_ID, or `[deploy.render].owner_id` in Cargo.toml."
                    .into(),
            ));
        } else if is_placeholder_owner_id(&raw_owner) {
            // Templates ship a `REPLACE_WITH_RENDER_WORKSPACE_ID`
            // sentinel so users know where to paste. Catching it here
            // turns a confusing 404 from Render's API ("owner not
            // found") into a clear next step before the deploy starts.
            out.push(Constraint::Refuse(format!(
                "render: `owner_id = \"{raw_owner}\"` is still the placeholder value. Replace it via `pocopine deploy config set render owner_id <id>` or in Cargo.toml.",
            )));
        }
        // Resolve the container registry — explicit `image_registry`,
        // else derived from the git remote (Phase 18). An unresolvable
        // registry halts here, before the image build.
        if let Err(e) = resolved_registry(spec) {
            out.push(Constraint::Refuse(e.to_string()));
        }

        // Backing-service URLs — same gate as Fly.
        if spec.requires_postgres() && !env_provides(spec, "DATABASE_URL") {
            out.push(Constraint::Refuse(
                "postgres is required but no `DATABASE_URL` entry in [deploy.env]. Add a literal, `{ from = \"env\" }`, or `{ from = \"secret\" }` entry (Render Postgres URL or external)."
                    .into(),
            ));
        }
        if spec.requires_redis() && !env_provides(spec, "REDIS_URL") {
            out.push(Constraint::Refuse(
                "redis is required but no `REDIS_URL` entry in [deploy.env]. Add a literal, `{ from = \"env\" }`, or `{ from = \"secret\" }` entry."
                    .into(),
            ));
        }

        // Secret env values: must be present in process env so we can
        // push to Render's encrypted env-var store at deploy time.
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
                    "render needs secret values to push at deploy time: keys declared `{{ from = \"secret\" }}` aren't set in this process: {}. Export them and re-run.",
                    missing.join(", "),
                )));
            }
        }

        // Scale ceiling (Render supports auto-scaling separately; we
        // keep an explicit cap until we wire it).
        for (name, proc) in spec.processes() {
            if proc.scale.min == 0 {
                out.push(Constraint::Warn(format!(
                    "process `{name}` scale.min = 0; flooring to 1 (Render's free tier may sleep services; paid plans need an explicit autoscaler — not wired yet)",
                )));
            }
            if proc.scale.min > 20 {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` scale.min = {} exceeds the safety cap (20).",
                    proc.scale.min,
                )));
            }
        }

        out.push(Constraint::Hint(
            "render adapter uses image-backed services. Make sure your registry creds are configured in Render's dashboard before the first deploy.".into(),
        ));

        out
    }

    fn render_config(&self, spec: &DeploySpec, out: &mut StagedFiles) {
        out.write("Dockerfile", common::render_dockerfile(spec));
        out.write("Dockerfile.dockerignore", common::dockerignore());
        out.write("render.yaml", render_yaml(spec));
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
        let dockerfile = spec.dockerfile_path();
        docker
            .build(Path::new("."), &tag, Some(Path::new(&dockerfile)))
            .context("render: docker build failed")?;
        Ok(Artefact::OciImage { tag })
    }

    #[cfg(target_arch = "wasm32")]
    fn build_artefact(&self, _spec: &DeploySpec) -> Result<Artefact> {
        anyhow::bail!("render adapter not available on wasm32 target")
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn deploy(&self, spec: &DeploySpec, artefact: &Artefact) -> Result<DeployOutcome> {
        use pocopine_deploy::docker::DockerClient;
        use std::collections::BTreeMap;

        let Artefact::OciImage { tag } = artefact else {
            anyhow::bail!("render: deploy requires an OCI image artefact");
        };

        let token = load_render_token()?;
        let overrides = render_override(spec);
        // Three-tier resolution: env > Cargo.toml > ~/.pocopine/config.toml.
        // `owner_id` is required; `detect_constraints` already refused
        // when all three tiers were empty, so unwrap is safe here.
        let owner_id =
            pocopine_deploy::config::resolve("render", "owner_id", overrides.owner_id.as_deref())
                .expect("detect_constraints refuses missing owner_id");
        let region =
            pocopine_deploy::config::resolve("render", "region", overrides.region.as_deref())
                .unwrap_or_else(|| "oregon".to_owned());
        let plan = pocopine_deploy::config::resolve("render", "plan", overrides.plan.as_deref())
            .unwrap_or_else(|| "starter".to_owned());

        let env_vars = resolve_env_for_render(spec)?;

        // Resolve the registry once — used for the push and for the
        // per-service private-image pull credential.
        let registry = resolved_registry(spec)?;

        // 1. Push the locally-built image to the resolved registry —
        //    Render then pulls it. Reuse the user's existing
        //    `docker login`; if absent, fail with a clear next step
        //    rather than a raw push error.
        if !spec.skip_build {
            let docker = DockerClient::new();
            if !docker.has_login(&registry.host) {
                anyhow::bail!(
                    "render: not authenticated to container registry `{}` — `docker push` would fail. Run `docker login {}` and re-deploy.",
                    registry.host,
                    registry.host,
                );
            }
            docker.push(tag).context("render: docker push failed")?;
        }

        // 2. Upsert one Render service per declared process.
        let client = client::RenderClient::new(&token);

        // For a private image, register a pull credential with Render
        // and attach it to each service — no dashboard step. `None` →
        // the image is assumed public and Render pulls anonymously.
        let registry_username = pocopine_deploy::config::resolve(
            "render",
            "registry_username",
            overrides.registry_username.as_deref(),
        );
        let registry_credential_id = match pocopine_deploy::resolve_registry_credentials(
            &registry,
            registry_username.as_deref(),
            spec.git_remote.as_deref(),
        ) {
            Some(creds) => Some(client.ensure_registry_credential(
                &client::RegistryCredentialRequest {
                    name: format!("pocopine-{}", registry.host),
                    registry: render_registry_kind(&registry.host).to_owned(),
                    username: creds.username,
                    auth_token: creds.token,
                    owner_id: owner_id.to_owned(),
                },
            )?),
            None => None,
        };
        if registry_credential_id.is_none() {
            tracing::warn!(
                target: "pocopine.log",
                "render: no registry pull credentials resolved for `{}` — a private image will fail to pull. Run `pocopine deploy auth {}` if the image is private.",
                registry.host, registry.host,
            );
        }
        let mut last_url = String::new();
        for (proc_name, proc) in spec.processes() {
            let service_name = format!("{}-{proc_name}", spec.app_name);
            let desired_type = if proc.is_public() {
                "web_service"
            } else {
                "background_worker"
            };

            let service_env_vars = build_service_env_vars(&env_vars, proc_name, proc);

            let existing = client.find_service_by_name(&service_name)?;
            let svc = match existing {
                Some(s) => {
                    // Render doesn't support changing a service's `type`
                    // in place. Refuse with a clear next step so we
                    // don't silently deploy a worker into a web slot.
                    if let Some(t) = s.service_type.as_deref() {
                        if t != desired_type {
                            anyhow::bail!(
                                "render service `{}` exists as `{}` but the spec now requires `{}`. Delete the service via the Render dashboard and rerun the deploy.",
                                service_name, t, desired_type,
                            );
                        }
                    }
                    client.update_service_image(
                        &s.id,
                        tag,
                        registry_credential_id.as_deref(),
                        &owner_id,
                    )?;
                    s
                }
                None => client.create_service(&client::CreateServiceRequest {
                    name: service_name.clone(),
                    owner_id: owner_id.to_owned(),
                    service_type: desired_type.into(),
                    region: region.to_owned(),
                    plan: plan.to_owned(),
                    image_url: tag.clone(),
                    registry_credential_id: registry_credential_id.clone(),
                    env_vars: service_env_vars.clone(),
                })?,
            };
            client.set_env_vars(&svc.id, &service_env_vars)?;

            // 3. Set instance count unconditionally. Render's scaling
            //    is independent of the deploy; if the service was
            //    previously at 5 and the user lowered scale.min to 1,
            //    we need to push the new count, not leave the old.
            let instances = proc.scale.min.max(1);
            client.set_scale(&svc.id, instances)?;

            // 4. Trigger deploy with the new image URL and wait for live.
            let deploy = client.trigger_deploy(&svc.id, tag)?;
            client.wait_deploy(&svc.id, &deploy.id, &owner_id, WAIT_LIVE_SECS)?;

            // Re-fetch the service to pick up the assigned onrender.com
            // URL (Render doesn't populate it on the create/list shape
            // returned earlier, and on first deploy it's only assigned
            // once the service is live).
            if proc.is_public() {
                let refreshed = client
                    .get_service(&svc.id)
                    .with_context(|| format!("render: refreshing `{}` after deploy", svc.name))?;
                if let Some(url) = refreshed.public_url() {
                    last_url = url.to_owned();
                }
            }
            let _ = (env_vars.len(), BTreeMap::<String, String>::new()); // silence unused warning on minimal builds
        }

        if last_url.is_empty() {
            tracing::warn!(
                target: "pocopine.log",
                "render: deploy completed but no public URL was reported by Render for any web process. \
                 Check the Render dashboard for the assigned hostname.",
            );
        }

        Ok(DeployOutcome {
            url: last_url,
            host_ids: vec![owner_id.to_owned()],
        })
    }

    #[cfg(target_arch = "wasm32")]
    fn deploy(&self, _spec: &DeploySpec, _artefact: &Artefact) -> Result<DeployOutcome> {
        anyhow::bail!("render adapter not available on wasm32 target")
    }

    fn post_deploy_hint(&self, _spec: &DeploySpec, outcome: &DeployOutcome) -> Vec<Hint> {
        vec![Hint::Info(format!("deployed to {}", outcome.url))]
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn status(&self, spec: &DeploySpec) -> Result<Vec<ProcessStatus>> {
        let token = load_render_token()?;
        let client = client::RenderClient::new(&token);
        let mut out = Vec::new();
        for (proc_name, _proc) in spec.processes() {
            let service_name = format!("{}-{proc_name}", spec.app_name);
            let svc = client
                .find_service_by_name(&service_name)
                .with_context(|| format!("render status: lookup `{service_name}`"))?;
            let Some(svc) = svc else {
                out.push(ProcessStatus {
                    process: proc_name.to_owned(),
                    host_service_id: None,
                    deploy_id: None,
                    state: DeployState::Unknown,
                    raw_state: String::new(),
                    url: None,
                    image: None,
                    created_at: None,
                    finished_at: None,
                });
                continue;
            };
            // The list endpoint (find_service_by_name) doesn't reliably
            // populate `serviceDetails.url`; fetch the service detail
            // so the status table reports the actual onrender.com URL
            // for live web services instead of `-`. Best-effort —
            // fall back to the list-shape `svc` on error.
            let detailed = client.get_service(&svc.id).unwrap_or_else(|_| svc.clone());
            let latest = client
                .latest_deploy(&svc.id)
                .with_context(|| format!("render status: latest_deploy for `{service_name}`"))?;
            let (deploy_id, raw_state, image, created_at, finished_at, state) = match latest {
                Some(d) => {
                    let raw = d.status.clone().unwrap_or_default();
                    let state = map_render_state(&raw);
                    let image = d.image_ref().map(str::to_owned);
                    (Some(d.id), raw, image, d.created_at, d.finished_at, state)
                }
                None => (None, String::new(), None, None, None, DeployState::Unknown),
            };
            let url = detailed.public_url().map(str::to_owned);
            out.push(ProcessStatus {
                process: proc_name.to_owned(),
                host_service_id: Some(svc.id),
                deploy_id,
                state,
                raw_state,
                url,
                image,
                created_at,
                finished_at,
            });
        }
        Ok(out)
    }

    #[cfg(target_arch = "wasm32")]
    fn status(&self, _spec: &DeploySpec) -> Result<Vec<ProcessStatus>> {
        anyhow::bail!("render adapter not available on wasm32 target")
    }
}

/// Translate a Render deploy status string into the normalised
/// [`DeployState`]. Unknown strings map to [`DeployState::Unknown`]; the
/// caller still gets the raw value on [`ProcessStatus::raw_state`].
#[cfg(not(target_arch = "wasm32"))]
fn map_render_state(s: &str) -> DeployState {
    match s {
        "created" | "queued" => DeployState::Pending,
        "build_in_progress" | "pre_deploy_in_progress" => DeployState::Building,
        "update_in_progress" => DeployState::Deploying,
        "live" => DeployState::Live,
        "build_failed" | "update_failed" | "pre_deploy_failed" => DeployState::Failed,
        "canceled" | "build_canceled" | "deactivated" => DeployState::Canceled,
        _ => DeployState::Unknown,
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────

/// Per-service env pushed to Render: base `[deploy.env]` literals +
/// `POCOPINE_PROCESS` (launcher dispatch) + `PORT` for public procs
/// (so Render's ingress and the app agree; default would be 10000).
#[cfg(any(not(target_arch = "wasm32"), test))]
fn build_service_env_vars(
    env_vars: &[(String, String)],
    proc_name: &str,
    proc: &pocopine_deploy::ProcessSpec,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = env_vars.to_vec();
    out.push(("POCOPINE_PROCESS".into(), proc_name.to_owned()));
    out.extend(pocopine_deploy::common::port_env(proc));
    out
}

/// Reject sentinel strings the example templates ship — they're
/// non-empty but would 404 from Render's API as "owner not found".
fn is_placeholder_owner_id(s: &str) -> bool {
    let upper = s.trim().to_ascii_uppercase();
    upper.starts_with("REPLACE_WITH") || upper == "<REPLACE_ME>" || upper == "TODO"
}

/// Render service names must match `[a-z0-9][a-z0-9_-]{0,62}`. Same
/// rule we apply to Fly's machine names; we re-check here so the adapter
/// rejects bad identifiers before any API call.
fn is_render_safe_service_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn render_override(spec: &DeploySpec) -> RenderOverride {
    spec.host_override::<RenderOverride>("render")
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Resolve the container registry for this deploy. Three-tier
/// resolver for `image_registry`: `$POCOPINE_RENDER_IMAGE_REGISTRY` →
/// `[deploy.render].image_registry` → `~/.pocopine/config.toml`.
/// Falls back to `pocopine_deploy::resolve_registry`'s git-remote
/// derivation when all three tiers are silent.
fn resolved_registry(spec: &DeploySpec) -> Result<pocopine_deploy::ResolvedRegistry> {
    let overrides = render_override(spec);
    let image_registry = pocopine_deploy::config::resolve(
        "render",
        "image_registry",
        overrides.image_registry.as_deref(),
    );
    pocopine_deploy::resolve_registry(image_registry.as_deref(), spec.git_remote.as_deref())
}

/// Map a registry host to Render's `registrycredentials` `registry`
/// enum. Render accepts `GITHUB` / `GITLAB` / `DOCKER` (and others); we
/// map the registries [`resolved_registry`] can produce.
#[cfg(not(target_arch = "wasm32"))]
fn render_registry_kind(host: &str) -> &'static str {
    match host {
        "ghcr.io" => "GITHUB",
        "registry.gitlab.com" => "GITLAB",
        _ => "DOCKER",
    }
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

#[cfg(not(target_arch = "wasm32"))]
fn load_render_token() -> Result<String> {
    use pocopine_deploy::credentials;
    if let Ok(t) = credentials::load("render") {
        return Ok(t);
    }
    if let Ok(t) = std::env::var("RENDER_API_KEY") {
        if !t.is_empty() {
            return Ok(t);
        }
    }
    anyhow::bail!(
        "render: no API token. Run `pocopine deploy auth render`, or export $POCOPINE_RENDER_TOKEN or $RENDER_API_KEY.",
    )
}

/// Resolve `[deploy.env]` into a `(key, value)` list ready to push to
/// Render's env-vars API. Same shape as Fly's `resolve_env` + secret
/// path collapsed into one map (Render's `env_vars` API encrypts
/// values, so secrets and plain envs use the same endpoint — no
/// separate `set_secrets` call needed).
#[cfg(not(target_arch = "wasm32"))]
fn resolve_env_for_render(spec: &DeploySpec) -> Result<Vec<(String, String)>> {
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
            "render: env values not set in this process: {}. Export them or change to a literal in Pocopine.toml [deploy.env].",
            missing.join(", "),
        );
    }
    Ok(out)
}

fn render_yaml(spec: &DeploySpec) -> String {
    use std::fmt::Write as _;
    let overrides = render_override(spec);
    // Use the three-tier resolver — same as `deploy()` — so the
    // audit artefact reflects what the deploy actually sent.
    // Reading `overrides.*` directly would diverge from the deploy
    // path whenever a value comes from an env var or
    // `~/.pocopine/config.toml` rather than `[deploy.render]`.
    let region = pocopine_deploy::config::resolve("render", "region", overrides.region.as_deref())
        .unwrap_or_else(|| "oregon".into());
    let plan = pocopine_deploy::config::resolve("render", "plan", overrides.plan.as_deref())
        .unwrap_or_else(|| "starter".into());
    let owner =
        pocopine_deploy::config::resolve("render", "owner_id", overrides.owner_id.as_deref())
            .unwrap_or_else(|| "REPLACE_ME".into());

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Generated by pocopine-deploy-render. Do not hand-edit;"
    );
    let _ = writeln!(
        out,
        "# changes go in Pocopine.toml [deploy] or [deploy.render]."
    );
    let _ = writeln!(
        out,
        "# This file is an audit artefact for `pocopine deploy diff`;"
    );
    let _ = writeln!(out, "# the actual deploy uses Render's REST API directly.");
    let _ = writeln!(out);
    let _ = writeln!(out, "services:");

    for (proc_name, proc) in spec.processes() {
        let kind = if proc.is_public() { "web" } else { "worker" };
        let _ = writeln!(out, "  - type: {kind}");
        let _ = writeln!(out, "    name: {}-{proc_name}", spec.app_name);
        let _ = writeln!(out, "    owner: {owner}");
        let _ = writeln!(out, "    runtime: image");
        let _ = writeln!(out, "    region: {region}");
        let _ = writeln!(out, "    plan: {plan}");
        if let Some(hc) = &proc.healthcheck {
            let _ = writeln!(out, "    healthCheckPath: {hc}");
        }
        if proc.scale.min > 1 {
            let _ = writeln!(out, "    numInstances: {}", proc.scale.min);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_deploy::{ProcessSpec, Scale, ServiceSpec};
    use std::collections::BTreeMap;

    fn fullstack_spec_with_render_config() -> DeploySpec {
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

        let mut env: BTreeMap<String, pocopine_deploy::EnvValue> = BTreeMap::new();
        env.insert(
            "DATABASE_URL".into(),
            pocopine_deploy::EnvValue::Indirect {
                from: pocopine_deploy::EnvSource::Env,
            },
        );
        env.insert(
            "REDIS_URL".into(),
            pocopine_deploy::EnvValue::Indirect {
                from: pocopine_deploy::EnvSource::Env,
            },
        );

        let render_block = toml::toml! {
            owner_id = "wrk-abc123"
            image_registry = "ghcr.io/myorg"
            region = "oregon"
            plan = "starter"
        };
        let mut host_overrides: BTreeMap<String, toml::Value> = BTreeMap::new();
        host_overrides.insert("render".into(), toml::Value::from(render_block));

        DeploySpec {
            app_name: "test-app".into(),
            package_name: "test-app".into(),
            git_sha: "abc1234".into(),
            git_remote: None,
            mode: Mode::Fullstack,
            processes,
            services,
            env,
            host_overrides,
            uses_jobs: true,
            uses_collab: false,
            uses_storage: true,
            uses_websocket: false,
            first_deploy: true,
            skip_build: false,
            environment: None,
            workspace_subpath: String::new(),
            has_rust_toolchain: false,
            static_files: vec!["index.html".into(), "pkg".into()],
            build_dir: None,
        }
    }

    #[test]
    fn refuses_static_mode() {
        let mut spec = fullstack_spec_with_render_config();
        spec.mode = Mode::Static;
        let cs = RenderAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(c, Constraint::Refuse(_))));
    }

    #[test]
    fn refuses_missing_owner_id() {
        let mut spec = fullstack_spec_with_render_config();
        // Replace the render block with one missing owner_id.
        let render_block = toml::toml! {
            image_registry = "ghcr.io/myorg"
        };
        spec.host_overrides
            .insert("render".into(), toml::Value::from(render_block));
        let cs = RenderAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("owner_id"),
        )));
    }

    #[test]
    fn refuses_unresolvable_registry() {
        // No explicit image_registry and no git remote — nothing to
        // derive a registry from.
        let mut spec = fullstack_spec_with_render_config();
        let render_block = toml::toml! {
            owner_id = "wrk-abc"
        };
        spec.host_overrides
            .insert("render".into(), toml::Value::from(render_block));
        spec.git_remote = None;
        let cs = RenderAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("image_registry"),
        )));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn auto_resolves_registry_from_github_remote() {
        // No explicit image_registry — derive it from the GitHub remote.
        let mut spec = fullstack_spec_with_render_config();
        let render_block = toml::toml! {
            owner_id = "wrk-abc123"
        };
        spec.host_overrides
            .insert("render".into(), toml::Value::from(render_block));
        spec.git_remote = Some("https://github.com/acme/myapp.git".into());

        let cs = RenderAdapter.detect_constraints(&spec);
        assert!(!cs
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(s) if s.contains("registry"))));
        assert_eq!(image_tag(&spec), "ghcr.io/acme/test-app:abc1234");
    }

    #[test]
    fn renders_yaml_with_processes_and_runtime_image() {
        let spec = fullstack_spec_with_render_config();
        let mut staged = StagedFiles::new();
        RenderAdapter.render_config(&spec, &mut staged);
        let y = staged.get("render.yaml").expect("render.yaml emitted");
        assert!(y.contains("type: web"));
        assert!(y.contains("type: worker"));
        assert!(y.contains("runtime: image"));
        assert!(y.contains("name: test-app-web"));
        assert!(y.contains("name: test-app-worker"));
        assert!(y.contains("owner: wrk-abc123"));
        assert!(y.contains("healthCheckPath: /healthz"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn image_tag_uses_registry_app_name_and_sha() {
        let spec = fullstack_spec_with_render_config();
        assert_eq!(image_tag(&spec), "ghcr.io/myorg/test-app:abc1234");
    }

    #[test]
    fn build_service_env_vars_injects_process_and_port() {
        let proc = pocopine_deploy::ProcessSpec {
            bin: "server".into(),
            port: Some(3022),
            healthcheck: None,
            scale: pocopine_deploy::Scale { min: 1, max: 1 },
            public: None,
        };
        let env = vec![("LOG_LEVEL".to_string(), "info".to_string())];
        let out = build_service_env_vars(&env, "web", &proc);
        assert!(
            out.contains(&("LOG_LEVEL".into(), "info".into())),
            "base env preserved",
        );
        assert!(
            out.contains(&("POCOPINE_PROCESS".into(), "web".into())),
            "POCOPINE_PROCESS injected for launcher dispatch",
        );
        assert!(
            out.contains(&("PORT".into(), "3022".into())),
            "PORT injected from spec port",
        );
    }

    #[test]
    fn build_service_env_vars_skips_port_for_workers() {
        let proc = pocopine_deploy::ProcessSpec {
            bin: "worker".into(),
            // A worker may have a port (e.g. health probe), but if it
            // isn't public Render doesn't route to it.
            port: Some(7000),
            healthcheck: None,
            scale: pocopine_deploy::Scale { min: 1, max: 1 },
            public: Some(false),
        };
        let out = build_service_env_vars(&[], "worker", &proc);
        assert!(
            !out.iter().any(|(k, _)| k == "PORT"),
            "private process doesn't get PORT (Render won't route)",
        );
    }

    #[test]
    fn refuses_placeholder_owner_id() {
        // The template ships `REPLACE_WITH_RENDER_WORKSPACE_ID` so
        // users know where to paste. It's non-empty so the old
        // `is_empty` check passed, then Render's API would 404 with
        // an unclear "owner not found". detect_constraints now catches
        // it earlier with the original "where to find it" hint.
        let mut spec = fullstack_spec_with_render_config();
        let render_block = toml::toml! {
            owner_id = "REPLACE_WITH_RENDER_WORKSPACE_ID"
            image_registry = "ghcr.io/myorg"
        };
        spec.host_overrides
            .insert("render".into(), toml::Value::from(render_block));
        let cs = RenderAdapter.detect_constraints(&spec);
        assert!(cs.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("placeholder") && s.contains("REPLACE_WITH"),
        )));
    }

    #[test]
    fn is_placeholder_owner_id_recognises_template_sentinels() {
        assert!(is_placeholder_owner_id("REPLACE_WITH_RENDER_WORKSPACE_ID"));
        assert!(is_placeholder_owner_id("replace_with_anything"));
        assert!(is_placeholder_owner_id("TODO"));
        assert!(is_placeholder_owner_id("<REPLACE_ME>"));
        assert!(!is_placeholder_owner_id("wrk-abc123"));
        assert!(!is_placeholder_owner_id("owner-12345"));
    }

    #[test]
    fn is_render_safe_service_name_matches_regex() {
        assert!(is_render_safe_service_name("web"));
        assert!(is_render_safe_service_name("test-app-worker"));
        assert!(is_render_safe_service_name("1abc"));
        assert!(!is_render_safe_service_name(""));
        assert!(!is_render_safe_service_name("Web"));
        assert!(!is_render_safe_service_name("api.v2"));
        assert!(!is_render_safe_service_name("-web"));
    }
}
