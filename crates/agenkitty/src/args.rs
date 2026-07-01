use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use pocopine_agenkit::server::SecretString;

use crate::events::{EventFormat, render_human, render_jsonl};
use crate::project::ProjectContext;
use crate::supervisor::{AgentRunOptions, FrameworkRunner, QwenProviderConfig};
use crate::tools::resolve_tool_ids;

/// Agenkitty framework CLI.
#[derive(Debug, Parser)]
#[command(name = "agenkitty")]
#[command(about = "Open agent framework on top of Pocopine Agenkit")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate project discovery and framework configuration.
    Doctor(DoctorArgs),
    /// Run one prompt through the local framework runner.
    Run(RunArgs),
    /// Internal: the bubblewrap egress-proxy entrypoint. Not a user command — it
    /// runs the real command behind an in-namespace loopback→UDS relay.
    #[cfg(unix)]
    #[command(name = "__egress-shim", hide = true)]
    EgressShim(EgressShimArgs),
}

#[derive(Debug, Parser)]
struct DoctorArgs {
    /// Project root.
    #[arg(long, default_value = ".")]
    path: PathBuf,
}

#[derive(Debug, Parser)]
struct RunArgs {
    /// Project root.
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Provider selection.
    #[arg(long, default_value = "mock")]
    provider: ProviderArg,
    /// Read provider settings from a dotenv-style file. Currently used for
    /// QWEN_API_KEY, QWEN_BASE_URL, and QWEN_MODEL.
    #[arg(long, value_name = "PATH")]
    env_file: Option<PathBuf>,
    /// Model alias to bind on the Agenkit runtime. For qwen, a bare model like
    /// `qwen-plus` is normalized to `qwen/qwen-plus`.
    #[arg(long)]
    model: Option<String>,
    /// Comma-separated tool ids to expose to the agent. Use `none` to disable
    /// tools for a run.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "fs.search,fs.list,fs.read,fs.stat,fs.exists,session.info,session.events"
    )]
    tools: Vec<String>,
    /// Agent id persisted on the thread.
    #[arg(long, default_value = "agenkitty")]
    agent_id: String,
    /// Resume an existing durable agent thread id.
    #[arg(long)]
    thread_id: Option<String>,
    /// Durable session root for transcript and Agenkitty metadata. Relative
    /// paths are resolved against the project root.
    #[arg(long, value_name = "PATH")]
    session_root: Option<PathBuf>,
    /// Render format.
    #[arg(long, default_value = "human")]
    format: OutputFormat,
    /// Prompt to send to the agent.
    prompt: String,
}

#[cfg(unix)]
#[derive(Debug, Parser)]
struct EgressShimArgs {
    /// The host egress proxy's Unix socket, bound into the sandbox.
    #[arg(long)]
    uds: PathBuf,
    /// The command to run behind the relay (everything after `--`).
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProviderArg {
    Mock,
    Qwen,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Human,
    Jsonl,
}

impl From<OutputFormat> for EventFormat {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Human => EventFormat::Human,
            OutputFormat::Jsonl => EventFormat::Jsonl,
        }
    }
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor(args) => doctor(args).await,
        Command::Run(args) => run_prompt(args).await,
        #[cfg(unix)]
        Command::EgressShim(args) => {
            // Replace this process's exit status with the wrapped command's.
            let code = crate::tools::process::run_egress_shim(args.uds, args.command).await;
            std::process::exit(code);
        }
    }
}

async fn doctor(args: DoctorArgs) -> Result<()> {
    let project = ProjectContext::discover(&args.path)
        .with_context(|| format!("discover project at `{}`", args.path.display()))?;
    println!("agenkitty doctor");
    println!("  root: {}", project.root.display());
    println!("  AGENTS.md: {}", yes_no(project.agents_md.is_some()));
    println!(
        "  .agenkitty/instructions.md: {}",
        yes_no(project.agenkitty_instructions.is_some())
    );
    Ok(())
}

async fn run_prompt(args: RunArgs) -> Result<()> {
    let project = ProjectContext::discover(&args.path)
        .with_context(|| format!("discover project at `{}`", args.path.display()))?;
    let system = project.system_prompt()?;
    let tool_ids = resolve_tool_ids(&args.tools).map_err(|err| anyhow::anyhow!(err))?;
    let session_root = resolve_session_root(&project.root, args.session_root.as_deref());
    let (runner, model) = match args.provider {
        ProviderArg::Mock => (
            FrameworkRunner::mock_for_project_with_session_root(&project.root, &session_root)?,
            args.model.unwrap_or_else(|| "local/default".to_string()),
        ),
        ProviderArg::Qwen => qwen_runner(args.model, args.env_file, &project.root, &session_root)?,
    };
    let report = runner
        .run_prompt(AgentRunOptions {
            agent_id: args.agent_id,
            model,
            system,
            prompt: args.prompt,
            thread_id: args.thread_id,
            max_steps_per_turn: 8,
            tool_ids,
        })
        .await?;

    match EventFormat::from(args.format) {
        EventFormat::Human => render_human(&report.events),
        EventFormat::Jsonl => {
            for line in render_jsonl(&report.events)? {
                println!("{line}");
            }
        }
    }

    if report.failed() {
        bail!("agent run failed");
    }
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn resolve_session_root(project_root: &Path, session_root: Option<&Path>) -> PathBuf {
    match session_root {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => project_root.join(path),
        None => project_root.join(".agenkitty").join("sessions"),
    }
}

fn qwen_runner(
    model: Option<String>,
    env_file: Option<PathBuf>,
    root: &Path,
    session_root: &Path,
) -> Result<(FrameworkRunner, String)> {
    let env_file = env_file
        .as_deref()
        .map(read_qwen_env_file)
        .transpose()?
        .unwrap_or_default();
    let model = qwen_model_ref(model, &env_file);
    let api_key = env_file
        .api_key
        .or_else(|| std::env::var("QWEN_API_KEY").ok().map(SecretString::new))
        .context("QWEN_API_KEY is not set")?;
    let base_url = env_file
        .base_url
        .or_else(|| std::env::var("QWEN_BASE_URL").ok())
        .context("QWEN_BASE_URL is not set")?;
    let runner = FrameworkRunner::qwen_for_project_with_session_root(
        QwenProviderConfig {
            api_key,
            base_url,
            default_model: model.clone(),
        },
        root,
        session_root,
    )?;
    Ok((runner, model))
}

fn qwen_model_ref(model: Option<String>, env_file: &QwenEnvFile) -> String {
    let model = model
        .or_else(|| env_file.model.clone())
        .or_else(|| std::env::var("QWEN_MODEL").ok())
        .unwrap_or_else(|| "qwen-plus".to_string());
    normalize_qwen_model_ref(model)
}

fn normalize_qwen_model_ref(model: String) -> String {
    if model.contains('/') {
        model
    } else {
        format!("qwen/{model}")
    }
}

#[derive(Default)]
struct QwenEnvFile {
    api_key: Option<SecretString>,
    base_url: Option<String>,
    model: Option<String>,
}

fn read_qwen_env_file(path: &Path) -> Result<QwenEnvFile> {
    let content =
        fs::read_to_string(path).with_context(|| format!("read env file `{}`", path.display()))?;
    parse_qwen_env_file(&content)
}

fn parse_qwen_env_file(content: &str) -> Result<QwenEnvFile> {
    let mut parsed = QwenEnvFile::default();
    for (index, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            bail!("invalid env line {}: expected KEY=value", index + 1);
        };
        let key = key.trim();
        let value = unquote_env_value(value.trim());
        match key {
            "QWEN_API_KEY" => parsed.api_key = Some(SecretString::new(value)),
            "QWEN_BASE_URL" => parsed.base_url = Some(value),
            "QWEN_MODEL" => parsed.model = Some(value),
            _ => {}
        }
    }
    Ok(parsed)
}

fn unquote_env_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_qwen_model_ref, parse_qwen_env_file, qwen_model_ref, resolve_session_root,
    };
    use std::path::Path;

    #[test]
    fn qwen_model_ref_accepts_bare_or_prefixed_model() {
        assert_eq!(
            normalize_qwen_model_ref("qwen-plus".to_string()),
            "qwen/qwen-plus"
        );
        assert_eq!(
            normalize_qwen_model_ref("qwen/qwen-max".to_string()),
            "qwen/qwen-max"
        );
    }

    #[test]
    fn qwen_model_ref_prefers_explicit_then_env_file_model() {
        let env = parse_qwen_env_file("QWEN_MODEL=qwen-max").unwrap();
        assert_eq!(qwen_model_ref(None, &env), "qwen/qwen-max");
        assert_eq!(
            qwen_model_ref(Some("qwen-turbo".to_string()), &env),
            "qwen/qwen-turbo"
        );
    }

    #[test]
    fn qwen_env_file_parses_quotes_and_ignores_unknown_keys() {
        let env = parse_qwen_env_file(
            r#"
# comment
export QWEN_API_KEY="secret"
QWEN_BASE_URL='https://example.test/compatible-mode/v1'
QWEN_MODEL=qwen-plus
OTHER=value
"#,
        )
        .unwrap();
        assert_eq!(env.api_key.unwrap().expose(), "secret");
        assert_eq!(
            env.base_url.as_deref(),
            Some("https://example.test/compatible-mode/v1")
        );
        assert_eq!(env.model.as_deref(), Some("qwen-plus"));
    }

    #[test]
    fn session_root_defaults_under_project_and_accepts_overrides() {
        let project = Path::new("/workspace/project");
        assert_eq!(
            resolve_session_root(project, None),
            project.join(".agenkitty").join("sessions")
        );
        assert_eq!(
            resolve_session_root(project, Some(Path::new("state/sessions"))),
            project.join("state/sessions")
        );
        assert_eq!(
            resolve_session_root(project, Some(Path::new("/tmp/agenkitty-sessions"))),
            Path::new("/tmp/agenkitty-sessions")
        );
    }
}
