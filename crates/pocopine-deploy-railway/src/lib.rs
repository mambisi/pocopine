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
//! Backing services: unlike Render, the adapter does **not**
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
    Mode, ProcessStatus, StagedFiles,
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

/// Wait timeout (seconds) for a Railway deployment to reach a terminal
/// state. Short on purpose — when a deploy fails to even start (stuck
/// in `isUpdatable=false` or queue limbo) we'd rather surface a clear
/// timeout in a minute than burn 8 minutes waiting for nothing. If a
/// real build/pull is in flight the user can re-run with `--skip-build`
/// once the registry has the image.
#[cfg(not(target_arch = "wasm32"))]
const WAIT_DEPLOY_SECS: u64 = 60;

/// How many log lines to dump per phase (build / runtime) when a
/// deployment fails or times out. Tight enough to stay scannable in a
/// terminal, large enough to capture the actual error.
#[cfg(not(target_arch = "wasm32"))]
const FAIL_LOG_LINES: i64 = 80;

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
        out.write("Dockerfile.dockerignore", common::dockerignore());
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
        let dockerfile = spec.dockerfile_path();
        docker
            .build(Path::new("."), &tag, Some(Path::new(&dockerfile)))
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
        // Three-tier resolution: env > Cargo.toml > ~/.pocopine/config.toml.
        // `project` defaults to the app name when all three tiers are
        // silent (most projects don't need a custom Railway project
        // name — the app name is fine).
        let project_name =
            pocopine_deploy::config::resolve("railway", "project", overrides.project.as_deref())
                .unwrap_or_else(|| spec.app_name.clone());
        let workspace_id = pocopine_deploy::config::resolve(
            "railway",
            "workspace_id",
            overrides.workspace_id.as_deref(),
        );
        let environment_name = pocopine_deploy::config::resolve(
            "railway",
            "environment",
            overrides.environment.as_deref(),
        );
        let registry_username = pocopine_deploy::config::resolve(
            "railway",
            "registry_username",
            overrides.registry_username.as_deref(),
        );
        let region =
            pocopine_deploy::config::resolve("railway", "region", overrides.region.as_deref());

        // Resolve every `[deploy.env]` entry before touching Railway —
        // a missing `{ from = "env" }` value must fail before we push.
        let env_vars = resolve_env_for_railway(spec)?;

        // Resolve the registry and any private-image pull credentials
        // once — used for the push and for every service instance.
        let registry = resolved_registry(spec)?;
        let registry_creds = pocopine_deploy::resolve_registry_credentials(
            &registry,
            registry_username.as_deref(),
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
        let project = client.ensure_project(&project_name, workspace_id.as_deref())?;
        let environment = match environment_name.as_deref() {
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

            // POCOPINE_PROCESS dispatches the shared launcher to the
            // right bin (RFC 080 §5.3); PORT aligns app bind with
            // Railway's ingress. Build the var-set up-front so we can
            // either fold it into `serviceCreate` (so the implicit
            // initial deploy has them) or upsert it for existing
            // services.
            let mut variables: BTreeMap<String, String> = env_vars.iter().cloned().collect();
            variables.insert("POCOPINE_PROCESS".into(), proc_name.to_owned());
            variables.extend(common::port_env(proc));

            // Track whether the service was just created — Railway's
            // `serviceCreate` queues an initial deploy implicitly with
            // the variables and source we hand it. For existing
            // services we upsert variables, then `serviceInstanceUpdate`
            // pushes settings; if Railway considers that a no-op we
            // explicitly redeploy so var changes still propagate.
            let (service, freshly_created) = match project.service(&service_name) {
                Some(s) => {
                    client.upsert_variables(&project.id, &environment.id, &s.id, &variables)?;
                    (s.clone(), false)
                }
                None => (
                    client.create_service(
                        &project.id,
                        &environment.id,
                        &service_name,
                        tag,
                        registry_creds.as_ref(),
                        &variables,
                    )?,
                    true,
                ),
            };

            // Snapshot the ServiceInstance before pushing settings so
            // we can detect whether the update actually queued a new
            // deployment (latest_deployment_id changes) or no-opped.
            let before = client
                .fetch_service_instance(&service.id, &environment.id)
                .ok()
                .flatten();
            if let Some(b) = &before {
                tracing::info!(
                    target: "pocopine.log",
                    service = %service_name,
                    instance = %b.id,
                    is_updatable = b.is_updatable,
                    source = ?b.source_image,
                    latest_deployment = ?b.latest_deployment_id,
                    "railway serviceInstance (before update)",
                );
                // Probe auto-deploy state — if disabled on a service
                // that has source bound but no deployment, that's why
                // mutations succeed but nothing fires.
                let (enabled, can_enable, reason) =
                    client.fetch_auto_deploy_status(&project.id, &service.id, &environment.id);
                tracing::info!(
                    target: "pocopine.log",
                    service = %service_name,
                    auto_deploy_enabled = ?enabled,
                    can_enable = ?can_enable,
                    reason = ?reason,
                    "railway autoDeployStatus",
                );
            }

            // `serviceConnect` is what wires the dashboard's Source
            // panel and Railway's deploy pipeline to the image.
            // `serviceCreate(source.image)` sets `ServiceInstance.source`
            // — visible via the API — but the dashboard reads a
            // different source-of-truth that only `serviceConnect`
            // populates. Without this call, the service shows up
            // "unconnected" in the UI and no deployment ever fires,
            // even though the API reports a bound source. Call it
            // unconditionally; the mutation is idempotent.
            tracing::info!(
                target: "pocopine.log",
                service = %service_name,
                image = %tag,
                "railway: calling serviceConnect to wire the dashboard source + deploy pipeline",
            );
            client.connect_service_image(&service.id, tag)?;

            let config = client::ServiceInstanceConfig {
                image: tag.clone(),
                num_replicas: proc.scale.min.max(1),
                healthcheck_path: proc.healthcheck.clone(),
                region: region.clone(),
                registry_credentials: registry_creds.clone(),
            };
            client.update_service_instance(&service.id, &environment.id, &config)?;

            // For existing services, check whether the update queued a
            // new deployment. If not, force one via the same mutation
            // Railway's CLI uses — `deploymentRedeploy(id)` — so var /
            // cred changes still ship. Three sub-cases:
            //
            //   (a) before=None, after=Some — update kicked off the
            //       first ever deployment. Nothing more to do.
            //   (b) before=Some, after=different — update queued a new
            //       deploy. Nothing more to do.
            //   (c) before=Some, after=same — update was a no-op.
            //       Redeploy the prior deployment so variables /
            //       credentials ship.
            //   (d) before=None, after=None — service exists but has
            //       never deployed AND the update didn't trigger one.
            //       We can't redeploy from nothing; `wait_for_deployment`
            //       will surface the wedge with a clear error + logs.
            // When set, makes `wait_for_deployment` poll a specific
            // deployment id (via `deployment(id)`) instead of "latest
            // for the service" — needed when we just triggered a new
            // deployment whose id we know, but Railway hasn't promoted
            // it to "latest" yet.
            let mut wait_target: Option<String> = None;

            if !freshly_created {
                let after = client
                    .fetch_service_instance(&service.id, &environment.id)
                    .ok()
                    .flatten();
                let before_id = before
                    .as_ref()
                    .and_then(|b| b.latest_deployment_id.as_deref());
                let after_id = after
                    .as_ref()
                    .and_then(|a| a.latest_deployment_id.as_deref());
                // (a) and (b): update kicked off a NEW deploy whose
                // id is `after_id`. Pin the wait to that one so a slow
                // promote-to-latest doesn't make us latch the prior
                // SUCCESS deployment.
                match (before_id, after_id) {
                    (Some(b), Some(a)) if b != a => wait_target = Some(a.to_owned()),
                    (None, Some(a)) => wait_target = Some(a.to_owned()),
                    _ => {}
                }
                match (before_id, after_id) {
                    (Some(b_id), Some(a_id)) if b_id == a_id => {
                        tracing::info!(
                            target: "pocopine.log",
                            service = %service_name,
                            deployment = %b_id,
                            "railway: serviceInstanceUpdate was a no-op; calling deploymentRedeploy",
                        );
                        // `deploymentRedeploy` returns a *new* clone id.
                        // Capture it so the wait below polls that
                        // specific deployment — Railway doesn't always
                        // promote the clone to "latest" by the first
                        // poll, and waiting on "latest" would latch
                        // onto the prior SUCCESS deployment and report
                        // success without verifying the new clone
                        // actually shipped.
                        wait_target = client.redeploy_deployment(b_id)?;
                    }
                    (None, None) => {
                        // Service exists with source set but has never
                        // deployed (Railway's `isUpdatable=false`
                        // limbo). `deploymentRedeploy` can't help —
                        // no deployment id to clone. Use the same
                        // mutation Railway CLI uses for the
                        // "deploy from current source" gesture; it
                        // works even when latestDeployment is None.
                        tracing::info!(
                            target: "pocopine.log",
                            service = %service_name,
                            "railway: no prior deployment; calling serviceInstanceDeploy(latestCommit:true) to kick the first deploy",
                        );
                        client.deploy_latest_source(&service.id, &environment.id)?;
                        // Re-snapshot so we can tell whether Railway
                        // queued a deployment in response. If
                        // latest_deployment_id is still None after
                        // this, the wait below will surface it.
                        let after_kick = client
                            .fetch_service_instance(&service.id, &environment.id)
                            .ok()
                            .flatten();
                        if let Some(s) = &after_kick {
                            tracing::info!(
                                target: "pocopine.log",
                                service = %service_name,
                                is_updatable = s.is_updatable,
                                latest_deployment = ?s.latest_deployment_id,
                                latest_status = ?s.latest_deployment_status,
                                "railway serviceInstance (after kick)",
                            );
                        }
                        // Pin the wait to the deployment Railway just
                        // queued, if one materialised — same reason
                        // as the redeploy branch.
                        if let Some(id) = after_kick.and_then(|s| s.latest_deployment_id) {
                            wait_target = Some(id);
                        }
                    }
                    _ => {} // (a) or (b): update kicked off a new deploy
                }
            }

            // Verify a real deployment exists for this service. Without
            // this check we used to print "deployed to <url>" even when
            // Railway silently dropped the work (soft-deleted project,
            // no-op update, missing image, …).
            let outcome = client.wait_for_deployment(
                &project.id,
                &service.id,
                &environment.id,
                &service_name,
                WAIT_DEPLOY_SECS,
                wait_target.as_deref(),
            )?;
            match &outcome {
                client::DeploymentOutcome::Live(d) => {
                    tracing::info!(
                        target: "pocopine.log",
                        service = %service_name,
                        deployment = %d.id,
                        "railway deployment live",
                    );
                }
                client::DeploymentOutcome::Failed(d) => {
                    let logs = client.fetch_deployment_logs(&d.id, FAIL_LOG_LINES);
                    print_deployment_logs(&service_name, &d.id, &logs);
                    anyhow::bail!(
                        "railway: deployment {} for service `{service_name}` ended in `{}`. \
                         Logs printed above; full history at \
                         `https://railway.com/project/{}/service/{}`.",
                        d.id,
                        d.status,
                        project.id,
                        service.id,
                    );
                }
                client::DeploymentOutcome::Skipped(d) => {
                    tracing::warn!(
                        target: "pocopine.log",
                        service = %service_name,
                        deployment = %d.id,
                        "railway deployment skipped — Railway no-opped (identical image already running)",
                    );
                }
                client::DeploymentOutcome::Timeout(Some(d)) => {
                    // Long-running deploys often wedge in BUILDING /
                    // DEPLOYING — dump a tail of logs so the user can
                    // diagnose without leaving the terminal, then bail.
                    // We deliberately do *not* fall through to domain
                    // creation + the "deployed to …" line: an unresolved
                    // build/deploy is not a success even if the prior
                    // deployment (still serving) is healthy.
                    let logs = client.fetch_deployment_logs(&d.id, FAIL_LOG_LINES);
                    print_deployment_logs(&service_name, &d.id, &logs);
                    anyhow::bail!(
                        "railway: deployment {} for service `{service_name}` still in `{}` after {WAIT_DEPLOY_SECS}s. \
                         Logs printed above; the previous deployment may still be serving traffic. \
                         Check `https://railway.com/project/{}/service/{}` for build/runtime state.",
                        d.id,
                        d.status,
                        project.id,
                        service.id,
                    );
                }
                client::DeploymentOutcome::Timeout(None) => {
                    anyhow::bail!(
                        "railway: no deployment appeared for service `{service_name}` within {WAIT_DEPLOY_SECS}s. \
                         Railway accepted the mutations but never created a deployment record — common causes: \
                         image-pull credentials missing on the source, or the service got stuck in an internal state. \
                         Inspect the service at `https://railway.com/project/{}/service/{}`.",
                        project.id,
                        service.id,
                    );
                }
            }

            // Generate a public domain for the web-facing service —
            // only after we've confirmed a real, non-failed deployment.
            if proc.is_public() && !matches!(outcome, client::DeploymentOutcome::Failed(_)) {
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

    fn status(&self, _spec: &DeploySpec) -> Result<Vec<ProcessStatus>> {
        anyhow::bail!(
            "railway: `pocopine deploy status` not yet implemented for the railway adapter. \
             Track via the Railway dashboard or `railway status` in the meantime."
        )
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────

/// Dump the tail of build + runtime logs for a failed/timed-out
/// deployment to stderr so the user sees what went wrong without
/// opening the dashboard. Empty log lists print a one-line note.
#[cfg(not(target_arch = "wasm32"))]
fn print_deployment_logs(service_name: &str, deployment_id: &str, logs: &[client::LogEntry]) {
    eprintln!();
    eprintln!("── railway logs · {service_name} · deployment {deployment_id} ──────────");
    if logs.is_empty() {
        eprintln!("(no logs returned — Railway may not have captured any yet)");
        return;
    }
    let mut last_phase: Option<client::LogPhase> = None;
    for entry in logs {
        if last_phase != Some(entry.phase) {
            eprintln!(
                "── {} logs ──",
                match entry.phase {
                    client::LogPhase::Build => "build",
                    client::LogPhase::Runtime => "runtime",
                }
            );
            last_phase = Some(entry.phase);
        }
        let sev = entry.severity.as_deref().unwrap_or("");
        let sev_tag = if sev.is_empty() {
            String::new()
        } else {
            format!("[{sev}] ")
        };
        eprintln!("{} {sev_tag}{}", entry.timestamp, entry.message);
    }
    eprintln!("── end logs ────────────────────────────────────");
    eprintln!();
}

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

/// Resolve the container registry for this deploy. Three-tier
/// resolver for `image_registry`: `$POCOPINE_RAILWAY_IMAGE_REGISTRY` →
/// `[deploy.railway].image_registry` → `~/.pocopine/config.toml`.
/// Falls back to `pocopine_deploy::resolve_registry`'s git-remote
/// derivation when all three tiers are silent.
fn resolved_registry(spec: &DeploySpec) -> Result<pocopine_deploy::ResolvedRegistry> {
    let overrides = railway_override(spec);
    let image_registry = pocopine_deploy::config::resolve(
        "railway",
        "image_registry",
        overrides.image_registry.as_deref(),
    );
    pocopine_deploy::resolve_registry(image_registry.as_deref(), spec.git_remote.as_deref())
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
    // Use the three-tier resolver — same as `deploy()` — so the audit
    // artefact reflects what the deploy actually sent. Reading
    // `overrides.region` directly would diverge whenever the value
    // comes from $POCOPINE_RAILWAY_REGION or
    // `~/.pocopine/config.toml [default.railway] region`.
    if let Some(region) =
        pocopine_deploy::config::resolve("railway", "region", overrides.region.as_deref())
    {
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
            package_name: "test-app".into(),
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
            workspace_subpath: String::new(),
            has_rust_toolchain: false,
            static_files: vec!["index.html".into(), "pkg".into()],
            build_dir: None,
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
        // like Render. The adapter itself does not provision DBs.
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
        assert!(staged.get("Dockerfile.dockerignore").is_some());
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
