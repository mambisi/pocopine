//! API-direct Cloudflare Pages static deploy adapter (RFC 080).
//!
//! The adapter assembles the shared [`pocopine_deploy::Artefact::StaticDist`]
//! and speaks Cloudflare's Pages Direct Upload protocol itself. It never
//! shells out to Wrangler.

#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub mod client;

use anyhow::{Context, Result, bail};
use pocopine_deploy::{
    AdapterMode, Artefact, Constraint, DeployAdapter, DeployOutcome, DeploySpec, Hint, Mode,
    ProcessStatus, StagedFiles, build_static_dist, static_dist_path,
};
use serde::Deserialize;

const HOST: &str = "cf-pages";

/// Built-in Cloudflare Pages adapter, selected with `--target cf-pages`.
pub struct CloudflarePagesAdapter;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloudflarePagesOverride {
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    production_branch: Option<String>,
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedConfig {
    account_id: String,
    project: String,
    production_branch: String,
    branch: String,
}

impl DeployAdapter for CloudflarePagesAdapter {
    fn name(&self) -> &'static str {
        HOST
    }

    fn mode(&self) -> AdapterMode {
        AdapterMode::Static
    }

    fn tested_against(&self) -> semver::VersionReq {
        semver::VersionReq::parse(">=4.0.0, <5.0.0").expect("static range parses")
    }

    fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint> {
        let mut constraints = Vec::new();

        if spec.mode != Mode::Static {
            constraints.push(Constraint::Refuse(
                "cf-pages is static-only; set `[package.metadata.pocopine.deploy] mode = \"static\"`"
                    .into(),
            ));
        }
        if !spec.processes.is_empty() {
            constraints.push(Constraint::Refuse(
                "cf-pages cannot run server or worker processes; remove `[deploy.processes]` or choose a fullstack target"
                    .into(),
            ));
        }
        if !spec.services.is_empty() {
            constraints.push(Constraint::Refuse(
                "cf-pages cannot provision runtime services; remove `[deploy.services]` or choose a fullstack target"
                    .into(),
            ));
        }

        let mut features = Vec::new();
        if spec.uses_jobs {
            features.push("jobs");
        }
        if spec.uses_collab {
            features.push("collab");
        }
        if spec.uses_storage {
            features.push("server-side storage");
        }
        if spec.uses_websocket {
            features.push("websockets");
        }
        if !features.is_empty() {
            constraints.push(Constraint::Refuse(format!(
                "cf-pages cannot host runtime features detected in this app: {}",
                features.join(", ")
            )));
        }

        match resolved_config(spec) {
            Ok(_) => {}
            Err(error) => constraints.push(Constraint::Refuse(format!(
                "invalid cf-pages configuration: {error:#}"
            ))),
        }

        constraints
    }

    fn render_config(&self, _spec: &DeploySpec, _out: &mut StagedFiles) {
        // Direct Upload needs no checked-in or generated vendor config.
    }

    fn build_artefact(&self, spec: &DeploySpec) -> Result<Artefact> {
        build_static_dist(spec)
    }

    fn default_artefact(&self, spec: &DeploySpec) -> Artefact {
        Artefact::StaticDist {
            path: static_dist_path(spec),
        }
    }

    fn deploy(&self, spec: &DeploySpec, artefact: &Artefact) -> Result<DeployOutcome> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (spec, artefact);
            bail!("cf-pages deployment is available only on the host target")
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            use pocopine_crypto::SecretString;

            let config = resolved_config(spec)?;
            let dist = match artefact {
                Artefact::StaticDist { path } => path,
                Artefact::OciImage { .. } => {
                    bail!("cf-pages expected a static dist artefact, not an OCI image")
                }
            };
            if !dist.is_dir() {
                bail!(
                    "cf-pages static dist does not exist at `{}`; run without --skip-build or build it first",
                    dist.display()
                );
            }

            let token = pocopine_deploy::credentials::load(HOST).context(
                "cf-pages: no API token. Run `pocopine deploy auth cf-pages` or export POCOPINE_CF_PAGES_TOKEN",
            )?;
            let client = client::PagesClient::new(SecretString::new(token))?;
            client.ensure_project(
                &config.account_id,
                &config.project,
                &config.production_branch,
            )?;
            let deployment = client.deploy_directory(
                &config.account_id,
                &config.project,
                &config.branch,
                &spec.git_sha,
                dist,
            )?;

            Ok(DeployOutcome {
                url: deployment.url,
                host_ids: vec![config.project, deployment.id],
            })
        }
    }

    fn post_deploy_hint(&self, _spec: &DeploySpec, outcome: &DeployOutcome) -> Vec<Hint> {
        vec![Hint::Info(format!(
            "Cloudflare Pages deployment: {}",
            outcome.url
        ))]
    }

    fn status(&self, spec: &DeploySpec) -> Result<Vec<ProcessStatus>> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = spec;
            bail!("cf-pages status is available only on the host target")
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            use pocopine_crypto::SecretString;

            let config = resolved_config(spec)?;
            let token = pocopine_deploy::credentials::load(HOST).context(
                "cf-pages: no API token. Run `pocopine deploy auth cf-pages` or export POCOPINE_CF_PAGES_TOKEN",
            )?;
            let client = client::PagesClient::new(SecretString::new(token))?;
            let environment = if config.branch == config.production_branch {
                "production"
            } else {
                "preview"
            };
            let deployment =
                client.latest_deployment(&config.account_id, &config.project, Some(environment))?;
            Ok(vec![client::deployment_status(
                &config.project,
                deployment.as_ref(),
            )])
        }
    }
}

fn resolved_config(spec: &DeploySpec) -> Result<ResolvedConfig> {
    let overrides = spec
        .host_override::<CloudflarePagesOverride>(HOST)
        .context("parsing `[deploy.cf-pages]`")?
        .unwrap_or_default();

    let account_id =
        pocopine_deploy::config::resolve(HOST, "account_id", overrides.account_id.as_deref())
            .unwrap_or_default();
    let project = pocopine_deploy::config::resolve(HOST, "project", overrides.project.as_deref())
        .unwrap_or_else(|| spec.package_name.clone());
    let production_branch = pocopine_deploy::config::resolve(
        HOST,
        "production_branch",
        overrides.production_branch.as_deref(),
    )
    .unwrap_or_else(|| "main".into());
    let preview_branch =
        pocopine_deploy::config::resolve(HOST, "branch", overrides.branch.as_deref())
            .unwrap_or_else(|| "preview".into());
    let branch = if spec.environment.as_deref() == Some("production") {
        production_branch.clone()
    } else {
        preview_branch
    };

    let account_id = required_nonempty("account_id", account_id).context(
        "set `[deploy.cf-pages].account_id`, run `pocopine deploy config set cf-pages account_id <id>`, or export POCOPINE_CF_PAGES_ACCOUNT_ID",
    )?;

    Ok(ResolvedConfig {
        account_id,
        project: required_nonempty("project", project)?,
        production_branch: required_nonempty("production_branch", production_branch)?,
        branch: required_nonempty("branch", branch)?,
    })
}

fn required_nonempty(field: &str, value: String) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("`{field}` must not be empty");
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use pocopine_deploy::{ProcessSpec, Scale, ServiceSpec};

    fn spec(mode: Mode) -> DeploySpec {
        let override_value: toml::Value = toml::toml! {
            account_id = "account-123"
            project = "my-site"
            production_branch = "trunk"
            branch = "feature-preview"
        }
        .into();
        DeploySpec {
            app_name: "site".into(),
            package_name: "site".into(),
            git_sha: "abc1234".into(),
            git_remote: None,
            mode,
            processes: BTreeMap::new(),
            services: BTreeMap::new(),
            env: BTreeMap::new(),
            host_overrides: BTreeMap::from([(HOST.into(), override_value)]),
            uses_jobs: false,
            uses_collab: false,
            uses_storage: false,
            uses_websocket: false,
            first_deploy: false,
            skip_build: false,
            environment: None,
            workspace_subpath: "examples/site".into(),
            has_rust_toolchain: false,
            static_files: vec!["index.html".into(), "pkg".into()],
            build_dir: None,
        }
    }

    #[test]
    fn adapter_is_static_and_renders_no_vendor_files() {
        let adapter = CloudflarePagesAdapter;
        assert_eq!(adapter.name(), "cf-pages");
        assert_eq!(adapter.mode(), AdapterMode::Static);
        let mut staged = StagedFiles::new();
        adapter.render_config(&spec(Mode::Static), &mut staged);
        assert!(staged.is_empty());
    }

    #[test]
    fn static_spec_with_account_is_accepted() {
        let constraints = CloudflarePagesAdapter.detect_constraints(&spec(Mode::Static));
        assert!(
            constraints
                .iter()
                .all(|constraint| !matches!(constraint, Constraint::Refuse(_))),
            "{constraints:?}"
        );
    }

    #[test]
    fn fullstack_processes_services_and_runtime_features_are_refused() {
        let mut spec = spec(Mode::Fullstack);
        spec.processes.insert(
            "web".into(),
            ProcessSpec {
                bin: "server".into(),
                port: Some(8080),
                healthcheck: None,
                scale: Scale::default(),
                public: None,
            },
        );
        spec.services
            .insert("postgres".into(), ServiceSpec { required: true });
        spec.uses_jobs = true;

        let refusals = CloudflarePagesAdapter
            .detect_constraints(&spec)
            .into_iter()
            .filter(|constraint| matches!(constraint, Constraint::Refuse(_)))
            .count();
        assert_eq!(refusals, 4);
    }

    #[test]
    fn production_uses_production_branch_and_preview_uses_branch() {
        let preview = resolved_config(&spec(Mode::Static)).unwrap();
        assert_eq!(preview.branch, "feature-preview");

        let mut production = spec(Mode::Static);
        production.environment = Some("production".into());
        let production = resolved_config(&production).unwrap();
        assert_eq!(production.branch, "trunk");
    }

    #[test]
    fn default_artefact_points_at_shared_dist_path() {
        let spec = spec(Mode::Static);
        assert!(matches!(
            CloudflarePagesAdapter.default_artefact(&spec),
            Artefact::StaticDist { path }
                if path == std::path::Path::new("examples/site/.pocopine/build/dist")
        ));
    }
}
