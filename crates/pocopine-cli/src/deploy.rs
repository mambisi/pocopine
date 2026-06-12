//! `pocopine deploy` subcommand (RFC 080 §7).
//!
//! Wires `pocopine-deploy`'s `DeployAdapter` pipeline to the CLI:
//!
//!   1. read `[package.metadata.pocopine.deploy]` from the project's
//!      `Cargo.toml` (RFC 080 §4.1 — Pocopine.toml is the long-term home,
//!      but cargo metadata is what's available today)
//!   2. infer `app_name` from `[package].name` and `git_sha` from
//!      `git rev-parse HEAD`
//!   3. resolve the adapter (`railway`/`render` are built-in; new
//!      vendors live in their own crates)
//!   4. run the pipeline: `detect_constraints` → `render_config` →
//!      flush to disk → `build_artefact` → `deploy` → `post_deploy_hint`
//!
//! Non-`run` subcommands:
//!   * `pocopine deploy auth <host>` / `--list` / `--revoke <host>` —
//!     manage `~/.pocopine/credentials.toml`.
//!   * `pocopine deploy doctor` — check docker daemon + configured
//!     tokens for every known host.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use pocopine_deploy::{
    config, credentials, docker::DockerClient, spec, Constraint, DeployAdapter, Hint, StagedFiles,
};

use crate::args::{AuthArgs, ConfigArgs, ConfigCmd, DeployArgs, DeployCmd, StatusArgs};

pub fn run(args: &DeployArgs) -> Result<()> {
    match &args.cmd {
        None => run_deploy(args),
        Some(DeployCmd::Auth(a)) => run_auth(a),
        Some(DeployCmd::Doctor) => run_doctor(),
        Some(DeployCmd::Status(s)) => run_status(args, s),
        Some(DeployCmd::Config(c)) => run_config(args, c),
    }
}

fn run_deploy(args: &DeployArgs) -> Result<()> {
    if args.workspace {
        let entry = args
            .path
            .canonicalize()
            .with_context(|| format!("resolving project path {}", args.path.display()))?;
        let workspace_root = discover_workspace_root(&entry)?;
        let members = discover_deployable_members(&workspace_root)?;
        if members.is_empty() {
            bail!(
                "no deployable workspace members under `{}` (looked for `[package.metadata.pocopine.deploy]` in each crate's Cargo.toml)",
                workspace_root.display()
            );
        }
        eprintln!(
            "▶ workspace deploy: {} member(s) under `{}`",
            members.len(),
            workspace_root.display()
        );
        for member in members {
            let app = member
                .file_name()
                .map(|s: &std::ffi::OsStr| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| member.display().to_string());
            eprintln!("\n── {app} ─────────────────────────────────────────────");
            deploy_one_project(args, &member)
                .with_context(|| format!("deploying workspace member `{app}`"))?;
        }
        return Ok(());
    }

    let project = args
        .path
        .canonicalize()
        .with_context(|| format!("resolving project path {}", args.path.display()))?;
    deploy_one_project(args, &project)
}

fn deploy_one_project(args: &DeployArgs, project: &Path) -> Result<()> {
    let manifest = read_manifest(project)?;
    let app_name = app_name_from_manifest(&manifest)?;
    let deploy_table = deploy_table_from_manifest(&manifest)?;
    let git_sha = short_git_sha(project)?;

    // workspace_root → docker build context; subpath is "" for
    // standalone projects, e.g. "examples/keep" for workspace members.
    let workspace_root = discover_workspace_root(project)?;
    let workspace_subpath =
        normalize_workspace_subpath(project.strip_prefix(&workspace_root).ok())?;

    let target = args
        .target
        .as_deref()
        .context("--target required (e.g. `--target railway`)")?;

    let environment = if args.prod {
        Some("production".to_owned())
    } else {
        None
    };
    let mut spec = spec::parse(deploy_table.clone(), app_name, git_sha, environment)?;
    // RFC-100 Mode A — a public bucket/CDN base in
    // `[package.metadata.pocopine.assets] public-base` becomes the
    // server-side asset base. Explicit [deploy.env] declarations win.
    if let Some(public_base) = crate::config::load(project)?
        .assets
        .as_ref()
        .and_then(|a| a.public_base.clone())
    {
        spec.env
            .entry("POCOPINE_ASSET_BASE".to_owned())
            .or_insert(pocopine_deploy::EnvValue::Literal(public_base));
    }
    // first_deploy is per-(target, env) so prod and staging track separately.
    let state_basename = match spec.environment.as_deref() {
        Some(env) => format!("{target}-{env}.toml"),
        None => format!("{target}.toml"),
    };
    let state_file = project.join(".pocopine/deploy").join(&state_basename);
    spec.first_deploy = !state_file.exists();
    spec.skip_build = args.skip_build;
    spec.git_remote = discover_git_remote(project);
    spec.workspace_subpath = workspace_subpath.clone();
    spec.has_rust_toolchain = workspace_root.join("rust-toolchain.toml").exists();

    let adapter = resolve_adapter(target)?;

    // 1. Constraints.
    let constraints = adapter.detect_constraints(&spec);
    let mut refused = false;
    for c in &constraints {
        match c {
            Constraint::Refuse(msg) => {
                eprintln!("error: {msg}");
                refused = true;
            }
            Constraint::Warn(msg) => eprintln!("warn:  {msg}"),
            Constraint::Hint(msg) => eprintln!("hint:  {msg}"),
        }
    }
    if refused {
        bail!("aborting: deploy refused by adapter constraints");
    }

    // 2. Render config to a staging dir.
    let mut staged = StagedFiles::new();
    adapter.render_config(&spec, &mut staged);

    if args.dry_run {
        let build_root = project.join(spec.build_dir());
        if workspace_subpath.is_empty() {
            eprintln!(
                "--dry-run: would write {} file(s) under {}",
                staged.len(),
                build_root.display(),
            );
        } else {
            eprintln!(
                "--dry-run: workspace member detected — project `{}` at `{}/` \
                 (docker build context: workspace root). Would write {} file(s) under `{}`.",
                spec.app_name,
                workspace_subpath,
                staged.len(),
                build_root.display(),
            );
        }
        for (path, content) in staged.iter() {
            println!("\n=== {path} ===");
            println!("{content}");
        }
        if args.skip_build {
            eprintln!("--dry-run: --skip-build is set; would reuse the existing pushed image and run the host-API deploy only.");
        } else {
            eprintln!("--dry-run: would then bundle wasm + run `docker build` + `docker push` + host-API deploy.");
        }
        return Ok(());
    }

    // 3. Flush staged files under `<project>/<spec.build_dir()>`.
    //    Default is `.pocopine/build` (mirrors Rust's `target/`
    //    convention: one `.pocopine/` gitignore rule, `rm -rf
    //    .pocopine/build` clean). Override with `[deploy] build_dir =
    //    "..."` in Cargo.toml. `flush_one` refuses to clobber
    //    hand-edited files that lack GENERATED_MARKER (RFC 080 §7).
    let build_root = project.join(spec.build_dir());
    std::fs::create_dir_all(&build_root)
        .with_context(|| format!("creating {}", build_root.display()))?;
    for (path, content) in staged.iter() {
        let dest = build_root.join(path);
        flush_one(&dest, content)?;
    }

    // 4. Build the same artefacts as `pocopine build --release` so
    //    the Dockerfile's COPY picks up fresh wasm + bundles.
    //    Read config + build configured bins from `project`, not
    //    `args.path` — in workspace mode they're different (`args.path`
    //    is the workspace root or another member) and the wrong path
    //    would skip the member's Tailwind / configured-bin settings.
    if !args.skip_build {
        let cfg = crate::config::load(project)?;
        crate::build::wasm(project, true)?;
        crate::client_modules::build(project, true)?;
        crate::build::configured_bins(project, &cfg, true)?;
        if let Some(tw) = cfg.tailwind.as_ref() {
            crate::tailwind::run_once(project, tw, true)?;
        }
    }

    // 4.5 RFC-100 §7 — sync content-addressed assets BEFORE the app
    //     flip. New keys land beside the old ones (hashes never
    //     collide), so a half-finished sync can't break the running
    //     deploy; only after the sync succeeds does the new app — and
    //     with it the new URLs — go live. Runs in both normal and
    //     --skip-build deploys: CI may have built the image, but the
    //     bucket still has to be brought up to date.
    {
        let cfg = crate::config::load(project)?;
        if let Some(assets_cfg) = cfg.assets.as_ref() {
            eprintln!("▶ syncing assets to bucket `{}`", assets_cfg.bucket);
            crate::assets_sync::push(project, assets_cfg)
                .context("asset sync failed; aborting before the app flip")?;
        }
    }

    // 5. `docker build .` runs from the workspace root so workspace
    //    members' sibling crates (e.g. pocopine-launcher) are visible.
    let original_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(&workspace_root)
        .with_context(|| format!("cd {}", workspace_root.display()))?;
    let result = (|| -> Result<()> {
        let artefact = if args.skip_build {
            adapter.default_artefact(&spec)
        } else {
            adapter.build_artefact(&spec)?
        };
        let outcome = adapter.deploy(&spec, &artefact)?;

        for hint in adapter.post_deploy_hint(&spec, &outcome) {
            match hint {
                Hint::Info(s) => println!("{s}"),
                Hint::OneTime(s) => {
                    println!("\n--- one-time setup ---\n{s}");
                }
            }
        }

        // Record the deploy so subsequent runs see `first_deploy = false`.
        if let Some(parent) = state_file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&state_file, format!("app = {:?}\n", spec.app_name)).ok();

        Ok(())
    })();
    if let Some(cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }
    result
}

fn run_auth(args: &AuthArgs) -> Result<()> {
    if args.list {
        let entries = credentials::list()?;
        if entries.is_empty() {
            println!("No host tokens configured.");
            return Ok(());
        }
        for (host, source) in entries {
            let src = match source {
                credentials::Source::File => "file",
                credentials::Source::Env => "env",
                credentials::Source::EnvOverridesFile => "env (overrides file)",
            };
            println!("{host:<12} {src}");
        }
        return Ok(());
    }

    if let Some(host) = args.revoke.as_deref() {
        credentials::revoke(host)?;
        println!("Revoked token for `{host}`.");
        return Ok(());
    }

    let host = args
        .host
        .as_deref()
        .context("usage: `pocopine deploy auth <host>` | `--list` | `--revoke <host>`")?;
    let dashboard = dashboard_url_for(host);
    eprintln!("Paste the API token for `{host}`. Get one from:");
    eprintln!("    {dashboard}");
    eprintln!();
    eprint!("token: ");
    std::io::stderr().flush().ok();

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading token from stdin")?;
    let token = line.trim();
    if token.is_empty() {
        bail!("empty token");
    }

    credentials::store(host, token)?;
    eprintln!("Stored token for `{host}` to ~/.pocopine/credentials.toml (mode 0600).");
    Ok(())
}

fn run_config(parent: &DeployArgs, args: &ConfigArgs) -> Result<()> {
    match &args.cmd {
        ConfigCmd::Set { host, field, value } => {
            let value = match value.as_deref() {
                Some(v) => v.to_owned(),
                None => {
                    eprint!("value for `{host}.{field}`: ");
                    std::io::stderr().flush().ok();
                    let mut line = String::new();
                    std::io::stdin()
                        .read_line(&mut line)
                        .context("reading value from stdin")?;
                    line.trim().to_owned()
                }
            };
            // Reject whitespace-only too — these would pass an
            // `is_empty()` check but would be sent verbatim to host
            // APIs and produce confusing 4xx errors.
            if value.trim().is_empty() {
                bail!("empty value");
            }
            // Stash the original (pre-trim) value if it survives the
            // whitespace check, but strip leading/trailing whitespace
            // so a copy-paste with a trailing newline doesn't corrupt
            // the stored field. Inner whitespace is preserved.
            let value = value.trim().to_owned();
            config::store_field(host, field, &value)?;
            eprintln!("Stored `[default.{host}] {field} = {value:?}` to ~/.pocopine/config.toml.",);
            Ok(())
        }
        ConfigCmd::Get { host, field } => {
            // Resolve the project tier the same way `deploy()` will:
            // apply the `[deploy.production.<host>]` overlay when the
            // parent --prod flag is set. Without this the `get`
            // command would report `[deploy.<host>].<field>` for a
            // project whose --prod deploy actually resolves to the
            // production-overlay value.
            let project_path = parent.path.as_path();
            let project_value = read_project_field(project_path, host, field, parent.prod).ok();
            let resolved = config::resolve_with_source(host, field, project_value.as_deref());
            match resolved {
                Some((value, source)) => {
                    let src = match source {
                        config::Source::Env => {
                            format!("env (${})", config::env_var_name(host, field))
                        }
                        config::Source::Project => {
                            let suffix = if parent.prod {
                                format!(" [deploy.production.{host}].{field}")
                            } else {
                                format!(" [deploy.{host}].{field}")
                            };
                            format!("project ({}/Cargo.toml{suffix})", project_path.display(),)
                        }
                        config::Source::File => "file (~/.pocopine/config.toml)".to_owned(),
                    };
                    println!("{value}");
                    eprintln!("  source: {src}");
                }
                None => {
                    eprintln!(
                        "no value for `{host}.{field}` in env, project, or file. \
                         Set via `pocopine deploy config set {host} {field} <value>`, \
                         export ${}, or add to `[deploy.{host}]` in Cargo.toml.",
                        config::env_var_name(host, field),
                    );
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        ConfigCmd::List => {
            let entries = config::list()?;
            if entries.is_empty() {
                println!("No host config configured.");
                return Ok(());
            }
            // Compact table: HOST, FIELD, SOURCE — keeps the output
            // scannable for users with a handful of entries; bigger
            // setups can grep.
            for (host, field, source) in entries {
                let src = match source {
                    config::Source::Env => "env",
                    config::Source::File => "file",
                    config::Source::Project => "project",
                };
                println!("{host:<12} {field:<22} {src}");
            }
            Ok(())
        }
        ConfigCmd::Revoke { host, field } => {
            config::revoke_field(host, field)?;
            eprintln!("Revoked `[default.{host}] {field}`.");
            Ok(())
        }
    }
}

/// Read `[package.metadata.pocopine.deploy.<host>] <field>` from the
/// project's Cargo.toml — used by `config get` to surface the
/// project-tier value when reporting the resolved source. Best-effort;
/// any read/parse error is treated as "no project value".
fn read_project_field(project: &Path, host: &str, field: &str, prod: bool) -> Result<String> {
    let manifest = read_manifest(project)?;
    let deploy = manifest
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("pocopine"))
        .and_then(|p| p.get("deploy"))
        .context("not set in project Cargo.toml")?;

    // When `--prod` is set, the deploy path merges the
    // `[deploy.production.<host>]` overlay on top of the base
    // `[deploy.<host>]`. The `get` command must do the same so its
    // "source" report matches what an actual --prod deploy resolves.
    if prod {
        if let Some(v) = deploy
            .get("production")
            .and_then(|p| p.get(host))
            .and_then(|h| h.get(field))
            .and_then(|v| v.as_str())
        {
            return Ok(v.to_owned());
        }
    }
    deploy
        .get(host)
        .and_then(|h| h.get(field))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .context("not set in project Cargo.toml")
}

fn run_status(args: &DeployArgs, opts: &StatusArgs) -> Result<()> {
    use comfy_table::{presets::UTF8_FULL, Cell, Color, ContentArrangement, Table};

    let entry = args
        .path
        .canonicalize()
        .with_context(|| format!("resolving project path {}", args.path.display()))?;

    let projects: Vec<PathBuf> = if args.workspace {
        let workspace_root = discover_workspace_root(&entry)?;
        let members = discover_deployable_members(&workspace_root)?;
        if members.is_empty() {
            bail!(
                "no deployable workspace members under `{}` (looked for `[package.metadata.pocopine.deploy]` in each crate's Cargo.toml)",
                workspace_root.display()
            );
        }
        members
    } else {
        vec![entry]
    };

    // Per project, fan out across one or more targets. With `--target`
    // explicit, that target is used everywhere. Without it, each
    // project's configured hosts (`[deploy.<host>]` sub-tables that
    // resolve to a known adapter) are queried in turn — so the table
    // shows every platform the project is wired up to.
    let show_platform = args.target.is_none();
    let mut rows: Vec<StatusRow> = Vec::new();
    for project in &projects {
        let targets = targets_for_project(args, project)?;
        if targets.is_empty() {
            // Project has no host sub-table and no `--target` — surface
            // the row with an empty target so workspace status doesn't
            // silently skip it.
            rows.push(StatusRow {
                app: read_manifest(project)
                    .ok()
                    .and_then(|m| app_name_from_manifest(&m).ok())
                    .unwrap_or_else(|| project.display().to_string()),
                target: "(none)".to_owned(),
                processes: Vec::new(),
                error: Some("no [deploy.<host>] sub-table; pass --target explicitly".to_owned()),
            });
            continue;
        }
        for target in targets {
            let (app, processes, error) = match status_one_project(args, project, target) {
                Ok((a, p)) => (a, p, None),
                Err(e) => {
                    // Don't abort the whole command on per-platform
                    // failure — surface it as a row instead so the
                    // user sees which targets responded.
                    let app = read_manifest(project)
                        .ok()
                        .and_then(|m| app_name_from_manifest(&m).ok())
                        .unwrap_or_else(|| project.display().to_string());
                    (app, Vec::new(), Some(format!("{e:#}")))
                }
            };
            rows.push(StatusRow {
                app,
                target: target.to_owned(),
                processes,
                error,
            });
        }
    }

    if opts.json {
        #[derive(serde::Serialize)]
        struct AppStatus<'a> {
            app: &'a str,
            target: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            error: Option<&'a str>,
            processes: &'a [pocopine_deploy::ProcessStatus],
        }
        let payload: Vec<AppStatus<'_>> = rows
            .iter()
            .map(|r| AppStatus {
                app: &r.app,
                target: &r.target,
                error: r.error.as_deref(),
                processes: r.processes.as_slice(),
            })
            .collect();
        let s = serde_json::to_string_pretty(&payload).context("serialising status to JSON")?;
        println!("{s}");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);
    let mut header: Vec<&str> = Vec::new();
    if args.workspace {
        header.push("APP");
    }
    if show_platform {
        header.push("PLATFORM");
    }
    header.extend(["PROCESS", "STATE", "DEPLOY", "SERVICE", "URL"]);
    table.set_header(header);

    let mut had_any_rows = false;
    for row in &rows {
        if let Some(err) = &row.error {
            had_any_rows = true;
            let mut cells = Vec::new();
            if args.workspace {
                cells.push(Cell::new(&row.app));
            }
            if show_platform {
                cells.push(Cell::new(&row.target).fg(Color::Magenta));
            }
            cells.push(Cell::new("-"));
            cells.push(Cell::new(format!("error: {err}")).fg(Color::Red));
            cells.push(Cell::new("-"));
            cells.push(Cell::new("-"));
            cells.push(Cell::new("-"));
            table.add_row(cells);
            continue;
        }
        for s in &row.processes {
            had_any_rows = true;
            let state_label = format!("{:?}", s.state).to_lowercase();
            let state_cell = if !s.raw_state.is_empty() && s.raw_state != state_label {
                format!("{state_label} ({})", s.raw_state)
            } else {
                state_label.clone()
            };
            let state_color = match s.state {
                pocopine_deploy::DeployState::Live => Color::Green,
                pocopine_deploy::DeployState::Failed => Color::Red,
                pocopine_deploy::DeployState::Canceled => Color::Yellow,
                pocopine_deploy::DeployState::Building
                | pocopine_deploy::DeployState::Deploying
                | pocopine_deploy::DeployState::Pending => Color::Cyan,
                pocopine_deploy::DeployState::Unknown => Color::DarkGrey,
            };
            let mut cells = Vec::new();
            if args.workspace {
                cells.push(Cell::new(&row.app));
            }
            if show_platform {
                cells.push(Cell::new(&row.target).fg(Color::Magenta));
            }
            cells.push(Cell::new(&s.process));
            cells.push(Cell::new(state_cell).fg(state_color));
            cells.push(Cell::new(s.deploy_id.as_deref().unwrap_or("-")));
            cells.push(Cell::new(s.host_service_id.as_deref().unwrap_or("-")));
            cells.push(Cell::new(s.url.as_deref().unwrap_or("-")));
            table.add_row(cells);
        }
    }

    if !had_any_rows {
        println!("(no processes declared in any deployable member)");
        return Ok(());
    }
    println!("{table}");
    Ok(())
}

struct StatusRow {
    app: String,
    target: String,
    processes: Vec<pocopine_deploy::ProcessStatus>,
    error: Option<String>,
}

/// Pick which target(s) to query for a single project.
/// - `--target` set → use it verbatim, single entry.
/// - `--target` unset → enumerate `[deploy.<host>]` sub-tables that
///   resolve to a known adapter. Empty list means the project hasn't
///   declared a platform; the caller surfaces that to the user.
fn targets_for_project(args: &DeployArgs, project: &Path) -> Result<Vec<&'static str>> {
    if let Some(t) = args.target.as_deref() {
        // Validate so an unknown --target name fails once with a clear
        // error, not silently per-project.
        let _ = resolve_adapter(t)?;
        return Ok(KNOWN_TARGETS.iter().copied().filter(|k| *k == t).collect());
    }

    let manifest = match read_manifest(project) {
        Ok(m) => m,
        Err(_) => return Ok(Vec::new()),
    };
    let deploy_table = match deploy_table_from_manifest(&manifest) {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };
    // A project can declare a host either at the base
    // `[deploy.<host>]` sub-table or only under an environment
    // override block such as `[deploy.production.<host>]`. The deploy
    // path merges them via `spec::parse` — discovery has to look at
    // both, otherwise `--prod status` (without an explicit --target)
    // misses production-only host config and reports "no target".
    let env_table = if args.prod {
        deploy_table.get("production")
    } else {
        None
    };
    Ok(KNOWN_TARGETS
        .iter()
        .copied()
        .filter(|name| {
            deploy_table.get(*name).is_some() || env_table.and_then(|t| t.get(*name)).is_some()
        })
        .collect())
}

/// Adapter names supported by the built-in [`resolve_adapter`]. Source
/// of truth for `--target`-less status discovery.
const KNOWN_TARGETS: &[&str] = &["railway", "render"];

fn status_one_project(
    args: &DeployArgs,
    project: &Path,
    target: &str,
) -> Result<(String, Vec<pocopine_deploy::ProcessStatus>)> {
    let manifest = read_manifest(project)?;
    let app_name = app_name_from_manifest(&manifest)?;
    let deploy_table = deploy_table_from_manifest(&manifest)?;
    let git_sha = short_git_sha(project)?;

    let workspace_root = discover_workspace_root(project)?;
    let workspace_subpath =
        normalize_workspace_subpath(project.strip_prefix(&workspace_root).ok())?;

    let environment = if args.prod {
        Some("production".to_owned())
    } else {
        None
    };
    let mut spec = spec::parse(deploy_table.clone(), app_name.clone(), git_sha, environment)?;
    spec.git_remote = discover_git_remote(project);
    spec.workspace_subpath = workspace_subpath;
    spec.has_rust_toolchain = workspace_root.join("rust-toolchain.toml").exists();

    let adapter = resolve_adapter(target)?;
    let statuses = adapter.status(&spec)?;
    Ok((spec.app_name, statuses))
}

fn run_doctor() -> Result<()> {
    println!("pocopine deploy doctor");
    println!();

    print!("docker: ");
    let docker = DockerClient::new();
    match docker.check_available() {
        Ok(()) => println!("ok"),
        Err(e) => println!("unavailable\n  {e}"),
    }
    println!();

    println!("configured hosts:");
    for host in credentials::KNOWN_HOSTS {
        match credentials::load(host) {
            Ok(_) => println!("  {host:<10} ok"),
            Err(_) => {
                println!("  {host:<10} no token (run `pocopine deploy auth {host}` to configure)")
            }
        }
    }

    Ok(())
}

// ─── Helpers ───────────────────────────────────────────────────────────

/// Write `content` to `dest`, refusing to overwrite a hand-edited file.
/// "Hand-edited" = the existing file is present and does not contain the
/// `pocopine-deploy::common::GENERATED_MARKER`. Identical existing
/// content (e.g. a re-render produced the same bytes) is treated as a
/// no-op so the unmarked-but-canonical case doesn't keep refusing.
fn flush_one(dest: &Path, content: &str) -> Result<()> {
    use pocopine_deploy::common::GENERATED_MARKER;

    if dest.exists() {
        let existing = std::fs::read_to_string(dest)
            .with_context(|| format!("reading existing {}", dest.display()))?;
        let existing_is_ours = existing.contains(GENERATED_MARKER);
        if !existing_is_ours && existing.trim() != content.trim() {
            bail!(
                "refusing to overwrite hand-edited file `{}` — file does not carry the pocopine-generated marker.\n\
                 Move it aside (or delete it) to let pocopine regenerate, or commit the generated version next pass.",
                dest.display(),
            );
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(dest, content).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

fn resolve_adapter(target: &str) -> Result<Box<dyn DeployAdapter>> {
    match target {
        "railway" => Ok(Box::new(pocopine_deploy_railway::RailwayAdapter)),
        "render" => Ok(Box::new(pocopine_deploy_render::RenderAdapter)),
        other => bail!("unknown target `{other}`. Known: railway, render."),
    }
}

fn dashboard_url_for(host: &str) -> &'static str {
    match host {
        "railway" => "https://railway.com/account/tokens",
        "render" => "https://dashboard.render.com/u/settings#api-keys",
        _ => "<host dashboard>",
    }
}

fn read_manifest(project: &Path) -> Result<toml::Value> {
    let cargo_toml = project.join("Cargo.toml");
    let raw = std::fs::read_to_string(&cargo_toml)
        .with_context(|| format!("reading {}", cargo_toml.display()))?;
    let v: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parsing {}", cargo_toml.display()))?;
    Ok(v)
}

fn app_name_from_manifest(manifest: &toml::Value) -> Result<String> {
    manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_owned)
        .context("Cargo.toml missing [package].name (required to derive app name)")
}

fn deploy_table_from_manifest(manifest: &toml::Value) -> Result<toml::Value> {
    manifest
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("pocopine"))
        .and_then(|p| p.get("deploy"))
        .cloned()
        .context(
            "no [package.metadata.pocopine.deploy] in Cargo.toml — see RFC 080 §4.1 for the schema.",
        )
}

fn short_git_sha(project: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project)
        .output()
        .context("running `git rev-parse HEAD`")?;
    if !out.status.success() {
        bail!(
            "`git rev-parse HEAD` failed; is `{}` a git repo?",
            project.display()
        );
    }
    let sha = String::from_utf8(out.stdout)?.trim().to_owned();
    if sha.is_empty() {
        bail!("`git rev-parse HEAD` returned an empty SHA");
    }
    Ok(sha.chars().take(7).collect())
}

/// Normalize a subpath for Dockerfile COPY: backslash → `/`, trim
/// trailing slash, refuse whitespace/control chars (which would break
/// the unquoted COPY tokenization).
fn normalize_workspace_subpath(rel: Option<&Path>) -> Result<String> {
    let Some(rel) = rel else {
        return Ok(String::new());
    };
    let normalized = rel.to_string_lossy().replace('\\', "/");
    if normalized.is_empty() {
        return Ok(String::new());
    }
    if normalized
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        bail!(
            "workspace subpath `{normalized}` contains whitespace/control characters that would break Dockerfile COPY. \
             Rename the project directory to a path without spaces.",
        );
    }
    Ok(normalized.trim_end_matches('/').to_owned())
}

/// Walk up from `project` for a `Cargo.toml` containing `[workspace]`.
/// The innermost wins; standalone projects (no `[workspace]` anywhere)
/// return `project` itself. Read/parse errors propagate.
fn discover_workspace_root(project: &Path) -> Result<PathBuf> {
    let mut cursor: Option<&Path> = Some(project);
    while let Some(dir) = cursor {
        let cargo_toml = dir.join("Cargo.toml");
        match std::fs::read_to_string(&cargo_toml) {
            Ok(raw) => {
                let v: toml::Value = raw
                    .parse()
                    .with_context(|| format!("parsing `{}`", cargo_toml.display()))?;
                if v.get("workspace").is_some() {
                    return Ok(dir.to_path_buf());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading `{}`", cargo_toml.display())),
        }
        cursor = dir.parent();
    }
    Ok(project.to_path_buf())
}

/// Workspace members with a `[package.metadata.pocopine.deploy]` table.
/// Reads `[workspace].members` from the workspace `Cargo.toml`, expands
/// trailing-`*` directory globs (e.g. `examples/*`), and filters to
/// crates that actually declare a deploy block. Sorted by path so the
/// per-deploy output order is stable.
fn discover_deployable_members(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let cargo_toml = workspace_root.join("Cargo.toml");
    let raw = std::fs::read_to_string(&cargo_toml)
        .with_context(|| format!("reading `{}`", cargo_toml.display()))?;
    let v: toml::Value = raw
        .parse()
        .with_context(|| format!("parsing `{}`", cargo_toml.display()))?;
    let members = v
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .with_context(|| {
            format!(
                "workspace Cargo.toml at `{}` has no [workspace].members array",
                cargo_toml.display()
            )
        })?;

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in members {
        let pattern = entry
            .as_str()
            .context("workspace member entry is not a string")?;
        for path in expand_workspace_member(workspace_root, pattern) {
            if has_deploy_metadata(&path)? {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// Expand a `[workspace].members` entry to a list of crate directories.
/// Supports literal paths and trailing-`*` directory globs (`crates/*`,
/// `examples/*`). Unsupported patterns return an empty list.
fn expand_workspace_member(workspace_root: &Path, pattern: &str) -> Vec<PathBuf> {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        let dir = workspace_root.join(prefix);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join("Cargo.toml").exists() {
                out.push(p);
            }
        }
        out
    } else if pattern.contains('*') {
        // Other glob shapes (`**`, `**/*`) — not handled; users can
        // enumerate explicit members. Empty list keeps discovery
        // safe rather than crashing.
        Vec::new()
    } else {
        vec![workspace_root.join(pattern)]
    }
}

/// `true` when the project's Cargo.toml carries a
/// `[package.metadata.pocopine.deploy]` table. Missing files / parse
/// failures yield `false` so workspaces with unrelated members don't
/// break workspace discovery.
fn has_deploy_metadata(project: &Path) -> Result<bool> {
    let cargo_toml = project.join("Cargo.toml");
    let raw = match std::fs::read_to_string(&cargo_toml) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("reading `{}`", cargo_toml.display())),
    };
    let Ok(v) = raw.parse::<toml::Value>() else {
        return Ok(false);
    };
    Ok(v.get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("pocopine"))
        .and_then(|p| p.get("deploy"))
        .is_some())
}

/// Best-effort `git remote get-url origin`. Returns `None` when there is
/// no `origin` remote — adapters then fall back to requiring an explicit
/// `image_registry`.
fn discover_git_remote(project: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(project)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if url.is_empty() {
        None
    } else {
        Some(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_from_manifest_reads_package_name() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [package]
            name = "myapp"
            version = "0.1.0"
        "#,
        )
        .unwrap();
        assert_eq!(app_name_from_manifest(&manifest).unwrap(), "myapp");
    }

    #[test]
    fn app_name_from_manifest_errors_without_package() {
        let manifest: toml::Value = toml::from_str(r#"[workspace]"#).unwrap();
        assert!(app_name_from_manifest(&manifest).is_err());
    }

    #[test]
    fn deploy_table_reads_nested_metadata() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [package]
            name = "myapp"

            [package.metadata.pocopine.deploy]
            mode = "fullstack"
        "#,
        )
        .unwrap();
        let table = deploy_table_from_manifest(&manifest).unwrap();
        assert_eq!(
            table.get("mode").and_then(|v| v.as_str()),
            Some("fullstack")
        );
    }

    #[test]
    fn deploy_table_errors_when_missing() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [package]
            name = "myapp"
        "#,
        )
        .unwrap();
        let err = deploy_table_from_manifest(&manifest)
            .unwrap_err()
            .to_string();
        assert!(err.contains("[package.metadata.pocopine.deploy]"));
    }

    #[test]
    fn dashboard_url_for_known_hosts() {
        assert!(dashboard_url_for("railway").contains("railway"));
        assert!(dashboard_url_for("render").contains("render"));
        assert_eq!(dashboard_url_for("nope"), "<host dashboard>");
    }

    #[test]
    fn resolve_adapter_known_target() {
        assert!(resolve_adapter("railway").is_ok());
        assert!(resolve_adapter("render").is_ok());
        assert!(resolve_adapter("nope").is_err());
    }

    #[test]
    fn flush_one_writes_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("Dockerfile");
        flush_one(
            &dest,
            "# Generated by pocopine-deploy (test)\nFROM scratch\n",
        )
        .unwrap();
        assert!(dest.exists());
    }

    #[test]
    fn flush_one_refuses_to_clobber_unmarked_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("Dockerfile");
        std::fs::write(&dest, "FROM ubuntu\nRUN do-not-clobber-me\n").unwrap();
        let err = flush_one(
            &dest,
            "# Generated by pocopine-deploy (test)\nFROM scratch\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("refusing to overwrite hand-edited"));
    }

    #[test]
    fn flush_one_allows_overwrite_of_marker_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("Dockerfile");
        std::fs::write(&dest, "# Generated by pocopine-deploy (old)\nold\n").unwrap();
        flush_one(&dest, "# Generated by pocopine-deploy (new)\nnew\n").unwrap();
        let after = std::fs::read_to_string(&dest).unwrap();
        assert!(after.contains("new"));
    }

    #[test]
    fn normalize_workspace_subpath_handles_unix_paths() {
        let got = normalize_workspace_subpath(Some(Path::new("examples/keep"))).unwrap();
        assert_eq!(got, "examples/keep");
    }

    #[test]
    fn normalize_workspace_subpath_converts_backslashes() {
        let got = normalize_workspace_subpath(Some(Path::new("examples\\keep"))).unwrap();
        assert_eq!(got, "examples/keep");
    }

    #[test]
    fn normalize_workspace_subpath_refuses_whitespace() {
        let err = normalize_workspace_subpath(Some(Path::new("my project/keep")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("whitespace"));
    }

    #[test]
    fn normalize_workspace_subpath_trims_trailing_slash() {
        let got = normalize_workspace_subpath(Some(Path::new("examples/keep/"))).unwrap();
        assert_eq!(got, "examples/keep");
    }

    #[test]
    fn normalize_workspace_subpath_empty_when_no_rel() {
        assert_eq!(normalize_workspace_subpath(None).unwrap(), "");
    }

    #[test]
    fn discover_workspace_root_finds_workspace_ancestor() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"examples/keep\"]\n",
        )
        .unwrap();
        let member = root.path().join("examples/keep");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"keep\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let found = discover_workspace_root(&member).unwrap();
        // canonicalize: macOS tempdir resolves through `/private/`.
        assert_eq!(
            found.canonicalize().unwrap(),
            root.path().canonicalize().unwrap(),
        );
    }

    #[test]
    fn discover_workspace_root_returns_project_when_it_is_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        let found = discover_workspace_root(dir.path()).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap(),
        );
    }

    #[test]
    fn discover_workspace_root_picks_innermost_for_nested_workspaces() {
        let outer = tempfile::tempdir().unwrap();
        std::fs::write(
            outer.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"inner\"]\n",
        )
        .unwrap();
        let inner = outer.path().join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(
            inner.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\n",
        )
        .unwrap();
        let app = inner.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let found = discover_workspace_root(&app).unwrap();
        assert_eq!(found.canonicalize().unwrap(), inner.canonicalize().unwrap());
    }

    #[test]
    fn discover_workspace_root_surfaces_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "this = is not valid toml [[").unwrap();
        let err = discover_workspace_root(dir.path()).unwrap_err().to_string();
        assert!(err.contains("parsing"));
    }

    #[test]
    fn discover_workspace_root_collapses_for_standalone_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("standalone");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"standalone\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let found = discover_workspace_root(&project).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            project.canonicalize().unwrap(),
        );
    }

    #[test]
    fn flush_one_noop_when_existing_matches_unmarked_content() {
        // Re-renders that happen to produce the same bytes shouldn't
        // start refusing just because the canonical content has no
        // marker (e.g. an external user-checked-in copy).
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(".dockerignore");
        let body = "target/\n";
        std::fs::write(&dest, body).unwrap();
        flush_one(&dest, body).unwrap(); // identical → no-op write, no error
    }
}
