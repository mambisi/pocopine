use std::io::IsTerminal;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::args::DoctorArgs;
use crate::{client_modules, config, server, tools};

pub fn run(args: &DoctorArgs) -> Result<()> {
    let mut report = Report::default();

    let project = match args.path.canonicalize() {
        Ok(project) => {
            report.ok("project path", project.display().to_string());
            Some(project)
        }
        Err(err) => {
            report.fail(
                "project path",
                format!("{} ({err})", args.path.display()),
                "run from a Pocopine project or pass `--path <project>`",
            );
            None
        }
    };

    let project_tools = match project.as_deref() {
        Some(project) => match tools::ProjectTools::load(project) {
            Ok(project_tools) => {
                if project_tools.loaded() {
                    report.ok(
                        ".pocopine.toml",
                        format!("loaded {}", project_tools.config_path().display()),
                    );
                } else {
                    report.ok(".pocopine.toml", "using PATH defaults");
                }
                project_tools
            }
            Err(err) => {
                report.fail(
                    ".pocopine.toml",
                    err.to_string(),
                    "fix the local tool config or remove it to use PATH defaults",
                );
                tools::ProjectTools::empty(project)
            }
        },
        None => tools::ProjectTools::empty(Path::new(".")),
    };

    check_command(
        &mut report,
        "cargo",
        project_tools.cargo(),
        &["--version"],
        "install Rust from https://rustup.rs/ or set `[tools].cargo` in .pocopine.toml",
    );
    check_command(
        &mut report,
        "rustc",
        project_tools.rustc(),
        &["--version"],
        "install Rust from https://rustup.rs/ or set `[tools].rustc` in .pocopine.toml",
    );
    check_command(
        &mut report,
        "wasm-pack",
        project_tools.wasm_pack(),
        &["--version"],
        "install wasm-pack or set `[tools].wasm-pack` in .pocopine.toml",
    );

    check_editor_extension(&mut report);

    let cfg = project
        .as_deref()
        .and_then(|project| check_project_config(&mut report, project));

    if let Some(project) = project.as_deref() {
        check_client_modules(&mut report, project, &project_tools);
        if let Some(cfg) = cfg.as_ref() {
            check_tailwind(&mut report, project, cfg, &project_tools);
            check_configured_bins(&mut report, project, cfg, &project_tools);
        }
    }

    report.print();

    let failures = report.failures();
    let warnings = report.warnings();
    if failures > 0 {
        bail!("doctor found {failures} failure(s)");
    }
    if args.strict && warnings > 0 {
        bail!("doctor found {warnings} warning(s) in --strict mode");
    }
    Ok(())
}

fn check_project_config(report: &mut Report, project: &Path) -> Option<config::PocopineConfig> {
    let manifest = project.join("Cargo.toml");
    if !manifest.is_file() {
        report.fail(
            "project config",
            format!("missing {}", manifest.display()),
            "pass `--path` to the crate or workspace you want to check",
        );
        return None;
    }

    match config::load(project) {
        Ok(cfg) => {
            report.ok("project config", "Cargo.toml parsed");
            Some(cfg)
        }
        Err(err) => {
            report.fail(
                "project config",
                err.to_string(),
                "fix `[package.metadata.pocopine]` in Cargo.toml",
            );
            None
        }
    }
}

fn check_client_modules(report: &mut Report, project: &Path, project_tools: &tools::ProjectTools) {
    match client_modules::status(project) {
        Ok(status) => {
            if status.module_count == 0 {
                report.ok("client modules", "not enabled");
                if status.package_json {
                    report.ok("package.json", "present (no Pocopine client modules)");
                }
                return;
            } else {
                report.ok(
                    "client modules",
                    format!("{} .client module(s) found", status.module_count),
                );
            }

            report.ok(
                "pm selection",
                format!(
                    "{} ({})",
                    status.package_manager_command.display(),
                    status.package_manager_source.describe()
                ),
            );
            for conflict in &status.package_manager_conflicts {
                report.warn(
                    "pm lockfile",
                    format!("also found {conflict}"),
                    "keep one JS lockfile at the project root so installs are deterministic",
                );
            }
            if status.package_manager_overridden
                && matches!(
                    status.package_manager_source,
                    client_modules::PackageManagerSource::Lockfile(_)
                )
                && status.package_manager_command.program_name() != status.package_manager.binary()
            {
                report.warn(
                    "pm override",
                    format!(
                        "{} overrides {} ({})",
                        status.package_manager_command.display(),
                        status.package_manager.label(),
                        status.package_manager_source.describe()
                    ),
                    "make sure `.pocopine.toml` matches the lockfile you want Pocopine to honor",
                );
            }

            let manager_is_bun = status
                .package_manager_command
                .program_name()
                .eq_ignore_ascii_case("bun");
            check_command(
                report,
                "package manager",
                status.package_manager_command,
                &["--version"],
                "install the detected package manager or add the lockfile for the manager you use",
            );
            if !manager_is_bun {
                check_command(
                    report,
                    "node",
                    project_tools.node(),
                    &["--version"],
                    "install Node.js or set `[tools].node` in .pocopine.toml",
                );
            }

            if status.package_json {
                report.ok("package.json", "present");
            } else {
                report.warn(
                    "package.json",
                    "missing",
                    "run `pocopine js init` before adding client-module dependencies",
                );
            }

            if status.has_esbuild_dependency {
                report.ok("esbuild config", "dependency declared");
            } else {
                report.warn(
                    "esbuild config",
                    "dependency not declared",
                    "run `pocopine js init` to add the managed esbuild dependency",
                );
            }

            if status.has_typescript_dependency {
                report.ok("typescript config", "dependency declared");
            } else {
                report.warn(
                    "typescript config",
                    "dependency not declared",
                    "run `pocopine js init` to add the managed TypeScript dependency",
                );
            }

            if status.node_modules && status.local_esbuild {
                report.ok("esbuild binary", "installed in node_modules");
            } else {
                report.warn(
                    "esbuild binary",
                    "not installed locally",
                    "run `pocopine js install` before the first client-module build",
                );
            }

            if status.node_modules && status.local_typescript {
                report.ok("typescript binary", "installed in node_modules");
            } else {
                report.warn(
                    "typescript binary",
                    "not installed locally",
                    "run `pocopine js install` before generating typed client-module bindings",
                );
            }
        }
        Err(err) => report.fail(
            "client modules",
            err.to_string(),
            "fix unsupported `.client` files or package.json before running build/dev",
        ),
    }
}

fn check_tailwind(
    report: &mut Report,
    project: &Path,
    cfg: &config::PocopineConfig,
    project_tools: &tools::ProjectTools,
) {
    let Some(tw) = cfg.tailwind.as_ref() else {
        report.ok("tailwind", "not enabled");
        return;
    };

    let input = project.join(&tw.input);
    if input.is_file() {
        report.ok("tailwind input", input.display().to_string());
    } else {
        report.fail(
            "tailwind input",
            format!("missing {}", input.display()),
            "create the configured input CSS file or update `pocopine.tailwind.input`",
        );
    }

    if let Some(explicit) = tw.binary.as_ref() {
        let resolved = if explicit.is_absolute() {
            explicit.clone()
        } else {
            project.join(explicit)
        };
        if resolved.is_file() {
            report.ok("tailwind binary", resolved.display().to_string());
        } else {
            report.fail(
                "tailwind binary",
                format!("missing {}", resolved.display()),
                "fix `pocopine.tailwind.binary` or remove it to let Pocopine resolve Tailwind",
            );
        }
        return;
    }

    if let Some(tool) = project_tools.tailwindcss() {
        check_command(
            report,
            "tailwind binary",
            tool,
            &["--help"],
            "fix `[tools].tailwindcss` in .pocopine.toml or remove it to use PATH/download defaults",
        );
        return;
    }

    if let Some(path) = tools::which("tailwindcss") {
        report.ok("tailwind binary", path.display().to_string());
    } else {
        report.warn(
            "tailwind binary",
            "tailwindcss not found on PATH",
            "Pocopine will try to download the standalone Tailwind binary on first build",
        );
    }
}

fn check_configured_bins(
    report: &mut Report,
    project: &Path,
    cfg: &config::PocopineConfig,
    project_tools: &tools::ProjectTools,
) {
    if cfg.bin.is_none() && cfg.worker_bin.is_none() {
        report.ok("server bins", "static serving mode");
        return;
    }

    let metadata_bins = match cargo_metadata_bins(project, project_tools) {
        Ok(bins) => Some(bins),
        Err(err) => {
            report.warn(
                "cargo metadata",
                err.to_string(),
                "fix cargo metadata before relying on configured server bins",
            );
            None
        }
    };

    if let Some(bin) = cfg.bin.as_deref() {
        check_bin_target(report, "server bin", bin, metadata_bins.as_deref());
    }
    if let Some(bin) = cfg.worker_bin.as_deref() {
        check_bin_target(report, "worker bin", bin, metadata_bins.as_deref());
        check_worker_backend(report);
    }
}

fn check_bin_target(
    report: &mut Report,
    label: &'static str,
    bin: &str,
    metadata_bins: Option<&[String]>,
) {
    let Some(metadata_bins) = metadata_bins else {
        return;
    };
    if metadata_bins.iter().any(|name| name == bin) {
        report.ok(label, format!("`{bin}` target found"));
    } else {
        report.fail(
            label,
            format!("`{bin}` target not found"),
            "make sure `[package.metadata.pocopine]` names an existing Cargo bin target",
        );
    }
}

fn check_worker_backend(report: &mut Report) {
    if let Err(err) = server::validate_worker_backend_for_separate_process(true) {
        report.fail(
            "worker backend",
            err.to_string(),
            "use Redis for a separate worker process or embed the worker in the server",
        );
        return;
    }

    if let Err(err) = server::validate_worker_backend_for_separate_process(false) {
        report.warn(
            "worker backend",
            err.to_string(),
            "`pocopine dev` supplies redis://127.0.0.1/ by default; set POCOPINE_REDIS_URL for `pocopine run`",
        );
        return;
    }

    report.ok(
        "worker backend",
        "environment supports a separate worker process",
    );
}

/// Recommend the "Poco LSP" editor extension. Always informational (never a
/// warning/failure, so it never trips `--strict`): if the VS Code CLI is on
/// PATH we report whether the extension is installed and otherwise print the
/// one-line install command; if there's no `code` CLI we just point at the
/// marketplaces.
fn check_editor_extension(report: &mut Report) {
    const EXT_ID: &str = "pocopine.vscode-poco";
    match tools::which("code") {
        Some(code) => {
            let installed = std::process::Command::new(&code)
                .arg("--list-extensions")
                .output()
                .ok()
                .filter(|out| out.status.success())
                .is_some_and(|out| {
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .any(|line| line.trim().eq_ignore_ascii_case(EXT_ID))
                });
            if installed {
                report.ok(
                    "editor extension",
                    "Poco LSP (pocopine.vscode-poco) installed",
                );
            } else {
                report.ok(
                    "editor extension",
                    "install Poco LSP — `code --install-extension pocopine.vscode-poco`",
                );
            }
        }
        None => report.ok(
            "editor extension",
            "Poco LSP — VS Code Marketplace / Open VSX (search \"Poco LSP\")",
        ),
    }
}

fn check_command(
    report: &mut Report,
    label: &'static str,
    tool: tools::ToolCommand,
    args: &[&str],
    hint: &'static str,
) {
    let Some(path) = tools::resolve_program(&tool) else {
        report.fail(label, format!("{} not found", tool.display()), hint);
        return;
    };

    let mut cmd = tool.command();
    match cmd.args(args).output() {
        Ok(output) if output.status.success() => {
            let version = first_line(&output.stdout)
                .or_else(|| first_line(&output.stderr))
                .unwrap_or_else(|| path.display().to_string());
            report.ok(label, version);
        }
        Ok(output) => {
            let detail = first_line(&output.stderr)
                .or_else(|| first_line(&output.stdout))
                .unwrap_or_else(|| format!("{} exited with {}", tool.display(), output.status));
            report.fail(label, detail, hint);
        }
        Err(err) => report.fail(
            label,
            format!("could not run {}: {err}", tool.display()),
            hint,
        ),
    }
}

fn cargo_metadata_bins(project: &Path, project_tools: &tools::ProjectTools) -> Result<Vec<String>> {
    let output = project_tools
        .cargo()
        .command()
        .arg("metadata")
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1")
        .current_dir(project)
        .output()
        .context("invoke cargo metadata")?;
    if !output.status.success() {
        let detail = first_line(&output.stderr)
            .or_else(|| first_line(&output.stdout))
            .unwrap_or_else(|| format!("cargo metadata exited with {}", output.status));
        bail!("{detail}");
    }

    let metadata: Value =
        serde_json::from_slice(&output.stdout).context("parse cargo metadata JSON")?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("cargo metadata did not include packages"))?;

    let mut bins = Vec::new();
    for package in packages {
        let Some(targets) = package.get("targets").and_then(Value::as_array) else {
            continue;
        };
        for target in targets {
            let is_bin = target
                .get("kind")
                .and_then(Value::as_array)
                .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")));
            if is_bin
                && let Some(name) = target.get("name").and_then(Value::as_str) {
                    bins.push(name.to_string());
                }
        }
    }
    bins.sort();
    bins.dedup();
    Ok(bins)
}

fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Default)]
struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn ok(&mut self, name: &'static str, detail: impl Into<String>) {
        self.checks.push(Check {
            level: Level::Ok,
            name,
            detail: detail.into(),
            hint: None,
        });
    }

    fn warn(&mut self, name: &'static str, detail: impl Into<String>, hint: &'static str) {
        self.checks.push(Check {
            level: Level::Warn,
            name,
            detail: detail.into(),
            hint: Some(hint),
        });
    }

    fn fail(&mut self, name: &'static str, detail: impl Into<String>, hint: &'static str) {
        self.checks.push(Check {
            level: Level::Fail,
            name,
            detail: detail.into(),
            hint: Some(hint),
        });
    }

    fn failures(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.level == Level::Fail)
            .count()
    }

    fn warnings(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.level == Level::Warn)
            .count()
    }

    fn print(&self) {
        let color = Color::auto();
        println!("{}", color.bold("pocopine doctor"));
        for check in &self.checks {
            let level = format!("{:<7}", check.level.label());
            println!(
                "{} {:<18} {}",
                check.level.paint(&color, level),
                check.name,
                check.detail
            );
            if let Some(hint) = check.hint {
                println!("        {} {hint}", color.dim("hint:"));
            }
        }

        let failures = self.failures();
        let warnings = self.warnings();
        let ok = self.checks.len().saturating_sub(failures + warnings);
        println!();
        println!(
            "{} passed, {} warning(s), {} failure(s)",
            color.green(ok),
            color.yellow(warnings),
            color.red(failures),
        );
    }
}

struct Check {
    level: Level,
    name: &'static str,
    detail: String,
    hint: Option<&'static str>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Ok => "[ok]",
            Level::Warn => "[warn]",
            Level::Fail => "[fail]",
        }
    }

    fn paint(self, color: &Color, value: impl std::fmt::Display) -> String {
        match self {
            Level::Ok => color.green(value),
            Level::Warn => color.yellow(value),
            Level::Fail => color.red(value),
        }
    }
}

struct Color {
    enabled: bool,
}

impl Color {
    fn auto() -> Self {
        let forced = std::env::var("CLICOLOR_FORCE").is_ok_and(|value| value != "0");
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let clicolor_disabled = std::env::var("CLICOLOR").is_ok_and(|value| value == "0");
        Self {
            enabled: !no_color
                && (forced || (!clicolor_disabled && std::io::stdout().is_terminal())),
        }
    }

    fn green(&self, value: impl std::fmt::Display) -> String {
        self.paint("32", value)
    }

    fn yellow(&self, value: impl std::fmt::Display) -> String {
        self.paint("33", value)
    }

    fn red(&self, value: impl std::fmt::Display) -> String {
        self.paint("31", value)
    }

    fn bold(&self, value: impl std::fmt::Display) -> String {
        self.paint("1", value)
    }

    fn dim(&self, value: impl std::fmt::Display) -> String {
        self.paint("2", value)
    }

    fn paint(&self, code: &str, value: impl std::fmt::Display) -> String {
        if self.enabled {
            format!("\x1b[{code}m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }
}
