use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "pocopine", about = "pocopine project CLI", version)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Build the wasm bundle (and the server bin, if configured).
    Build(BuildArgs),
    /// Build, then serve. Spawns the configured server bin if one exists;
    /// otherwise serves the project directory as static files.
    Run(ServeArgs),
    /// Same as `run`, with src/ watched for changes that retrigger the
    /// wasm build.
    Dev(ServeArgs),
    /// Check local tools and project configuration used by Pocopine.
    Doctor(DoctorArgs),
    /// Deploy to a registered host adapter (RFC 080).
    Deploy(DeployArgs),
    /// Managed JavaScript toolkit commands for typed `.client.ts` modules.
    Js(JsArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct BuildArgs {
    /// Path to the crate to build (defaults to current dir).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Build in release mode.
    #[arg(long)]
    pub release: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct ServeArgs {
    /// Path to the crate (defaults to current dir). Static files are
    /// served from this directory, and the server bin (if any) is spawned
    /// from here.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Port to listen on in static mode. Ignored in server-bin mode -
    /// the bin controls its own addr. If the port is taken, the next
    /// available port is tried (up to `port + 20`).
    #[arg(long, default_value_t = 5243)]
    pub port: u16,
    /// Build in release mode.
    #[arg(long)]
    pub release: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct DoctorArgs {
    /// Path to the project crate (defaults to current dir).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Treat warnings as failures.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct JsArgs {
    /// Path to the project crate (defaults to current dir).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    #[command(subcommand)]
    pub cmd: JsCmd,
}

#[derive(Parser, Debug, Clone)]
pub struct DeployArgs {
    /// Path to the project crate (defaults to current dir).
    #[arg(long, default_value = ".", global = true)]
    pub path: PathBuf,

    /// Subcommand. If omitted, runs a deploy with the flags below.
    #[command(subcommand)]
    pub cmd: Option<DeployCmd>,

    /// Target host adapter (e.g. `fly`).
    #[arg(long)]
    pub target: Option<String>,

    /// Print the rendered config and planned API calls; touch nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Deploy to the production environment. Sets the env to
    /// `production`, suffixes the host app name with `-production`,
    /// and applies any overrides under
    /// `[package.metadata.pocopine.deploy.production]` in Cargo.toml
    /// (env vars + host-specific blocks). Without this flag, deploys
    /// are env-agnostic (no suffix, no override block).
    #[arg(long)]
    pub prod: bool,

    /// Skip the local build + push. Use this when CI (or a previous
    /// `pocopine deploy`) has already pushed the image; the adapter
    /// will reuse the deterministic image tag and only run the
    /// host-API deploy.
    #[arg(long)]
    pub skip_build: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum DeployCmd {
    /// Manage host API tokens (`~/.pocopine/credentials.toml`).
    Auth(AuthArgs),
    /// Validate config, probe host APIs, check tokens + docker daemon.
    Doctor,
}

#[derive(Parser, Debug, Clone)]
pub struct AuthArgs {
    /// Host name (e.g. `fly`). Required unless `--list` or `--revoke`.
    pub host: Option<String>,

    /// List configured tokens and their source (file/env).
    #[arg(long, conflicts_with = "host")]
    pub list: bool,

    /// Remove the stored token for the given host.
    #[arg(long, value_name = "HOST")]
    pub revoke: Option<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum JsCmd {
    /// Create/update package.json with Pocopine's client-module toolkit.
    Init,
    /// Install client-module dependencies through the detected package manager.
    Install,
    /// Add npm packages for use from typed `.client.ts` modules.
    Add {
        /// Add packages to devDependencies.
        #[arg(short = 'D', long)]
        dev: bool,
        /// Package names and optional version ranges.
        packages: Vec<String>,
    },
}
