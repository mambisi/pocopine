//! Binary execution mode (RFC-121): the loader as a standalone CLI.
//!
//! Four subcommands over the same library the runtime embeds:
//! `validate` judges skill directories/roots (exit 1 on spec violations),
//! `list` reports the catalog, `inspect` dumps one skill in full, and
//! `index` renders the exact level-1 prompt block the runtime would inject.
//!
//! `--json` output is a versioned envelope (`"schema": "agenkitty-skills/v1"`)
//! that only evolves additively within v1. No default roots and no config
//! resolution here: the binary is a pure function over what it is pointed at.

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::{Value as JsonValue, json};

use crate::catalog::LoadedSkill;
use crate::discover::{SkillLoader, load_skill_dir};
use crate::meta::{Severity, SkillDiagnostic, SkillLimits};

pub const JSON_SCHEMA_TAG: &str = "agenkitty-skills/v1";

#[derive(Parser)]
#[command(
    name = "agenkitty-skills",
    version,
    about = "Agent Skills (agentskills.io) loader for the Agenkitty framework"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate skill directories (and/or whole roots) against the spec.
    Validate {
        /// Skill directories to validate (each must contain SKILL.md).
        dirs: Vec<PathBuf>,
        /// Skills roots to validate wholesale (repeatable, ordered).
        #[arg(long)]
        root: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// List the catalog: loaded, shadowed, and excluded skills.
    List {
        /// Skills roots (repeatable, ordered; earlier roots win collisions).
        #[arg(long, required = true)]
        root: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Dump one skill in full: parsed fields, extensions, raw frontmatter.
    Inspect {
        name: String,
        #[arg(long, required = true)]
        root: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Render the exact level-1 index block the runtime would inject.
    Index {
        #[arg(long, required = true)]
        root: Vec<PathBuf>,
        /// Byte budget for the rendered index.
        #[arg(long)]
        budget: Option<usize>,
    },
}

/// Parse argv, execute, and return the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    execute(cli, &mut out)
}

/// Testable entry point: everything `run` does, minus argv and process exit.
pub fn execute(cli: Cli, out: &mut impl Write) -> i32 {
    match cli.command {
        Command::Validate { dirs, root, json } => validate(dirs, root, json, out),
        Command::List { root, json } => list(root, json, out),
        Command::Inspect { name, root, json } => inspect(&name, root, json, out),
        Command::Index { root, budget } => index(root, budget, out),
    }
}

fn validate(dirs: Vec<PathBuf>, roots: Vec<PathBuf>, json: bool, out: &mut impl Write) -> i32 {
    if dirs.is_empty() && roots.is_empty() {
        let _ = writeln!(out, "validate needs at least one skill directory or --root");
        return 2;
    }
    for dir in &dirs {
        if !dir.is_dir() {
            let _ = writeln!(out, "`{}` is not a directory", dir.display());
            return 2;
        }
    }

    let mut diagnostics: Vec<SkillDiagnostic> = Vec::new();
    let mut checked = 0usize;
    for dir in &dirs {
        let catalog = load_skill_dir(dir, SkillLimits::default());
        checked += catalog.len() + usize::from(catalog.is_empty());
        diagnostics.extend(catalog.diagnostics().iter().cloned());
    }
    if !roots.is_empty() {
        let catalog = SkillLoader::new(roots).discover();
        checked += catalog.len();
        diagnostics.extend(catalog.diagnostics().iter().cloned());
    }

    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .count();

    if json {
        let envelope = json!({
            "schema": JSON_SCHEMA_TAG,
            "command": "validate",
            "checked": checked,
            "errors": errors,
            "diagnostics": diagnostics,
        });
        let _ = writeln!(out, "{}", pretty(&envelope));
    } else {
        for diagnostic in &diagnostics {
            let _ = writeln!(out, "{}", render_diagnostic(diagnostic));
        }
        let _ = writeln!(
            out,
            "{checked} skill{} checked, {errors} error{}",
            plural(checked),
            plural(errors)
        );
    }
    if errors > 0 { 1 } else { 0 }
}

fn list(roots: Vec<PathBuf>, json: bool, out: &mut impl Write) -> i32 {
    let catalog = SkillLoader::new(roots).discover();
    if json {
        let envelope = json!({
            "schema": JSON_SCHEMA_TAG,
            "command": "list",
            "skills": catalog.iter().map(|s| skill_view(s, false)).collect::<Vec<_>>(),
            "shadowed": catalog
                .shadowed()
                .iter()
                .map(|s| skill_view(s, false))
                .collect::<Vec<_>>(),
            "diagnostics": catalog.diagnostics(),
        });
        let _ = writeln!(out, "{}", pretty(&envelope));
        return 0;
    }

    for skill in catalog.iter() {
        let _ = writeln!(
            out,
            "{}  {}  {}",
            skill.meta.name,
            skill.digest_short(),
            skill.source_root.display()
        );
    }
    for skill in catalog.shadowed() {
        let _ = writeln!(
            out,
            "{}  {}  {}  (shadowed by {})",
            skill.meta.name,
            skill.digest_short(),
            skill.source_root.display(),
            skill
                .shadowed_by
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
    }
    let excluded: Vec<&SkillDiagnostic> = catalog
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect();
    if !excluded.is_empty() {
        let _ = writeln!(out, "excluded:");
        for diagnostic in excluded {
            let _ = writeln!(out, "  {}", render_diagnostic(diagnostic));
        }
    }
    0
}

fn inspect(name: &str, roots: Vec<PathBuf>, json: bool, out: &mut impl Write) -> i32 {
    let catalog = SkillLoader::new(roots).discover();
    let Some(skill) = catalog.get(name) else {
        let _ = writeln!(out, "skill `{name}` not found");
        return 1;
    };
    if json {
        let envelope = json!({
            "schema": JSON_SCHEMA_TAG,
            "command": "inspect",
            "skill": skill_view(skill, true),
        });
        let _ = writeln!(out, "{}", pretty(&envelope));
        return 0;
    }
    let body = catalog.body(name, None).map(|b| b.body).unwrap_or_default();
    let _ = writeln!(out, "name:         {}", skill.meta.name);
    let _ = writeln!(out, "description:  {}", skill.index_entry());
    let _ = writeln!(out, "dir:          {}", skill.root.display());
    let _ = writeln!(out, "source root:  {}", skill.source_root.display());
    let _ = writeln!(out, "digest:       {}", skill.digest_hex());
    let _ = writeln!(
        out,
        "body:         {} bytes, {} lines",
        body.len(),
        body.lines().count()
    );
    let _ = writeln!(out, "resources:    {}", skill.resources().len());
    for resource in skill.resources() {
        let _ = writeln!(out, "  {resource}");
    }
    0
}

fn index(roots: Vec<PathBuf>, budget: Option<usize>, out: &mut impl Write) -> i32 {
    let catalog = SkillLoader::new(roots).discover();
    let budget = budget.unwrap_or(catalog.limits().index_byte_budget);
    let _ = write!(out, "{}", catalog.render_index(budget));
    0
}

/// The stable JSON projection of one skill. `full` adds the raw frontmatter
/// and body stats (inspect); list keeps entries lean.
fn skill_view(skill: &LoadedSkill, full: bool) -> JsonValue {
    let mut view = json!({
        "name": skill.meta.name,
        "description": skill.meta.description,
        "license": skill.meta.license,
        "compatibility": skill.meta.compatibility,
        "metadata": skill.meta.metadata,
        "allowed_tools": skill.meta.allowed_tools,
        "ext": skill.ext,
        "dir": skill.root,
        "source_root": skill.source_root,
        "digest": skill.digest_hex(),
        "shadowed_by": skill.shadowed_by,
        "index_entry": skill.index_entry(),
        "resources": skill.resources(),
    });
    if full {
        let object = view.as_object_mut().expect("skill_view is an object");
        object.insert("raw".to_string(), json!(skill.raw));
    }
    view
}

fn render_diagnostic(diagnostic: &SkillDiagnostic) -> String {
    let severity = match diagnostic.severity {
        Severity::Error => "ERROR",
        Severity::Warning => "WARN",
        Severity::Info => "INFO",
    };
    let name = diagnostic
        .name
        .as_deref()
        .map(|name| format!(" [{name}]"))
        .unwrap_or_default();
    format!(
        "{severity} {}{name} {}: {}",
        diagnostic.dir.display(),
        diagnostic.rule,
        diagnostic.message
    )
}

fn pretty(value: &JsonValue) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
