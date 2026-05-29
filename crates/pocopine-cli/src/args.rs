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
    /// Manage the project's `.env` file (dev-only environment variables).
    ///
    /// `pocopine dev` loads `.env` into the spawned server + worker bins.
    /// `pocopine run` does NOT — production-shape runs inherit the parent
    /// environment unchanged, so values you set here never leak into
    /// real prod from a Pocopine-managed code path.
    Env(EnvArgs),
    /// Pine Stylekit utility-CSS compiler (RFC 092). Hidden debug verb:
    /// compile once and print the stylesheet (or write it to --output).
    #[command(hide = true)]
    Stylekit(StylekitArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct BuildArgs {
    /// Path to the crate to build (defaults to current dir).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Build in release mode.
    #[arg(long)]
    pub release: bool,
    /// Opt into the Pine Stylekit CSS stage even if the project has no
    /// `[package.metadata.pocopine.stylekit]` block (RFC 092 D2).
    #[arg(long)]
    pub stylekit: bool,
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
    /// Opt into the Pine Stylekit CSS stage (RFC 092 D2).
    #[arg(long)]
    pub stylekit: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct StylekitArgs {
    /// Path to the project crate (defaults to current dir).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Print the generated CSS to stdout instead of writing the output
    /// file.
    #[arg(long)]
    pub dump: bool,
    /// Print the utility catalog as Markdown (regenerates the docs
    /// table) and exit. Does not compile the project.
    #[arg(long)]
    pub docs: bool,
    /// Print LSP/autocomplete metadata as JSON and exit. Does not
    /// compile the project.
    #[arg(long)]
    pub metadata: bool,
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

    /// Operate on every deployable workspace member (any crate with a
    /// `[package.metadata.pocopine.deploy]` table). Resolves the
    /// workspace root from `--path`. Without this flag the command
    /// runs against a single project.
    #[arg(long, alias = "all")]
    pub workspace: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum DeployCmd {
    /// Manage host API tokens (`~/.pocopine/credentials.toml`).
    Auth(AuthArgs),
    /// Validate config, probe host APIs, check tokens + docker daemon.
    Doctor,
    /// Show the current deploy state per process on the target host.
    Status(StatusArgs),
    /// Manage per-user host config (`~/.pocopine/config.toml`) — the
    /// fallback tier for `owner_id` / `workspace_id` / `region` /
    /// `org` when neither $POCOPINE_<HOST>_<FIELD> nor the project's
    /// `[deploy.<host>]` block is set.
    Config(ConfigArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub cmd: ConfigCmd,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ConfigCmd {
    /// Write `[default.<host>] <field> = <value>` to
    /// `~/.pocopine/config.toml`. If `<value>` is omitted, read it
    /// from stdin (so secrets aren't visible in shell history).
    Set {
        /// Host name (`render`, `railway`, `fly`, …).
        host: String,
        /// Field name (`owner_id`, `workspace_id`, `region`, …).
        field: String,
        /// Value. Omit to read from stdin.
        value: Option<String>,
    },
    /// Show the resolved value of a field across all three tiers
    /// (env / project / file), naming the tier each came from. The
    /// project tier is read from the directory passed via the
    /// `--path` global flag on `pocopine deploy` (defaults to `.`).
    Get { host: String, field: String },
    /// Show every (host, field, source) tuple visible across the
    /// file and the env.
    List,
    /// Remove `<field>` from `[default.<host>]`. Idempotent.
    Revoke { host: String, field: String },
}

#[derive(Parser, Debug, Clone)]
pub struct StatusArgs {
    /// Emit JSON instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
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

#[derive(Parser, Debug, Clone)]
pub struct EnvArgs {
    /// Path to the project crate (defaults to current dir).
    #[arg(long, default_value = ".", global = true)]
    pub path: PathBuf,
    #[command(subcommand)]
    pub cmd: EnvCmd,
}

#[derive(Subcommand, Debug, Clone)]
pub enum EnvCmd {
    /// Set or overwrite a key in `.env`. Adds `.env` to `.gitignore` on
    /// first use. If `<value>` is omitted, the value is read from stdin
    /// (no trailing newline) so secrets stay out of shell history.
    Set {
        /// Variable name (e.g. `DATABASE_URL`). Must match
        /// `[A-Za-z_][A-Za-z0-9_]*`.
        key: String,
        /// Value. Omit to read from stdin.
        value: Option<String>,
    },
    /// Print the value of a single key, or exit non-zero if it is unset.
    Get { key: String },
    /// List every key currently set in `.env`. Values are masked unless
    /// `--show-values` is passed.
    List {
        /// Print full values instead of the masked preview.
        #[arg(long)]
        show_values: bool,
    },
    /// Remove a key. Idempotent.
    Unset { key: String },
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
