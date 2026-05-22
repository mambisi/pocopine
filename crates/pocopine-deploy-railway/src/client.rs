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
railway_op!(UpdateServiceInstance);
railway_op!(UpsertVariables);
railway_op!(DeployServiceInstance);
railway_op!(CreateServiceDomain);

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
    /// matches exactly.
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
    pub fn ensure_project(&self, name: &str, workspace_id: Option<&str>) -> Result<Project> {
        if let Some(existing) = self.find_project(name)? {
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
    pub fn create_service(&self, project_id: &str, name: &str, image: &str) -> Result<Service> {
        info!(target: "pocopine.log", project = %project_id, service = %name, "railway serviceCreate");
        let vars = create_service::Variables {
            input: create_service::ServiceCreateInput {
                branch: None,
                environment_id: None,
                icon: None,
                name: Some(name.to_owned()),
                project_id: project_id.to_owned(),
                registry_credentials: None,
                source: Some(create_service::ServiceSourceInput {
                    image: Some(image.to_owned()),
                    repo: None,
                }),
                template_id: None,
                template_service_id: None,
                variables: None,
            },
        };
        let data = self.run::<CreateService>(vars, "serviceCreate")?;
        Ok(Service {
            id: data.service_create.id,
            name: data.service_create.name,
        })
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

    /// Trigger a deployment of a service instance via the documented
    /// `serviceInstanceDeployV2` mutation.
    pub fn deploy_service_instance(&self, service_id: &str, environment_id: &str) -> Result<()> {
        info!(target: "pocopine.log", service = %service_id, env = %environment_id, "railway serviceInstanceDeployV2");
        let vars = deploy_service_instance::Variables {
            service_id: service_id.to_owned(),
            environment_id: environment_id.to_owned(),
        };
        self.run::<DeployServiceInstance>(vars, "serviceInstanceDeployV2")?;
        Ok(())
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
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct Environment {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Service {
    pub id: String,
    pub name: String,
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_project() -> Project {
        Project {
            id: "proj_1".into(),
            name: "myapp".into(),
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
