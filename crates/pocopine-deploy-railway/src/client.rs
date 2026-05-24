//! Railway GraphQL API client — typed via [`graphql_client`].
//!
//! Every operation in `graphql/railway.graphql` is checked against
//! `schema.json` (Railway's introspected schema) **at compile time**: a
//! renamed or removed field is a build error, not a runtime 4xx.
//!
//! `schema.json` is not vendored — at ~1.5 MB it is too large to review
//! in diffs. `build.rs` downloads it from Railway's unauthenticated
//! introspection endpoint on the first build and caches it;
//! `tests/spec_drift.rs` re-introspects the live schema to catch drift.
//!
//! No `railway` CLI is shelled out (RFC 080 §2.3, §11 q7). Railway's API
//! is environment-scoped: a project has environments (default
//! `production`) and every service-level operation takes an
//! `environmentId`.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use graphql_client::{GraphQLQuery, Response};
use tracing::info;

/// Railway's public GraphQL endpoint.
pub const RAILWAY_GRAPHQL: &str = "https://backboard.railway.com/graphql/v2";

// Railway custom GraphQL scalars. `graphql_client` resolves these by
// name from the enclosing module when it generates the query types.
#[allow(clippy::upper_case_acronyms)]
type JSON = serde_json::Value;
type EnvironmentVariables = serde_json::Value;
// `DateTime` shows up in `LatestDeployment.createdAt` and
// `Project.deletedAt`. Represented as a raw ISO-8601 string — we only
// surface it to the user / logs, never compute against it.
type DateTime = String;

/// Declare a `graphql_client` query/mutation struct against the pinned
/// schema. One per operation in `graphql/railway.graphql`.
macro_rules! railway_op {
    ($name:ident) => {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "schema.json",
            query_path = "graphql/railway.graphql",
            response_derives = "Debug,Clone"
        )]
        struct $name;
    };
}

railway_op!(FindProjects);
railway_op!(CreateProject);
railway_op!(CreateService);
railway_op!(ConnectServiceImage);
railway_op!(UpdateServiceInstance);
railway_op!(UpsertVariables);
railway_op!(CreateServiceDomain);
railway_op!(LatestDeployment);
railway_op!(BuildLogs);
railway_op!(DeploymentLogs);
railway_op!(RedeployDeployment);
railway_op!(FetchDeployment);
railway_op!(DeployLatestSource);
railway_op!(FetchServiceInstance);
railway_op!(FetchAutoDeployStatus);

pub struct RailwayClient {
    /// Full GraphQL endpoint URL (every request POSTs here).
    endpoint: String,
    http: reqwest::blocking::Client,
}

impl RailwayClient {
    pub fn new(token: impl AsRef<str>) -> Self {
        Self::with_endpoint(RAILWAY_GRAPHQL, token)
    }

    /// Point the client at an arbitrary GraphQL endpoint — used by the
    /// wiremock HTTP tests to assert on payload shape.
    pub fn with_endpoint(endpoint: impl Into<String>, token: impl AsRef<str>) -> Self {
        use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
        let mut headers = HeaderMap::new();
        if let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {}", token.as_ref())) {
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        Self {
            endpoint: endpoint.into(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .default_headers(headers)
                .build()
                .expect("reqwest blocking client builds with default settings"),
        }
    }

    /// Execute one typed GraphQL operation and unwrap the
    /// `{ data, errors }` envelope. GraphQL `errors` surface verbatim —
    /// Railway's own messages are the most actionable thing to show.
    fn run<Q: GraphQLQuery>(&self, variables: Q::Variables, op: &str) -> Result<Q::ResponseData> {
        let resp: Response<Q::ResponseData> =
            graphql_client::reqwest::post_graphql_blocking::<Q, _>(
                &self.http,
                &self.endpoint,
                variables,
            )
            .with_context(|| format!("railway graphql `{op}`: request failed"))?;

        if let Some(errors) = resp.errors {
            if !errors.is_empty() {
                bail!(
                    "railway graphql `{op}`: {}",
                    errors
                        .iter()
                        .map(|e| e.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                );
            }
        }
        resp.data
            .with_context(|| format!("railway graphql `{op}`: response carried no data"))
    }

    // ─── Projects ────────────────────────────────────────────────────────

    /// List the caller's projects and return the first whose name
    /// matches exactly. Includes soft-deleted projects so the caller
    /// can distinguish "no such project" from "project is in the 2-day
    /// soft-delete window"; see [`Self::ensure_project`].
    pub fn find_project(&self, name: &str) -> Result<Option<Project>> {
        let data = self.run::<FindProjects>(find_projects::Variables {}, "projects")?;
        Ok(data
            .projects
            .edges
            .into_iter()
            .map(|edge| {
                let node = edge.node;
                Project {
                    id: node.id,
                    name: node.name,
                    deleted_at: node.deleted_at,
                    environments: node
                        .environments
                        .edges
                        .into_iter()
                        .map(|e| Environment {
                            id: e.node.id,
                            name: e.node.name,
                        })
                        .collect(),
                    services: node
                        .services
                        .edges
                        .into_iter()
                        .map(|e| Service {
                            id: e.node.id,
                            name: e.node.name,
                            deleted_at: e.node.deleted_at,
                        })
                        .collect(),
                }
            })
            .find(|p| p.name == name))
    }

    /// Resolve a project by name, creating it if absent. Railway has no
    /// `projectUpsert`, so this lists projects and matches on name; a
    /// fresh project is created via `projectCreate` then re-queried so
    /// the caller always gets a uniformly-shaped [`Project`].
    ///
    /// Soft-deleted projects (within Railway's 2-day delete window) are
    /// refused with an actionable error: the dashboard hides them but
    /// the API still returns them, and using one as if it were live
    /// silently drops every subsequent mutation — `serviceCreate`
    /// returns a real-looking id that immediately becomes a tombstone,
    /// then the deploy "succeeds" with a URL that resolves to nothing.
    pub fn ensure_project(&self, name: &str, workspace_id: Option<&str>) -> Result<Project> {
        if let Some(existing) = self.find_project(name)? {
            if let Some(deleted_at) = existing.deleted_at.as_deref() {
                bail!(
                    "railway: project `{name}` is in the 2-day soft-delete window (deletedAt={deleted_at}). \
                     Railway reserves the name during this period and silently drops mutations against the project. \
                     Permanently delete it from the Railway dashboard (Project settings → Danger Zone → Delete project, \
                     then confirm again under the deleted-projects view), or rename the app in Cargo.toml \
                     (`[package].name` or `[deploy.railway].project = \"...\"`).",
                );
            }
            info!(target: "pocopine.log", project = %name, id = %existing.id, "railway project resolved");
            return Ok(existing);
        }

        info!(target: "pocopine.log", project = %name, "railway projectCreate");
        let vars = create_project::Variables {
            input: create_project::ProjectCreateInput {
                default_environment_name: None,
                description: None,
                is_monorepo: None,
                is_public: None,
                name: Some(name.to_owned()),
                pr_deploys: None,
                repo: None,
                runtime: None,
                workspace_id: workspace_id.map(str::to_owned),
            },
        };
        self.run::<CreateProject>(vars, "projectCreate")?;

        self.find_project(name)?
            .with_context(|| format!("railway: project `{name}` not found after projectCreate"))
    }

    // ─── Services ────────────────────────────────────────────────────────

    /// Create an image-backed service in a project. Railway creates a
    /// service instance per environment automatically; per-env config is
    /// then pushed via [`Self::update_service_instance`].
    ///
    /// `registry_credentials` is mandatory for private images: Railway
    /// binds them to the source at create time, and the dashboard shows
    /// the credential under "Source". Passing `None` later via update
    /// doesn't re-bind them (visually or for pulls), so first-deploy
    /// callers must include credentials here when the image is private.
    ///
    /// `variables` is folded into the initial deploy Railway implicitly
    /// queues from `serviceCreate`, so the first running container has
    /// `POCOPINE_PROCESS`/`PORT`/etc already in scope rather than
    /// needing a follow-up redeploy.
    ///
    /// **`environment_id` is required** — Railway's `ServiceCreateInput`
    /// schema marks it `String` (not `String!`), but in practice it is
    /// the field that actually binds the image source to the per-env
    /// `ServiceInstance.source`. Without it the dashboard shows the
    /// empty-state "choose an image" picker and no deploy fires.
    /// Mirrors Railway's own CLI (`railway add --image`).
    pub fn create_service(
        &self,
        project_id: &str,
        environment_id: &str,
        name: &str,
        image: &str,
        registry_credentials: Option<&pocopine_deploy::RegistryCredentials>,
        variables: &BTreeMap<String, String>,
    ) -> Result<Service> {
        info!(
            target: "pocopine.log",
            project = %project_id,
            env = %environment_id,
            service = %name,
            n_vars = variables.len(),
            "railway serviceCreate",
        );
        let variables_json = if variables.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(
                variables
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect(),
            ))
        };
        let vars = create_service::Variables {
            input: create_service::ServiceCreateInput {
                branch: None,
                environment_id: Some(environment_id.to_owned()),
                icon: None,
                name: Some(name.to_owned()),
                project_id: project_id.to_owned(),
                registry_credentials: registry_credentials.map(|c| {
                    create_service::RegistryCredentialsInput {
                        username: c.username.clone(),
                        password: c.token.clone(),
                    }
                }),
                source: Some(create_service::ServiceSourceInput {
                    image: Some(image.to_owned()),
                    repo: None,
                }),
                template_id: None,
                template_service_id: None,
                variables: variables_json,
            },
        };
        let data = self.run::<CreateService>(vars, "serviceCreate")?;
        Ok(Service {
            id: data.service_create.id,
            name: data.service_create.name,
            deleted_at: None,
        })
    }

    /// Recovery: bind an image source to a service whose
    /// `ServiceInstance.source` is currently unset. This is the state
    /// legacy pocopine deploys left behind when `serviceCreate` ran
    /// without `environmentId` — the dashboard shows the service but
    /// with the empty-state image picker, and `serviceInstanceUpdate`
    /// won't rebind because it treats the source assignment as a no-op.
    /// `serviceConnect` is the only API surface that re-attaches a
    /// source to an existing service without recreating it.
    pub fn connect_service_image(&self, service_id: &str, image: &str) -> Result<()> {
        info!(target: "pocopine.log", service = %service_id, image = %image, "railway serviceConnect (recover image source)");
        let vars = connect_service_image::Variables {
            id: service_id.to_owned(),
            input: connect_service_image::ServiceConnectInput {
                branch: None,
                image: Some(image.to_owned()),
                repo: None,
            },
        };
        self.run::<ConnectServiceImage>(vars, "serviceConnect")?;
        Ok(())
    }

    /// Push per-environment service-instance config: the image to run,
    /// replica count, healthcheck path, and region.
    pub fn update_service_instance(
        &self,
        service_id: &str,
        environment_id: &str,
        cfg: &ServiceInstanceConfig,
    ) -> Result<()> {
        info!(target: "pocopine.log", service = %service_id, env = %environment_id, "railway serviceInstanceUpdate");
        let vars = update_service_instance::Variables {
            service_id: service_id.to_owned(),
            environment_id: Some(environment_id.to_owned()),
            input: update_service_instance::ServiceInstanceUpdateInput {
                build_command: None,
                builder: None,
                cron_schedule: None,
                dockerfile_path: None,
                draining_seconds: None,
                healthcheck_path: cfg.healthcheck_path.clone(),
                healthcheck_timeout: None,
                ipv6_egress_enabled: None,
                multi_region_config: None,
                nixpacks_plan: None,
                num_replicas: Some(i64::from(cfg.num_replicas)),
                overlap_seconds: None,
                pre_deploy_command: None,
                railway_config_file: None,
                region: cfg.region.clone(),
                registry_credentials: cfg.registry_credentials.as_ref().map(|c| {
                    update_service_instance::RegistryCredentialsInput {
                        username: c.username.clone(),
                        password: c.token.clone(),
                    }
                }),
                restart_policy_max_retries: None,
                restart_policy_type: None,
                root_directory: None,
                sleep_application: None,
                source: Some(update_service_instance::ServiceSourceInput {
                    image: Some(cfg.image.clone()),
                    repo: None,
                }),
                start_command: None,
                watch_patterns: None,
            },
        };
        self.run::<UpdateServiceInstance>(vars, "serviceInstanceUpdate")?;
        Ok(())
    }

    // ─── Variables ───────────────────────────────────────────────────────

    /// Upsert the service's variables for one environment. Railway
    /// encrypts variables at rest, so plain env and secrets collapse into
    /// this one call. `replace = false` — our keys are merged, NOT made
    /// the whole set: a `true` here would wipe Railway-managed reference
    /// variables (e.g. an attached database's connection vars).
    pub fn upsert_variables(
        &self,
        project_id: &str,
        environment_id: &str,
        service_id: &str,
        variables: &BTreeMap<String, String>,
    ) -> Result<()> {
        info!(
            target: "pocopine.log",
            service = %service_id, env = %environment_id, n = variables.len(),
            "railway variableCollectionUpsert",
        );
        let vars = upsert_variables::Variables {
            input: upsert_variables::VariableCollectionUpsertInput {
                environment_id: environment_id.to_owned(),
                project_id: project_id.to_owned(),
                replace: Some(false),
                service_id: Some(service_id.to_owned()),
                skip_deploys: None,
                variables: serde_json::to_value(variables)
                    .context("railway: encoding service variables")?,
            },
        };
        self.run::<UpsertVariables>(vars, "variableCollectionUpsert")?;
        Ok(())
    }

    // ─── Deployments ─────────────────────────────────────────────────────

    /// Redeploy a specific deployment by id via `deploymentRedeploy` —
    /// the same mutation Railway's own CLI uses for `railway redeploy`.
    /// Use this when `serviceInstanceUpdate` was a no-op but we still
    /// need to re-roll the container (updated env vars or rotated
    /// registry credentials).
    ///
    /// "Not found" is tolerated: the deployment may have been
    /// hard-deleted between when the caller read its id and this call,
    /// and the original deploy flow will surface that via
    /// `wait_for_deployment`.
    pub fn redeploy_deployment(&self, deployment_id: &str) -> Result<Option<String>> {
        info!(target: "pocopine.log", deployment = %deployment_id, "railway deploymentRedeploy");
        let vars = redeploy_deployment::Variables {
            id: deployment_id.to_owned(),
        };
        match self.run::<RedeployDeployment>(vars, "deploymentRedeploy") {
            Ok(data) => Ok(Some(data.deployment_redeploy.id)),
            Err(e) if e.to_string().to_lowercase().contains("not found") => {
                tracing::warn!(
                    target: "pocopine.log",
                    deployment = %deployment_id,
                    "railway: deploymentRedeploy returned 'not found'; relying on implicit deploy from prior mutations",
                );
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Fetch a specific deployment by id. Used as the wait anchor when
    /// the caller has just triggered (or redeployed) a specific
    /// deployment and wants to poll *that one* — not whatever
    /// `latest_deployment(input)` happens to return, which Railway may
    /// not have promoted to "latest" by the first poll.
    pub fn fetch_deployment(&self, deployment_id: &str) -> Result<Option<DeploymentInfo>> {
        let vars = fetch_deployment::Variables {
            id: deployment_id.to_owned(),
        };
        match self.run::<FetchDeployment>(vars, "deployment") {
            Ok(d) => {
                let n = d.deployment;
                Ok(Some(DeploymentInfo {
                    id: n.id,
                    status: format!("{:?}", n.status).to_uppercase(),
                    url: n.url,
                    static_url: n.static_url,
                    created_at: format!("{:?}", n.created_at),
                }))
            }
            Err(e) if e.to_string().to_lowercase().contains("not found") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Read the auto-deploy state for a service instance. Returns
    /// `(enabled, can_enable, reason)`. Best-effort: returns
    /// `(None, None, None)` on any error so the caller can keep
    /// going.
    pub fn fetch_auto_deploy_status(
        &self,
        project_id: &str,
        service_id: &str,
        environment_id: &str,
    ) -> (Option<bool>, Option<bool>, Option<String>) {
        let vars = fetch_auto_deploy_status::Variables {
            project_id: project_id.to_owned(),
            environment_id: environment_id.to_owned(),
            service_id: service_id.to_owned(),
        };
        match self.run::<FetchAutoDeployStatus>(vars, "serviceInstanceAutoDeployStatus") {
            Ok(d) => {
                let s = d.service_instance_auto_deploy_status;
                // Schema lists these as nullable; graphql_client may
                // pull them as plain bool in some schema revisions —
                // wrap explicitly with Into so either flavor compiles.
                (Some(s.enabled), Some(s.can_enable), s.reason)
            }
            Err(_) => (None, None, None),
        }
    }

    /// Kick a deploy from the source currently bound to the
    /// ServiceInstance, regardless of whether a prior deployment
    /// exists. Calls `serviceInstanceDeploy(latestCommit: true)` —
    /// the same mutation Railway's CLI uses for
    /// `railway redeploy --from-source`. Critical for recovering
    /// services left in the "configured but never deployed" state by
    /// earlier pocopine versions (or any wedged `isUpdatable=false`
    /// instance with no `latestDeployment`).
    pub fn deploy_latest_source(&self, service_id: &str, environment_id: &str) -> Result<()> {
        info!(
            target: "pocopine.log",
            service = %service_id,
            env = %environment_id,
            "railway serviceInstanceDeploy(latestCommit: true)",
        );
        let vars = deploy_latest_source::Variables {
            service_id: service_id.to_owned(),
            environment_id: environment_id.to_owned(),
        };
        let data = self.run::<DeployLatestSource>(vars, "serviceInstanceDeploy")?;
        // Mutation returns Boolean — true == accepted, false == rejected.
        // Log it either way so we know Railway didn't silently no-op.
        info!(
            target: "pocopine.log",
            service = %service_id,
            accepted = data.service_instance_deploy,
            "railway serviceInstanceDeploy response",
        );
        if !data.service_instance_deploy {
            anyhow::bail!(
                "railway serviceInstanceDeploy returned false — Railway rejected the deploy trigger. \
                 Likely causes: invalid registry credentials, image-pull permission denied, or the \
                 service is in a transitional state (`isUpdatable=false`). Check the service in the \
                 Railway dashboard.",
            );
        }
        Ok(())
    }

    /// Read the per-environment `ServiceInstance` — Railway's
    /// "Settings" tab plus the pointer to its latest deployment.
    /// Returns `None` when Railway has no instance record yet (the
    /// `serviceCreate` ack hasn't materialized one).
    pub fn fetch_service_instance(
        &self,
        service_id: &str,
        environment_id: &str,
    ) -> Result<Option<ServiceInstanceSnapshot>> {
        let vars = fetch_service_instance::Variables {
            service_id: service_id.to_owned(),
            environment_id: environment_id.to_owned(),
        };
        let data = match self.run::<FetchServiceInstance>(vars, "serviceInstance") {
            Ok(d) => d,
            // Railway sometimes returns "Not found" rather than null
            // when the instance hasn't been created yet — treat both as
            // "not ready".
            Err(e) if e.to_string().to_lowercase().contains("not found") => return Ok(None),
            Err(e) => return Err(e),
        };
        let si = data.service_instance;
        Ok(Some(ServiceInstanceSnapshot {
            id: si.id,
            is_updatable: si.is_updatable,
            num_replicas: si.num_replicas.map(|n| n as u32),
            region: si.region,
            healthcheck_path: si.healthcheck_path,
            source_image: si.source.and_then(|s| s.image),
            latest_deployment_id: si.latest_deployment.as_ref().map(|d| d.id.clone()),
            latest_deployment_status: si
                .latest_deployment
                .map(|d| format!("{:?}", d.status).to_uppercase()),
        }))
    }

    // ─── Deployment verification ────────────────────────────────────────

    /// Fetch the most recent deployment for a (project, service,
    /// environment) triple. Returns `None` when Railway has no
    /// deployment on record yet — caller decides whether to retry or
    /// bail.
    pub fn latest_deployment(
        &self,
        project_id: &str,
        service_id: &str,
        environment_id: &str,
    ) -> Result<Option<DeploymentInfo>> {
        let vars = latest_deployment::Variables {
            input: latest_deployment::DeploymentListInput {
                environment_id: Some(environment_id.to_owned()),
                project_id: Some(project_id.to_owned()),
                service_id: Some(service_id.to_owned()),
                include_deleted: None,
                status: None,
            },
        };
        let data = self.run::<LatestDeployment>(vars, "deployments")?;
        let Some(edge) = data.deployments.edges.into_iter().next() else {
            return Ok(None);
        };
        let node = edge.node;
        Ok(Some(DeploymentInfo {
            id: node.id,
            status: format!("{:?}", node.status).to_uppercase(),
            url: node.url,
            static_url: node.static_url,
            created_at: format!("{:?}", node.created_at),
        }))
    }

    /// Poll `latest_deployment` until it returns a deployment whose
    /// status is a terminal one (success or failure) or the deadline
    /// elapses. A spinner ticks with the current status so callers can
    /// see progress.
    pub fn wait_for_deployment(
        &self,
        project_id: &str,
        service_id: &str,
        environment_id: &str,
        service_name: &str,
        timeout_secs: u64,
        target_deployment_id: Option<&str>,
    ) -> Result<DeploymentOutcome> {
        let started = std::time::Instant::now();
        let deadline = started + Duration::from_secs(timeout_secs);
        loop {
            let elapsed = started.elapsed().as_secs();
            // When the caller just triggered a *specific* deployment
            // (e.g. via `deploymentRedeploy` which returns a new id),
            // poll that one by id rather than "latest for the service"
            // — Railway may not have promoted the freshly-cloned
            // deployment to latest yet, and accepting an older
            // SUCCESS deployment as the wait anchor would be a false
            // positive.
            let latest = match target_deployment_id {
                Some(id) => self.fetch_deployment(id)?,
                None => self.latest_deployment(project_id, service_id, environment_id)?,
            };
            match latest.as_ref() {
                Some(d) => {
                    info!(
                        target: "pocopine.log",
                        service = %service_name,
                        deployment = %d.id,
                        status = %d.status,
                        elapsed_s = elapsed,
                        "railway deployment status",
                    );
                    match d.status.as_str() {
                        "SUCCESS" | "SLEEPING" => {
                            return Ok(DeploymentOutcome::Live(latest.unwrap()));
                        }
                        "FAILED" | "CRASHED" | "REMOVED" => {
                            return Ok(DeploymentOutcome::Failed(latest.unwrap()));
                        }
                        "SKIPPED" => return Ok(DeploymentOutcome::Skipped(latest.unwrap())),
                        // BUILDING / DEPLOYING / QUEUED / INITIALIZING /
                        // WAITING / NEEDS_APPROVAL / REMOVING — still in
                        // flight; keep polling.
                        _ => {}
                    }
                }
                None => {
                    info!(
                        target: "pocopine.log",
                        service = %service_name,
                        elapsed_s = elapsed,
                        "railway: no deployment record yet — waiting for Railway to register the trigger",
                    );
                }
            }
            if std::time::Instant::now() >= deadline {
                return Ok(DeploymentOutcome::Timeout(latest));
            }
            std::thread::sleep(Duration::from_secs(4));
        }
    }

    /// Fetch the latest `limit` build + runtime log lines for a
    /// deployment, build-phase first then runtime. Best-effort: log
    /// queries can fail independently (e.g. build never started, so
    /// `buildLogs` returns an empty list), and we'd rather print
    /// whatever we have than swallow the original deploy failure.
    pub fn fetch_deployment_logs(&self, deployment_id: &str, limit: i64) -> Vec<LogEntry> {
        let mut out = Vec::new();

        let build = self
            .run::<BuildLogs>(
                build_logs::Variables {
                    deployment_id: deployment_id.to_owned(),
                    limit: Some(limit),
                },
                "buildLogs",
            )
            .map(|d| d.build_logs)
            .unwrap_or_default();
        for l in build {
            out.push(LogEntry {
                phase: LogPhase::Build,
                timestamp: l.timestamp,
                severity: l.severity,
                message: l.message,
            });
        }

        let runtime = self
            .run::<DeploymentLogs>(
                deployment_logs::Variables {
                    deployment_id: deployment_id.to_owned(),
                    limit: Some(limit),
                },
                "deploymentLogs",
            )
            .map(|d| d.deployment_logs)
            .unwrap_or_default();
        for l in runtime {
            out.push(LogEntry {
                phase: LogPhase::Runtime,
                timestamp: l.timestamp,
                severity: l.severity,
                message: l.message,
            });
        }

        out
    }

    // ─── Domains ─────────────────────────────────────────────────────────

    /// Best-effort: ask Railway to generate a public domain for a
    /// service. Returns `Some(domain)` on success; any failure (e.g. a
    /// domain already exists) is logged and yields `None` rather than
    /// failing the deploy — the URL is cosmetic, not load-bearing.
    pub fn create_service_domain(&self, service_id: &str, environment_id: &str) -> Option<String> {
        let vars = create_service_domain::Variables {
            input: create_service_domain::ServiceDomainCreateInput {
                environment_id: environment_id.to_owned(),
                service_id: service_id.to_owned(),
                target_port: None,
            },
        };
        match self.run::<CreateServiceDomain>(vars, "serviceDomainCreate") {
            Ok(data) => Some(data.service_domain_create.domain),
            Err(e) => {
                info!(target: "pocopine.log", service = %service_id, "railway serviceDomainCreate skipped: {e}");
                None
            }
        }
    }
}

// ─── Public types ──────────────────────────────────────────────────────

/// Per-environment service-instance config for
/// [`RailwayClient::update_service_instance`].
#[derive(Debug, Clone)]
pub struct ServiceInstanceConfig {
    /// Fully-qualified image URL Railway pulls (`{registry}/{app}:{sha}`).
    pub image: String,
    /// Replica count. Floored to 1 by the adapter before it gets here.
    pub num_replicas: u32,
    /// HTTP healthcheck path; omitted when `None`.
    pub healthcheck_path: Option<String>,
    /// Railway region slug; omitted when `None`.
    pub region: Option<String>,
    /// Pull credentials for a private image. `None` = public image —
    /// Railway pulls anonymously.
    pub registry_credentials: Option<pocopine_deploy::RegistryCredentials>,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// `Some(timestamp)` while Railway is holding the project in its
    /// 2-day soft-delete window. The dashboard hides these but the
    /// `projects` query still returns them — using one as if it were
    /// live is the "deploy succeeded but I see nothing" footgun.
    pub deleted_at: Option<String>,
    environments: Vec<Environment>,
    services: Vec<Service>,
}

impl Project {
    /// The environment matching `name`, if the project has one.
    pub fn environment(&self, name: &str) -> Option<&Environment> {
        self.environments.iter().find(|e| e.name == name)
    }

    /// The `production` environment, or the first environment Railway
    /// reports if there is no `production`.
    pub fn default_environment(&self) -> Option<&Environment> {
        self.environment("production")
            .or_else(|| self.environments.first())
    }

    /// The service in this project with the given name, if any.
    /// Soft-deleted services are skipped — Railway holds the name
    /// reserved for ~2 days after deletion but the dashboard hides
    /// them; treating one as "existing" would put us back on the bad
    /// path where downstream mutations land in the void.
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services
            .iter()
            .find(|s| s.name == name && s.deleted_at.is_none())
    }
}

#[derive(Debug, Clone)]
pub struct Environment {
    pub id: String,
    pub name: String,
}

/// Read-only view of a [`ServiceInstance`] — the per-environment row
/// the Railway dashboard shows on the "Settings" tab. Used to detect
/// whether a `serviceInstanceUpdate` actually queued a new deployment
/// (via `latest_deployment_id` diff) and to dump diagnostics when the
/// deploy gets wedged.
#[derive(Debug, Clone)]
pub struct ServiceInstanceSnapshot {
    pub id: String,
    pub is_updatable: bool,
    pub num_replicas: Option<u32>,
    pub region: Option<String>,
    pub healthcheck_path: Option<String>,
    pub source_image: Option<String>,
    pub latest_deployment_id: Option<String>,
    pub latest_deployment_status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeploymentInfo {
    pub id: String,
    /// Uppercase status string (e.g. `SUCCESS`, `BUILDING`,
    /// `FAILED`). Mirrors Railway's `DeploymentStatus` enum.
    pub status: String,
    pub url: Option<String>,
    pub static_url: Option<String>,
    pub created_at: String,
}

/// Which phase a log line was emitted from. Build logs are usually
/// where deploy failures surface (image pull, registry auth, missing
/// binaries); runtime logs surface crashes after the container starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogPhase {
    Build,
    Runtime,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub phase: LogPhase,
    pub timestamp: String,
    pub severity: Option<String>,
    pub message: String,
}

/// Terminal outcome from [`RailwayClient::wait_for_deployment`].
/// `Timeout` carries the last status we saw (or `None` if no deployment
/// ever appeared) so the caller can describe the wedge.
#[derive(Debug, Clone)]
pub enum DeploymentOutcome {
    /// Reached `SUCCESS` / `SLEEPING`.
    Live(DeploymentInfo),
    /// Reached `FAILED` / `CRASHED` / `REMOVED`.
    Failed(DeploymentInfo),
    /// Reached `SKIPPED` — Railway no-opped the deploy (e.g. an
    /// identical image was already running). Not a failure; callers
    /// should treat the prior live deployment as the current state.
    Skipped(DeploymentInfo),
    /// Timed out before reaching a terminal state.
    Timeout(Option<DeploymentInfo>),
}

#[derive(Debug, Clone)]
pub struct Service {
    pub id: String,
    pub name: String,
    /// `Some(timestamp)` while Railway is holding the service in its
    /// soft-delete window (same 2-day grace period that applies to
    /// projects). The dashboard hides these but `projects` still
    /// returns them — re-using one as if it were live silently drops
    /// every subsequent mutation. `Project::service` filters them out.
    pub deleted_at: Option<String>,
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_project() -> Project {
        Project {
            id: "proj_1".into(),
            name: "myapp".into(),
            deleted_at: None,
            environments: vec![
                Environment {
                    id: "env_prod".into(),
                    name: "production".into(),
                },
                Environment {
                    id: "env_stg".into(),
                    name: "staging".into(),
                },
            ],
            services: vec![Service {
                id: "svc_web".into(),
                name: "myapp-web".into(),
                deleted_at: None,
            }],
        }
    }

    #[test]
    fn project_environment_and_service_lookup() {
        let p = sample_project();
        assert_eq!(
            p.environment("staging").map(|e| e.id.as_str()),
            Some("env_stg")
        );
        assert_eq!(
            p.default_environment().map(|e| e.id.as_str()),
            Some("env_prod"),
        );
        assert_eq!(
            p.service("myapp-web").map(|s| s.id.as_str()),
            Some("svc_web")
        );
        assert!(p.service("missing").is_none());
    }

    #[test]
    fn default_environment_falls_back_to_first_when_no_production() {
        let mut p = sample_project();
        p.environments = vec![Environment {
            id: "env_dev".into(),
            name: "development".into(),
        }];
        assert_eq!(
            p.default_environment().map(|e| e.id.as_str()),
            Some("env_dev"),
        );
    }

    #[test]
    fn new_uses_public_graphql_endpoint() {
        let c = RailwayClient::new("tok");
        assert_eq!(c.endpoint, RAILWAY_GRAPHQL);
    }
}
