//! Fly.io deploy adapter for pocopine (RFC 080 §6).
//!
//! Targets the Fly Machines API directly (https://api.machines.dev/v1).
//! No `flyctl` shell-out; auth is via `$FLY_API_TOKEN` or
//! `~/.pocopine/credentials.toml` (RFC 080 §5.5).

#![forbid(unsafe_code)]

// The Machines API client is host-only — it pulls in `reqwest::blocking`,
// which `reqwest` cfgs out on `wasm32-unknown-unknown`. The trait impl
// below stays available on every target so workspace-wide checks pass.
#[cfg(not(target_arch = "wasm32"))]
pub mod machines;

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Context;
use anyhow::Result;
use pocopine_deploy::{
    common, AdapterMode, Artefact, Constraint, DeployAdapter, DeployOutcome, DeploySpec, Hint,
    Mode, StagedFiles,
};
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
struct FlyOverride {
    #[serde(default)]
    regions: Vec<String>,
    #[serde(default)]
    primary_region: Option<String>,
    /// Fly organisation slug. Defaults to `personal`. Read only by the
    /// host-only `deploy` path; flagged unused on wasm32 otherwise.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    volumes: Vec<FlyVolume>,
}

#[derive(Debug, Clone, Deserialize)]
struct FlyVolume {
    process: String,
    path: String,
    size_gb: u32,
}

/// Metadata key used on every Machine we create so re-deploys can
/// identify the machines that belong to this app's processes (and
/// destroy them) without touching unrelated machines.
#[cfg(not(target_arch = "wasm32"))]
const PROCESS_METADATA_KEY: &str = "pocopine.process";

/// Wait timeout (seconds) passed to Fly's `/machines/{id}/wait?timeout=…`
/// endpoint. The API caps this at 60 s server-side, so we send 60 s.
/// A future iteration can poll in chunks for longer overall deadlines.
#[cfg(not(target_arch = "wasm32"))]
const WAIT_STARTED_SECS: u32 = 60;

// Phase 6 lifted the SUPPORTED_PROCESSES refusal. The runtime launcher
// now resolves process names via `POCOPINE_PROC_<UPPER>` env vars that
// `pocopine-deploy::common::render_dockerfile` emits from
// `[deploy.processes]`, so any name + bin combination works as long
// as the bin is declared in Cargo.toml and built by the image.

/// Fly Machine names must match `[a-z0-9][a-z0-9_-]*`. The same
/// regex applies to `[processes]` keys in `fly.toml`. We re-check
/// before rendering / API call so unsafe identifiers don't slip
/// through and surface as a 4xx from the Machines API.
fn is_fly_safe_process_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub struct FlyAdapter;

impl DeployAdapter for FlyAdapter {
    fn name(&self) -> &'static str {
        "fly"
    }

    fn mode(&self) -> AdapterMode {
        AdapterMode::Fullstack
    }

    fn tested_against(&self) -> semver::VersionReq {
        // Fly Machines API is v1 (api.machines.dev/v1/*). Bump on
        // schema-major moves; `doctor` probes the API's version endpoint
        // and warns outside this range.
        semver::VersionReq::parse(">=1.0.0, <2.0.0").expect("static range parses")
    }

    fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint> {
        let mut out = Vec::new();

        if spec.mode == Mode::Static {
            out.push(Constraint::Refuse(
                "fly is fullstack-only; use `cf-pages` for static apps".into(),
            ));
        }

        if !spec.has_process("web") {
            out.push(Constraint::Warn(
                "no `web` process declared — Fly's public load balancer needs one to route traffic"
                    .into(),
            ));
        }

        for (name, proc) in spec.processes() {
            if proc.is_public() && proc.port.is_none() {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` is public but declares no port"
                )));
            }
        }

        // Phase 6: launcher dispatch is env-var-driven, so any process
        // name + bin combo is fine. The render_dockerfile helper emits
        // `ENV POCOPINE_PROC_<NAME>=/usr/local/bin/<bin>` per declared
        // process, and the launcher reads them.
        for (name, proc) in spec.processes() {
            if proc.bin.is_empty() {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` has empty `bin` — declare the cargo bin name to launch.",
                )));
            }
            if !is_fly_safe_process_name(name) {
                out.push(Constraint::Refuse(format!(
                    "process name `{name}` is not a valid Fly Machine name. Allowed: lowercase a-z, 0-9, `-`, `_`; must start with a letter or digit.",
                )));
            }
        }

        // Two process names that collapse to the same
        // `POCOPINE_PROC_<KEY>` would silently exec each other's
        // binary (Docker keeps the later `ENV`). Refuse before deploy
        // turns into a broken machine.
        for (env, names) in common::process_env_collisions(spec) {
            out.push(Constraint::Refuse(format!(
                "process names {names:?} collide on launcher env var `{env}`. Rename so they normalise distinctly (e.g. `api-worker` vs `api2-worker`).",
            )));
        }

        // Backing services declared as required must have an env entry
        // (literal or `from = "env"`) that resolves to the connection
        // URL. The adapter does not provision Postgres/Redis itself yet;
        // letting the deploy proceed without these URLs would silently
        // boot a broken machine.
        if spec.requires_postgres() && !env_provides(spec, "DATABASE_URL") {
            out.push(Constraint::Refuse(
                "postgres is required but no `DATABASE_URL` entry in [deploy.env]. Add a literal or `{ from = \"env\" }` entry (Fly Postgres / external connection string)."
                    .into(),
            ));
        }
        if spec.requires_redis() && !env_provides(spec, "REDIS_URL") {
            out.push(Constraint::Refuse(
                "redis is required but no `REDIS_URL` entry in [deploy.env]. Add a literal or `{ from = \"env\" }` entry (Upstash partner add-on or external)."
                    .into(),
            ));
        }

        // Phase 9 wires Fly's Volumes API. Volumes attach to a single
        // machine at a time, so a process with a declared volume can't
        // safely scale beyond 1 replica yet (per-replica volumes are
        // a follow-up). Refuse unsafe combinations fail-fast.
        let overrides = fly_override(spec);
        let mut seen_processes: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for vol in &overrides.volumes {
            // Two volume entries with the same `process` would both
            // resolve to the Fly volume named `{process}_data`,
            // silently collapsing into one disk with two mount paths.
            // Refuse until we support entry-specific volume names.
            if !seen_processes.insert(vol.process.as_str()) {
                out.push(Constraint::Refuse(format!(
                    "[deploy.fly].volumes has more than one entry for process `{}`. Each volume would share the name `{}_data` and collapse into one disk; declare a single entry per process (multi-volume support is a follow-up).",
                    vol.process, vol.process,
                )));
            }
            let proc = spec.process(&vol.process);
            match proc {
                None => out.push(Constraint::Refuse(format!(
                    "[deploy.fly].volumes references process `{}` which is not declared in [deploy.processes].",
                    vol.process,
                ))),
                Some(p) if p.scale.min > 1 => out.push(Constraint::Refuse(format!(
                    "process `{}` declares scale.min = {} but also mounts a volume. Volumes attach to one machine; per-replica volumes are a follow-up. Lower scale to 1 or remove the volume for now.",
                    vol.process, p.scale.min,
                ))),
                _ => {}
            }
        }

        // `{ from = "secret" }` is now wired through Fly's secrets API
        // (Phase 7). The value still has to be present in this process's
        // environment so we can push it; refuse fail-fast on missing
        // values rather than after build + push.
        {
            use pocopine_deploy::{EnvSource, EnvValue};
            let missing_secret_values: Vec<&str> = spec
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
            if !missing_secret_values.is_empty() {
                out.push(Constraint::Refuse(format!(
                    "fly secrets need values: keys declared `{{ from = \"secret\" }}` aren't set in this process: {}. Export them before deploying so the adapter can push them via the Fly secrets API.",
                    missing_secret_values.join(", "),
                )));
            }
        }

        // Fly supports scale-to-zero via service `auto_stop`, but that's
        // a Fly-side knob the adapter doesn't currently configure for
        // non-public processes. Floor `scale.min = 0` to 1 with a warning
        // per RFC 080 §11 q2; sanity-cap large values until we have
        // budget guardrails.
        for (name, proc) in spec.processes() {
            if proc.scale.min == 0 {
                out.push(Constraint::Warn(format!(
                    "process `{name}` scale.min = 0; flooring to 1 (scale-to-zero on Fly requires service auto_stop; not wired in this phase)",
                )));
            }
            if proc.scale.min > 10 {
                out.push(Constraint::Refuse(format!(
                    "process `{name}` scale.min = {} exceeds the safety cap (10). Lower it or remove the cap in a follow-up.",
                    proc.scale.min,
                )));
            }
        }

        if spec.uses_collab {
            out.push(Constraint::Hint(
                "collab detected — sticky sessions disabled on `web` (Redis is the coordination layer per RFC 073)"
                    .into(),
            ));
        }

        if spec.requires_redis() {
            out.push(Constraint::Hint(
                "Fly has no first-party Redis; the adapter wires REDIS_URL to an Upstash partner add-on"
                    .into(),
            ));
        }

        if spec.requires_postgres() {
            out.push(Constraint::Hint(
                "Fly Postgres will be provisioned on first deploy (or set DATABASE_URL to an external instance)"
                    .into(),
            ));
        }

        out
    }

    fn render_config(&self, spec: &DeploySpec, out: &mut StagedFiles) {
        out.write("Dockerfile", common::render_dockerfile(spec));
        out.write("Dockerfile.dockerignore", common::dockerignore());
        out.write("fly.toml", render_fly_toml(spec));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn build_artefact(&self, spec: &DeploySpec) -> Result<Artefact> {
        use pocopine_deploy::docker::DockerClient;
        use std::path::Path;

        let tag = image_tag(spec);
        let docker = DockerClient::new();
        // Context `.` is the workspace root (set by the CLI) so sibling
        // crates like pocopine-launcher are visible to the build stage.
        let dockerfile = spec.dockerfile_path();
        docker
            .build(Path::new("."), &tag, Some(Path::new(&dockerfile)))
            .context("fly: docker build failed")?;
        Ok(Artefact::OciImage { tag })
    }

    #[cfg(target_arch = "wasm32")]
    fn build_artefact(&self, _spec: &DeploySpec) -> Result<Artefact> {
        anyhow::bail!("fly adapter not available on wasm32 target")
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn default_artefact(&self, spec: &DeploySpec) -> Artefact {
        Artefact::OciImage {
            tag: image_tag(spec),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn deploy(&self, spec: &DeploySpec, artefact: &Artefact) -> Result<DeployOutcome> {
        use pocopine_deploy::docker::DockerClient;

        let Artefact::OciImage { tag } = artefact else {
            anyhow::bail!("fly: deploy requires an OCI image artefact");
        };

        let token = load_fly_token()?;

        // 1. Ensure the app exists FIRST — `registry.fly.io/<app>:<sha>`
        //    requires the app namespace to exist before `docker push`
        //    can target it (regression fix for the Phase 4 review).
        let overrides = fly_override(spec);
        let org = overrides
            .org
            .clone()
            .unwrap_or_else(|| "personal".to_owned());
        let region = primary_region(&overrides);

        let client = machines::MachinesClient::new(&token);
        client.ensure_app(&spec.app_name, &org)?;

        // 2a. Resolve plain env FIRST. If any `{ from = "env" }` value
        //     is missing locally we must fail before pushing anything
        //     to Fly's secrets store — otherwise a failed deploy could
        //     have already rotated production secrets.
        let env = resolve_env(spec)?;

        // 2b. Collect `{ from = "secret" }` values from local env and
        //     push them via Fly's secrets API (Phase 7). `detect_constraints`
        //     has already refused on missing values, so reaching here
        //     means every declared secret has a non-empty local value.
        let secret_values = collect_secret_values(spec);
        let min_secrets_version = if secret_values.is_empty() {
            None
        } else {
            // Push and pin freshly-created machines to the resulting
            // version so they see the values we just set rather than a
            // stale snapshot.
            let resp = client.set_secrets(&spec.app_name, &secret_values)?;
            Some(resp.version)
        };

        // 3. Push the locally-built image to Fly's registry, unless the
        //    caller passed `--skip-build` — that flag means the image
        //    is already in the host registry from a previous build
        //    (CI's build-once-deploy-many pattern; RFC 080 §7).
        if !spec.skip_build {
            let docker = DockerClient::new();
            docker
                .login(machines::FLY_REGISTRY, "x", &token)
                .context("fly: docker login failed")?;
            docker.push(tag).context("fly: docker push failed")?;
        }

        // 3b. Ensure volumes declared in `[deploy.fly].volumes` exist.
        //     Volume names are deterministic (`{process}_data`) so
        //     re-runs are idempotent: existing volumes are reused.
        let overrides = fly_override(spec);
        let mounts_by_process = ensure_volumes(&client, &spec.app_name, &region, &overrides)?;

        // 4. Diff existing machines against the declared
        //    (process, replica) set:
        //    - update in-place when a name matches an existing machine
        //      that we own (zero-downtime);
        //    - create for declared replicas that aren't on Fly yet;
        //    - destroy ours that no longer match any declaration
        //      (scale-down).
        use std::collections::{BTreeMap, BTreeSet};

        let existing = client.list_machines(&spec.app_name)?;
        let owned_by_name: BTreeMap<String, &machines::Machine> = existing
            .iter()
            .filter(|m| owns_machine(m))
            .map(|m| (m.name.clone(), m))
            .collect();

        let mut desired_names: BTreeSet<String> = BTreeSet::new();
        for (proc_name, proc) in spec.processes() {
            for replica in 1..=proc.scale.min.max(1) {
                desired_names.insert(format!("{proc_name}-{replica}"));
            }
        }

        for (name, m) in &owned_by_name {
            if !desired_names.contains(name) {
                client.destroy_machine(&spec.app_name, &m.id, true)?;
            }
        }

        // 5. Upsert one Machine per declared replica. Each is pinned to
        //    the latest secrets version we just pushed so a deploy that
        //    rotates secrets boots its machines against the new values.
        for (proc_name, proc) in spec.processes() {
            let replicas = proc.scale.min.max(1);
            for replica in 1..=replicas {
                let name = format!("{proc_name}-{replica}");
                let existing_id = owned_by_name.get(&name).map(|m| m.id.clone());
                let mounts = mounts_by_process
                    .get(proc_name)
                    .cloned()
                    .unwrap_or_default();
                upsert_machine_for_process(
                    &client,
                    spec,
                    &region,
                    tag,
                    &env,
                    min_secrets_version,
                    &mounts,
                    proc_name,
                    proc,
                    replica,
                    existing_id,
                )?;
            }
        }

        Ok(DeployOutcome {
            url: format!("https://{}.fly.dev", spec.app_name),
            host_ids: vec![spec.app_name.clone()],
        })
    }

    #[cfg(target_arch = "wasm32")]
    fn deploy(&self, _spec: &DeploySpec, _artefact: &Artefact) -> Result<DeployOutcome> {
        anyhow::bail!("fly adapter not available on wasm32 target")
    }

    fn post_deploy_hint(&self, _spec: &DeploySpec, outcome: &DeployOutcome) -> Vec<Hint> {
        vec![Hint::Info(format!("deployed to {}", outcome.url))]
    }
}

/// Ensure every declared `[deploy.fly].volumes` entry exists on Fly.
/// Returns `process_name → Vec<MachineMount>` so each affected machine
/// gets the right mount list.
///
/// Volume naming is deterministic (`{process}_data`) so re-runs are
/// idempotent: existing volumes are reused. Per-replica volumes are a
/// follow-up; `detect_constraints` refuses `scale.min > 1` for any
/// process that mounts a volume.
#[cfg(not(target_arch = "wasm32"))]
fn ensure_volumes(
    client: &machines::MachinesClient,
    app: &str,
    region: &str,
    overrides: &FlyOverride,
) -> Result<std::collections::BTreeMap<String, Vec<machines::MachineMount>>> {
    use std::collections::BTreeMap;
    let mut out: BTreeMap<String, Vec<machines::MachineMount>> = BTreeMap::new();
    for vol in &overrides.volumes {
        let name = format!("{}_data", vol.process);
        let v = client.ensure_volume(app, &name, region, vol.size_gb)?;
        out.entry(vol.process.clone())
            .or_default()
            .push(machines::MachineMount {
                volume: v.id,
                path: vol.path.clone(),
            });
    }
    Ok(out)
}

/// Either create a new Machine or update the one with `existing_id` in
/// place. The body shape is the same; Fly's update endpoint is
/// effectively `create_machine` against a specific machine ID.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn upsert_machine_for_process(
    client: &machines::MachinesClient,
    spec: &DeploySpec,
    region: &str,
    image_tag: &str,
    env: &std::collections::BTreeMap<String, String>,
    min_secrets_version: Option<u32>,
    mounts: &[machines::MachineMount],
    proc_name: &str,
    proc: &pocopine_deploy::ProcessSpec,
    replica: u32,
    existing_id: Option<String>,
) -> Result<()> {
    use std::collections::BTreeMap;

    let mut metadata: BTreeMap<String, String> = BTreeMap::new();
    metadata.insert(PROCESS_METADATA_KEY.to_owned(), proc_name.to_owned());
    metadata.insert("pocopine.replica".to_owned(), replica.to_string());

    // Only publish a Fly service when the process is *public*.
    // A private process with a port (e.g. health endpoint for an
    // internal worker) must not be reachable on 80/443.
    let mut services = Vec::new();
    if proc.is_public() {
        if let Some(port) = proc.port {
            let checks = match &proc.healthcheck {
                Some(path) => vec![machines::MachineServiceCheck {
                    kind: "http".to_owned(),
                    interval: "10s".to_owned(),
                    timeout: "5s".to_owned(),
                    grace_period: Some("5s".to_owned()),
                    method: Some("GET".to_owned()),
                    path: Some(path.clone()),
                }],
                None => Vec::new(),
            };
            services.push(machines::MachineService {
                internal_port: port,
                protocol: "tcp".to_owned(),
                ports: vec![
                    machines::MachinePort {
                        port: 80,
                        handlers: vec!["http".to_owned()],
                    },
                    machines::MachinePort {
                        port: 443,
                        handlers: vec!["tls".to_owned(), "http".to_owned()],
                    },
                ],
                autostart: Some(true),
                autostop: Some("suspend".to_owned()),
                checks,
            });
        }
    }

    let mut machine_env = env.clone();
    machine_env.extend(pocopine_deploy::common::port_env(proc));

    let create_req = machines::CreateMachineRequest {
        name: format!("{proc_name}-{replica}"),
        region: region.to_owned(),
        config: machines::MachineConfig {
            image: image_tag.to_owned(),
            // The launcher dispatches argv[1] to the right binary
            // (RFC 080 §5.3). `cmd` is what Fly passes after the
            // image's ENTRYPOINT.
            init: Some(machines::MachineInit {
                cmd: vec![proc_name.to_owned()],
                ..Default::default()
            }),
            env: machine_env,
            services,
            metadata,
            mounts: mounts.to_vec(),
        },
        skip_launch: None,
        skip_service_registration: None,
        min_secrets_version,
    };

    let machine = match existing_id {
        Some(id) => client.update_machine(&spec.app_name, &id, &create_req)?,
        None => client.create_machine(&spec.app_name, &create_req)?,
    };
    if let Some(instance_id) = machine.instance_id.as_deref() {
        client.wait_machine(
            &spec.app_name,
            &machine.id,
            instance_id,
            "started",
            WAIT_STARTED_SECS,
        )?;
    }
    Ok(())
}

/// `registry.fly.io/<app>:<git_sha>`. Pure; tested independently.
#[cfg(not(target_arch = "wasm32"))]
fn image_tag(spec: &DeploySpec) -> String {
    format!(
        "{}/{}:{}",
        machines::FLY_REGISTRY,
        spec.app_name,
        spec.git_sha
    )
}

/// `true` iff `[deploy.env]` declares `key` as a literal value,
/// `{ from = "env" }`, or `{ from = "secret" }`. Secret entries count
/// since Phase 7 — they're pushed through Fly's secrets API and end
/// up as env vars on the running machine.
fn env_provides(spec: &DeploySpec, key: &str) -> bool {
    spec.env.contains_key(key)
}

fn fly_override(spec: &DeploySpec) -> FlyOverride {
    spec.host_override::<FlyOverride>("fly")
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn primary_region(o: &FlyOverride) -> String {
    o.primary_region
        .clone()
        .or_else(|| o.regions.first().cloned())
        .unwrap_or_else(|| "fra".into())
}

/// Returns true iff the machine carries our `pocopine.process` metadata
/// key — i.e. we created it in a previous deploy. Driven by the
/// `config.metadata` field that `list_machines` returns alongside each
/// machine.
#[cfg(not(target_arch = "wasm32"))]
fn owns_machine(m: &machines::Machine) -> bool {
    m.metadata(PROCESS_METADATA_KEY).is_some()
}

/// Look up the Fly API token, in this order: `POCOPINE_FLY_TOKEN` env
/// var, then `~/.pocopine/credentials.toml` `[fly]`, then `FLY_API_TOKEN`
/// (flyctl's convention — so users with an existing flyctl setup don't
/// need to re-paste the same token).
#[cfg(not(target_arch = "wasm32"))]
fn load_fly_token() -> Result<String> {
    use pocopine_deploy::credentials;
    if let Ok(t) = credentials::load("fly") {
        return Ok(t);
    }
    if let Ok(t) = std::env::var("FLY_API_TOKEN") {
        if !t.is_empty() {
            return Ok(t);
        }
    }
    anyhow::bail!(
        "fly: no API token. Run `pocopine deploy auth fly`, or export $POCOPINE_FLY_TOKEN or $FLY_API_TOKEN.",
    )
}

/// Translate `[deploy.env]` into the env map for a Fly machine. Secret
/// values are intentionally **excluded** — they live in Fly's secrets
/// store (Phase 7) and Fly injects them as env vars at machine runtime,
/// so duplicating them in `MachineConfig.env` would just leak the value
/// in plaintext.
///
/// - `EnvValue::Literal(s)` → included verbatim.
/// - `{ from = "env" }` → resolved against this process's environment.
///   Missing values fail the deploy fast with a clear list.
/// - `{ from = "secret" }` → skipped here; handled by `collect_secret_values`.
#[cfg(not(target_arch = "wasm32"))]
fn resolve_env(spec: &DeploySpec) -> Result<std::collections::BTreeMap<String, String>> {
    use pocopine_deploy::{EnvSource, EnvValue};
    use std::collections::BTreeMap;

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut missing_env: Vec<String> = Vec::new();

    for (k, v) in &spec.env {
        match v {
            EnvValue::Literal(s) => {
                out.insert(k.clone(), s.clone());
            }
            EnvValue::Indirect {
                from: EnvSource::Env,
            } => match std::env::var(k) {
                Ok(v) if !v.is_empty() => {
                    out.insert(k.clone(), v);
                }
                _ => missing_env.push(k.clone()),
            },
            // Secrets are pushed via the Fly API by `collect_secret_values`
            // and `MachinesClient::set_secrets`. Not in MachineConfig.env.
            EnvValue::Indirect {
                from: EnvSource::Secret,
            } => {}
        }
    }

    if !missing_env.is_empty() {
        anyhow::bail!(
            "fly: `{{ from = \"env\" }}` values are not set in this process: {}. \
             Export them or change them to a literal in Pocopine.toml [deploy.env].",
            missing_env.join(", "),
        );
    }

    Ok(out)
}

/// Collect the secret-typed env values from local process env. Returns
/// only keys that have a non-empty value; `detect_constraints` has
/// already refused on missing values, so a deploy reaching this point
/// has every secret present.
#[cfg(not(target_arch = "wasm32"))]
fn collect_secret_values(spec: &DeploySpec) -> std::collections::BTreeMap<String, String> {
    use pocopine_deploy::{EnvSource, EnvValue};
    use std::collections::BTreeMap;

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &spec.env {
        if let EnvValue::Indirect {
            from: EnvSource::Secret,
        } = v
        {
            if let Ok(value) = std::env::var(k) {
                if !value.is_empty() {
                    out.insert(k.clone(), value);
                }
            }
        }
    }
    out
}

fn render_fly_toml(spec: &DeploySpec) -> String {
    let overrides = fly_override(spec);
    let primary_region = primary_region(&overrides);

    use std::fmt::Write as _;
    let mut out = String::new();

    let _ = writeln!(out, "# Generated by pocopine-deploy-fly. Do not hand-edit;");
    let _ = writeln!(
        out,
        "# changes go in Pocopine.toml [deploy] or [deploy.fly]."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "app             = \"{}\"", spec.app_name);
    let _ = writeln!(out, "primary_region  = \"{primary_region}\"");
    let _ = writeln!(out);
    let _ = writeln!(out, "[build]");
    let _ = writeln!(out, "dockerfile = \"Dockerfile\"");

    let process_names: Vec<&str> = spec.processes().map(|(name, _)| name).collect();

    if !process_names.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "[processes]");
        for name in &process_names {
            let _ = writeln!(out, "{name} = \"{name}\"");
        }
    }

    for (name, proc) in spec.processes() {
        if let Some(port) = proc.port {
            let _ = writeln!(out);
            let _ = writeln!(out, "[[services]]");
            let _ = writeln!(out, "processes      = [\"{name}\"]");
            let _ = writeln!(out, "internal_port  = {port}");
            let _ = writeln!(out, "auto_start     = true");
            let _ = writeln!(out, "auto_stop      = \"suspend\"");
            if let Some(hc) = &proc.healthcheck {
                let _ = writeln!(out);
                let _ = writeln!(out, "  [[services.http_checks]]");
                let _ = writeln!(out, "  path = \"{hc}\"");
            }
        }
    }

    if !process_names.is_empty() {
        let _ = writeln!(out);
        for name in &process_names {
            let _ = writeln!(out, "[[vm]]");
            let _ = writeln!(
                out,
                "processes = [\"{name}\"]; size = \"shared-cpu-1x\"; memory = \"512mb\""
            );
        }
    }

    for vol in &overrides.volumes {
        let _ = writeln!(out);
        let _ = writeln!(out, "[[mounts]]");
        let _ = writeln!(out, "source       = \"{}_data\"", vol.process);
        let _ = writeln!(out, "destination  = \"{}\"", vol.path);
        let _ = writeln!(out, "processes    = [\"{}\"]", vol.process);
        let _ = writeln!(out, "initial_size = \"{}gb\"", vol.size_gb);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_deploy::{ProcessSpec, Scale, ServiceSpec};
    use std::collections::BTreeMap;

    fn fullstack_spec() -> DeploySpec {
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
        DeploySpec {
            app_name: "test-app".into(),
            package_name: "test-app".into(),
            git_sha: "abc1234".into(),
            git_remote: None,
            mode: Mode::Fullstack,
            processes,
            services,
            env: Default::default(),
            host_overrides: Default::default(),
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
        }
    }

    #[test]
    fn refuses_static_mode() {
        let mut spec = fullstack_spec();
        spec.mode = Mode::Static;
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(_))));
    }

    #[test]
    fn warns_when_no_web_process() {
        let mut spec = fullstack_spec();
        spec.processes.remove("web");
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(|c| matches!(c, Constraint::Warn(_))));
    }

    #[test]
    fn renders_fly_toml_with_processes_and_services() {
        let spec = fullstack_spec();
        let mut staged = StagedFiles::new();
        FlyAdapter.render_config(&spec, &mut staged);

        let fly_toml = staged.get("fly.toml").expect("fly.toml emitted");
        assert!(fly_toml.contains("app             = \"test-app\""));
        assert!(fly_toml.contains("[processes]"));
        assert!(fly_toml.contains("web = \"web\""));
        assert!(fly_toml.contains("worker = \"worker\""));
        assert!(fly_toml.contains("internal_port  = 8080"));
        assert!(fly_toml.contains("path = \"/healthz\""));

        assert!(staged.get("Dockerfile").is_some());
        assert!(staged.get("Dockerfile.dockerignore").is_some());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn image_tag_uses_app_name_and_git_sha() {
        let spec = fullstack_spec();
        assert_eq!(image_tag(&spec), "registry.fly.io/test-app:abc1234");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn default_artefact_matches_image_tag_for_skip_build_path() {
        // `pocopine deploy --skip-build` reuses an already-pushed image.
        // The CLI calls `default_artefact` instead of `build_artefact`;
        // the two must agree on the tag or the redeploy targets a
        // different image than what the previous build pushed.
        let spec = fullstack_spec();
        let artefact = FlyAdapter.default_artefact(&spec);
        match artefact {
            Artefact::OciImage { tag } => {
                assert_eq!(tag, image_tag(&spec));
            }
            _ => panic!("expected OciImage artefact for fly"),
        }
    }

    #[test]
    fn primary_region_prefers_explicit_setting() {
        let o = FlyOverride {
            primary_region: Some("ord".into()),
            regions: vec!["fra".into(), "iad".into()],
            ..Default::default()
        };
        assert_eq!(primary_region(&o), "ord");
    }

    #[test]
    fn primary_region_falls_back_to_first_region() {
        let o = FlyOverride {
            primary_region: None,
            regions: vec!["iad".into()],
            ..Default::default()
        };
        assert_eq!(primary_region(&o), "iad");
    }

    #[test]
    fn primary_region_defaults_to_fra() {
        let o = FlyOverride::default();
        assert_eq!(primary_region(&o), "fra");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn owns_machine_true_for_pocopine_metadata() {
        let body = r#"{"id":"m1","name":"web-1","config":{"metadata":{"pocopine.process":"web"}}}"#;
        let m: machines::Machine = serde_json::from_str(body).unwrap();
        assert!(owns_machine(&m));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn owns_machine_false_for_external_machines() {
        let body = r#"{"id":"m1","name":"misc","config":{"metadata":{"owner":"someone-else"}}}"#;
        let m: machines::Machine = serde_json::from_str(body).unwrap();
        assert!(!owns_machine(&m));

        // No config at all.
        let body = r#"{"id":"m1","name":"misc"}"#;
        let m: machines::Machine = serde_json::from_str(body).unwrap();
        assert!(!owns_machine(&m));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_env_literal_passes_through() {
        use pocopine_deploy::EnvValue;
        let mut spec = fullstack_spec();
        spec.env
            .insert("LOG_LEVEL".into(), EnvValue::Literal("info".into()));
        let env = resolve_env(&spec).unwrap();
        assert_eq!(env.get("LOG_LEVEL").map(String::as_str), Some("info"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_env_skips_secret_source_now_that_secrets_api_is_wired() {
        // Phase 7: `from = "secret"` no longer fails resolve_env; the
        // value is pushed separately via `collect_secret_values` +
        // `MachinesClient::set_secrets`. resolve_env just omits it from
        // `MachineConfig.env` (which is Fly-plaintext).
        use pocopine_deploy::{EnvSource, EnvValue};
        let mut spec = fullstack_spec();
        spec.env.insert(
            "DATABASE_URL".into(),
            EnvValue::Indirect {
                from: EnvSource::Secret,
            },
        );
        let env = resolve_env(&spec).unwrap();
        assert!(!env.contains_key("DATABASE_URL"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn collect_secret_values_reads_from_env_for_secret_typed_keys() {
        use pocopine_deploy::{EnvSource, EnvValue};
        let mut spec = fullstack_spec();
        spec.env.insert(
            "POCOPINE_FLY_SECRET_TEST_KEY".into(),
            EnvValue::Indirect {
                from: EnvSource::Secret,
            },
        );
        // SAFETY: test-only env mutation; the key is unique to this test.
        std::env::set_var("POCOPINE_FLY_SECRET_TEST_KEY", "shhh");
        let secrets = collect_secret_values(&spec);
        std::env::remove_var("POCOPINE_FLY_SECRET_TEST_KEY");
        assert_eq!(
            secrets
                .get("POCOPINE_FLY_SECRET_TEST_KEY")
                .map(String::as_str),
            Some("shhh"),
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_env_reports_missing_env_keys() {
        use pocopine_deploy::{EnvSource, EnvValue};
        let mut spec = fullstack_spec();
        // Use a key that's vanishingly unlikely to exist in the test
        // process's env so this test stays stable.
        spec.env.insert(
            "POCOPINE_FLY_RESOLVE_ENV_TEST_KEY".into(),
            EnvValue::Indirect {
                from: EnvSource::Env,
            },
        );
        let err = resolve_env(&spec).unwrap_err().to_string();
        assert!(err.contains("from = \"env\""));
        assert!(err.contains("POCOPINE_FLY_RESOLVE_ENV_TEST_KEY"));
    }

    /// Helper: a fullstack spec with the required service URLs declared
    /// so the new env-provides checks don't bite tests that aren't
    /// about them.
    fn fullstack_spec_with_urls() -> DeploySpec {
        use pocopine_deploy::{EnvSource, EnvValue};
        let mut spec = fullstack_spec();
        spec.env.insert(
            "DATABASE_URL".into(),
            EnvValue::Indirect {
                from: EnvSource::Env,
            },
        );
        spec.env.insert(
            "REDIS_URL".into(),
            EnvValue::Indirect {
                from: EnvSource::Env,
            },
        );
        spec
    }

    #[test]
    fn refuses_when_database_url_missing() {
        let mut spec = fullstack_spec();
        // postgres required (uses_storage = true), but no DATABASE_URL.
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(s) if s.contains("DATABASE_URL"))));

        // Adding the URL clears the refusal.
        use pocopine_deploy::EnvValue;
        spec.env.insert(
            "DATABASE_URL".into(),
            EnvValue::Literal("postgres://…".into()),
        );
        spec.env
            .insert("REDIS_URL".into(), EnvValue::Literal("redis://…".into()));
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(!constraints
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(s) if s.contains("DATABASE_URL"))));
    }

    #[test]
    fn refuses_when_redis_url_missing() {
        let spec = fullstack_spec();
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(s) if s.contains("REDIS_URL"))));
    }

    #[test]
    fn accepts_custom_process_name_after_phase_6() {
        // Phase 6: process names no longer have a hard-coded allowlist.
        // The launcher dispatches via POCOPINE_PROC_<NAME> env vars
        // emitted by render_dockerfile.
        use pocopine_deploy::ProcessSpec;
        let mut spec = fullstack_spec_with_urls();
        spec.processes.insert(
            "scheduler".into(),
            ProcessSpec {
                bin: "scheduler".into(),
                port: None,
                healthcheck: None,
                scale: Default::default(),
                public: None,
            },
        );
        let constraints = FlyAdapter.detect_constraints(&spec);
        // No refusal for the custom process name.
        assert!(!constraints.iter().any(
            |c| matches!(c, Constraint::Refuse(s) if s.contains("scheduler") && s.contains("not yet supported"))
        ));
    }

    #[test]
    fn accepts_custom_bin_mapping_after_phase_6() {
        // Phase 6: bins are free-form; the Dockerfile cargo-builds
        // whatever bin names are declared.
        let mut spec = fullstack_spec_with_urls();
        spec.processes.get_mut("web").unwrap().bin = "api".into();
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(!constraints.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("`web` → `server`"),
        )));
    }

    #[test]
    fn refuses_empty_bin() {
        let mut spec = fullstack_spec_with_urls();
        spec.processes.get_mut("web").unwrap().bin = "".into();
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(s) if s.contains("empty `bin`"))));
    }

    #[test]
    fn refuses_process_name_with_uppercase_or_dot() {
        use pocopine_deploy::ProcessSpec;
        // `api.v2` is a valid quoted TOML key but invalid as a Fly
        // Machine name. Refuse before rendering breaks fly.toml.
        let mut spec = fullstack_spec_with_urls();
        spec.processes.insert(
            "api.v2".into(),
            ProcessSpec {
                bin: "api".into(),
                port: None,
                healthcheck: None,
                scale: Default::default(),
                public: None,
            },
        );
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("api.v2") && s.contains("valid Fly Machine name"),
        )));
    }

    #[test]
    fn refuses_process_env_var_collision() {
        use pocopine_deploy::ProcessSpec;
        // `api-worker` and `api_worker` both encode to
        // `POCOPINE_PROC_API_WORKER`; the launcher would clobber one.
        let mut spec = fullstack_spec_with_urls();
        let p = ProcessSpec {
            bin: "api".into(),
            port: None,
            healthcheck: None,
            scale: Default::default(),
            public: None,
        };
        spec.processes.insert("api-worker".into(), p.clone());
        spec.processes.insert("api_worker".into(), p);
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("collide") && s.contains("POCOPINE_PROC_API_WORKER"),
        )));
    }

    #[test]
    fn is_fly_safe_process_name_matches_regex() {
        assert!(is_fly_safe_process_name("web"));
        assert!(is_fly_safe_process_name("api-2"));
        assert!(is_fly_safe_process_name("worker_main"));
        assert!(is_fly_safe_process_name("1web"));
        assert!(!is_fly_safe_process_name(""));
        assert!(!is_fly_safe_process_name("Web")); // uppercase
        assert!(!is_fly_safe_process_name("api.v2")); // dot
        assert!(!is_fly_safe_process_name("api worker")); // space
        assert!(!is_fly_safe_process_name("-web")); // leading dash
    }

    #[test]
    fn accepts_volume_for_single_replica_process() {
        // Phase 9: volumes are wired. A volume declaration for an
        // existing process with scale.min = 1 is accepted.
        let mut spec = fullstack_spec_with_urls();
        spec.processes.get_mut("worker").unwrap().scale.min = 1;
        let overrides = toml::toml! {
            volumes = [{ process = "worker", path = "/data", size_gb = 1 }]
        };
        spec.host_overrides
            .insert("fly".into(), toml::Value::from(overrides));
        let constraints = FlyAdapter.detect_constraints(&spec);
        // No refusal mentioning volumes or not-yet-implemented.
        assert!(!constraints.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("volume") || s.contains("not yet implemented"),
        )));
    }

    #[test]
    fn refuses_volume_for_multireplica_process() {
        // Per-replica volumes are a Phase 9 follow-up.
        let mut spec = fullstack_spec_with_urls();
        spec.processes.get_mut("worker").unwrap().scale.min = 2;
        let overrides = toml::toml! {
            volumes = [{ process = "worker", path = "/data", size_gb = 1 }]
        };
        spec.host_overrides
            .insert("fly".into(), toml::Value::from(overrides));
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("scale.min = 2") && s.contains("mounts a volume"),
        )));
    }

    #[test]
    fn refuses_duplicate_volume_per_process() {
        // Two volumes for the same process would both encode to
        // `{process}_data` on the Fly side, silently collapsing.
        let mut spec = fullstack_spec_with_urls();
        spec.processes.get_mut("worker").unwrap().scale.min = 1;
        let overrides = toml::toml! {
            volumes = [
                { process = "worker", path = "/data",  size_gb = 1 },
                { process = "worker", path = "/cache", size_gb = 1 },
            ]
        };
        spec.host_overrides
            .insert("fly".into(), toml::Value::from(overrides));
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("more than one entry for process `worker`"),
        )));
    }

    #[test]
    fn refuses_volume_for_unknown_process() {
        let mut spec = fullstack_spec_with_urls();
        let overrides = toml::toml! {
            volumes = [{ process = "scheduler", path = "/data", size_gb = 1 }]
        };
        spec.host_overrides
            .insert("fly".into(), toml::Value::from(overrides));
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("references process `scheduler`") && s.contains("not declared"),
        )));
    }

    #[test]
    fn refuses_secret_source_with_no_value_in_env() {
        // Phase 7: secrets are wired through Fly's API; we only refuse
        // when the user's env doesn't have the value to push.
        use pocopine_deploy::{EnvSource, EnvValue};
        let mut spec = fullstack_spec_with_urls();
        let key = "POCOPINE_FLY_SECRET_REFUSE_TEST";
        spec.env.insert(
            key.into(),
            EnvValue::Indirect {
                from: EnvSource::Secret,
            },
        );
        std::env::remove_var(key); // belt-and-braces — must be unset
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(
            |c| matches!(c, Constraint::Refuse(s) if s.contains(key) && s.contains("fly secrets need values"))
        ));
    }

    #[test]
    fn accepts_secret_source_when_value_is_in_env() {
        use pocopine_deploy::{EnvSource, EnvValue};
        let mut spec = fullstack_spec_with_urls();
        let key = "POCOPINE_FLY_SECRET_ACCEPT_TEST";
        spec.env.insert(
            key.into(),
            EnvValue::Indirect {
                from: EnvSource::Secret,
            },
        );
        std::env::set_var(key, "shhh");
        let constraints = FlyAdapter.detect_constraints(&spec);
        std::env::remove_var(key);
        assert!(!constraints
            .iter()
            .any(|c| matches!(c, Constraint::Refuse(s) if s.contains(key))));
    }

    #[test]
    fn accepts_secret_backed_database_url() {
        // Regression for the Phase 7 rev-1 review: required-service URLs
        // declared as `{ from = "secret" }` were previously refused by
        // env_provides because it only counted literal / from=env.
        use pocopine_deploy::{EnvSource, EnvValue};
        // Start from `fullstack_spec` (no URLs) so the only DATABASE_URL
        // / REDIS_URL entries are the secret-typed ones we add.
        let mut spec = fullstack_spec();
        spec.env.insert(
            "DATABASE_URL".into(),
            EnvValue::Indirect {
                from: EnvSource::Secret,
            },
        );
        spec.env.insert(
            "REDIS_URL".into(),
            EnvValue::Indirect {
                from: EnvSource::Secret,
            },
        );
        std::env::set_var("DATABASE_URL", "postgres://test");
        std::env::set_var("REDIS_URL", "redis://test");
        let constraints = FlyAdapter.detect_constraints(&spec);
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("REDIS_URL");
        // No "no DATABASE_URL entry" / "no REDIS_URL entry" refusal.
        assert!(!constraints.iter().any(|c| matches!(
            c,
            Constraint::Refuse(s) if s.contains("no `DATABASE_URL`") || s.contains("no `REDIS_URL`"),
        )));
    }

    #[test]
    fn warns_when_scale_min_is_zero() {
        let mut spec = fullstack_spec_with_urls();
        spec.processes.get_mut("worker").unwrap().scale.min = 0;
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints
            .iter()
            .any(|c| matches!(c, Constraint::Warn(s) if s.contains("`worker` scale.min = 0"))));
    }

    #[test]
    fn refuses_excessive_scale_min() {
        let mut spec = fullstack_spec_with_urls();
        spec.processes.get_mut("web").unwrap().scale.min = 50;
        let constraints = FlyAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(
            |c| matches!(c, Constraint::Refuse(s) if s.contains("scale.min = 50") && s.contains("safety cap"))
        ));
    }
}
