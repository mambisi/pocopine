//! `pocopine` - project CLI.
//!
//! Core subcommands:
//!
//! * `build` - wraps `wasm-pack build --target web` (plus `cargo build
//!   --bin <name>` when a server binary is configured) and bundles
//!   typed `.client.ts` modules when present.
//! * `run`   - build once, then either spawn the configured server bin
//!   OR serve the project directory as static files.
//! * `dev`   - same routing as `run`, plus a file watcher that rebuilds
//!   the wasm bundle on src changes.
//! * `doctor` - checks the local tools and project config used by the CLI.
//! * `js`    - managed JS toolkit commands for client-module deps.
//!
//! Project config lives in the project's `Cargo.toml` under
//! `[package.metadata.pocopine]`. See
//! `examples/blog/Cargo.toml` for a complete server-bin example.

mod args;
mod assets_sync;
mod build;
mod client_modules;
mod component_index;
mod config;
mod create;
mod deploy;
mod dev;
mod doctor;
mod env;
// RFC-117 — `pocopine fmt`, the rules that own where a template lives.
mod fmt;
// RFC-116 — name the lexer-hostile text in a `poco!` body before cargo
// reports it as an unexplained tokenizer error.
mod inline_lint;
mod locale;
mod lsp;
mod native;
mod server;
mod skills;
mod stylekit;
mod tailwind;
mod tools;

use anyhow::{Context, Result};
use clap::Parser;
use std::io::{self, Read as _};

use args::{AssetsCmd, Cli, Cmd, EnvArgs, EnvCmd, JsCmd};

/// Install a `tracing` subscriber so `tracing::info!`/`warn!` from
/// every dependency (including the deploy adapters) actually surfaces
/// to the user. Defaults to `info` for first-party crates and `warn`
/// for everything else; `RUST_LOG` overrides verbatim.
fn install_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let default = "warn,pocopine=info,pocopine_cli=info,pocopine_deploy=info,\
                   pocopine_deploy_cloudflare_pages=info,\
                   pocopine_deploy_railway=info,pocopine_deploy_render=info";
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .compact()
        .try_init();
}

fn main() -> Result<()> {
    install_tracing();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::New(args) => create::run(&args),
        Cmd::Build(args) => run_build(args),
        Cmd::Run(args) => run_project(args),
        Cmd::Dev(args) => dev::run(&args),
        Cmd::Native(args) => native::run(args),
        Cmd::Doctor(args) => doctor::run(&args),
        Cmd::Skills(args) => skills::run(&args),
        Cmd::Deploy(args) => deploy::run(&args),
        Cmd::Assets(args) => run_assets(args),
        Cmd::Js(args) => run_js(args),
        Cmd::Env(args) => run_env(args),
        Cmd::Stylekit(args) => {
            stylekit::run_command(&args.path, args.dump, args.docs, args.metadata)
        }
        Cmd::Lsp(args) => lsp::run(args),
        Cmd::Fmt(args) => fmt::run(&args),
    }
}

/// `pocopine assets <push|auth>` — the RFC-100 write path.
fn run_assets(args: args::AssetsArgs) -> Result<()> {
    match args.cmd {
        AssetsCmd::Push => {
            let project = args.path.canonicalize().with_context(|| {
                format!("could not resolve project path {}", args.path.display())
            })?;
            let cfg = config::load(&project)?;
            let assets_cfg = assets_sync::require_config(&cfg)?;
            assets_sync::push(&project, assets_cfg)?;
            Ok(())
        }
        AssetsCmd::Auth => assets_sync::auth(),
    }
}

fn run_env(args: EnvArgs) -> Result<()> {
    let project = args
        .path
        .canonicalize()
        .with_context(|| format!("could not resolve project path {}", args.path.display()))?;
    match args.cmd {
        EnvCmd::Set { key, value } => {
            let value = match value {
                Some(v) => v,
                None => read_value_from_stdin(&key)?,
            };
            let replaced = env::set(&project, &key, &value)?;
            if replaced {
                println!("✓ updated {key} in {}", env::env_path(&project).display());
            } else {
                println!("✓ wrote {key} to {}", env::env_path(&project).display());
            }
            Ok(())
        }
        EnvCmd::Get { key } => match env::get(&project, &key)? {
            Some(value) => {
                println!("{value}");
                Ok(())
            }
            None => {
                anyhow::bail!("{key} is not set in {}", env::env_path(&project).display());
            }
        },
        EnvCmd::List { show_values } => {
            let entries = env::list(&project)?;
            if entries.is_empty() {
                println!("(no env vars set; use `pocopine env set KEY value` to add one)");
                return Ok(());
            }
            for (key, value) in entries {
                let displayed = if show_values {
                    value
                } else {
                    env::mask(&value)
                };
                println!("{key}={displayed}");
            }
            Ok(())
        }
        EnvCmd::Unset { key } => {
            let removed = env::unset(&project, &key)?;
            if removed {
                println!("✓ removed {key}");
            } else {
                println!("(nothing to do; {key} was not set)");
            }
            Ok(())
        }
    }
}

fn read_value_from_stdin(key: &str) -> Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .with_context(|| format!("read value for {key} from stdin"))?;
    // Strip a single trailing newline so `echo $VALUE | pocopine env set …`
    // doesn't capture the echo terminator. Multi-line values are rejected
    // by `env::set` itself.
    if buf.ends_with('\n') {
        buf.pop();
        if buf.ends_with('\r') {
            buf.pop();
        }
    }
    Ok(buf)
}

fn run_build(args: args::BuildArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = config::load(&args.path)?;
    // Ahead of cargo, so unreadable text in an inline template is reported as
    // that, rather than as a bare `unknown start of token` naming no template.
    inline_lint::check_project(&project)?;
    build::wasm(&project, args.release)?;
    client_modules::build(&project, args.release)?;
    if !args.no_bins {
        build::configured_bins(&args.path, &cfg, args.release)?;
    }
    if let Some(tw) = cfg.tailwind.as_ref() {
        tailwind::run_once(&project, tw, args.release)?;
    }
    if stylekit::enabled(&cfg, args.stylekit, args.no_stylekit) {
        stylekit::run_once(&project, &cfg, args.stylekit, args.release)?;
    }
    Ok(())
}

fn run_project(args: args::ServeArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = config::load(&args.path)?;
    server::check_configured_port_available(&cfg, args.port)?;
    inline_lint::check_project(&project)?;
    build::wasm(&project, args.release)?;
    client_modules::build(&project, args.release)?;
    build::configured_bins(&project, &cfg, args.release)?;
    if let Some(tw) = cfg.tailwind.as_ref() {
        tailwind::run_once(&project, tw, args.release)?;
    }
    if stylekit::enabled(&cfg, args.stylekit, args.no_stylekit) {
        stylekit::run_once(&project, &cfg, args.stylekit, args.release)?;
    }
    server::run_project(&args.path, &cfg, args.release, args.port)
}

fn run_js(args: args::JsArgs) -> Result<()> {
    match args.cmd {
        JsCmd::Init => client_modules::init(&args.path),
        JsCmd::Install => client_modules::install(&args.path),
        JsCmd::Add { packages, dev } => client_modules::add(&args.path, &packages, dev),
    }
}
