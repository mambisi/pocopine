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
    /// Managed JavaScript toolkit commands for `.client.js` / `.client.ts`.
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

#[derive(Subcommand, Debug, Clone)]
pub enum JsCmd {
    /// Create/update package.json with Pocopine's client-module toolkit.
    Init,
    /// Install client-module dependencies through the detected package manager.
    Install,
    /// Add npm packages for use from `.client.js` / `.client.ts` modules.
    Add {
        /// Add packages to devDependencies.
        #[arg(short = 'D', long)]
        dev: bool,
        /// Package names and optional version ranges.
        packages: Vec<String>,
    },
}
