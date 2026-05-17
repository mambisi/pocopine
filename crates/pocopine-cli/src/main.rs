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
mod build;
mod client_modules;
mod config;
mod dev;
mod doctor;
mod server;
mod tailwind;
mod tools;

use anyhow::Result;
use clap::Parser;

use args::{Cli, Cmd, JsCmd};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build(args) => run_build(args),
        Cmd::Run(args) => run_project(args),
        Cmd::Dev(args) => dev::run(&args),
        Cmd::Doctor(args) => doctor::run(&args),
        Cmd::Js(args) => run_js(args),
    }
}

fn run_build(args: args::BuildArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = config::load(&args.path)?;
    build::wasm(&project, args.release)?;
    client_modules::build(&project, args.release)?;
    build::configured_bins(&args.path, &cfg, args.release)?;
    if let Some(tw) = cfg.tailwind.as_ref() {
        tailwind::run_once(&project, tw, args.release)?;
    }
    Ok(())
}

fn run_project(args: args::ServeArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = config::load(&args.path)?;
    server::check_configured_port_available(&cfg)?;
    build::wasm(&project, args.release)?;
    client_modules::build(&project, args.release)?;
    build::configured_bins(&project, &cfg, args.release)?;
    if let Some(tw) = cfg.tailwind.as_ref() {
        tailwind::run_once(&project, tw, args.release)?;
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
