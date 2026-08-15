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
    /// Scaffold a new pocopine project from the starter template (alias: `create`).
    #[command(alias = "create")]
    New(NewArgs),
    /// Build the wasm bundle (and the server bin, if configured).
    Build(BuildArgs),
    /// Build, then serve. Spawns the configured server bin if one exists;
    /// otherwise serves the project directory as static files.
    Run(ServeArgs),
    /// Same as `run`, with src/ watched for changes that retrigger the
    /// wasm build.
    Dev(ServeArgs),
    /// Native desktop target (RFC-104): run/build the app inside a Tauri
    /// webview with `#[server]` functions served in-process. Needs the
    /// platform webview libraries (`webkit2gtk-4.1` + friends on Linux).
    Native(NativeArgs),
    /// Check local tools and project configuration used by Pocopine.
    Doctor(DoctorArgs),
    /// Fetch + refresh the pocopine-skills agent guides in `.claude/skills/`.
    ///
    /// The guides live in their own repo and evolve independently of the
    /// framework: `install` vendors the current set, `update` refetches the
    /// latest, and `check` reports whether a newer revision is available.
    Skills(SkillsArgs),
    /// Deploy to a registered host adapter (RFC 080).
    Deploy(DeployArgs),
    /// RFC-100 asset pipeline: sync `assets/` to the configured
    /// S3-compatible bucket under content-addressed keys, and manage
    /// the bucket access keys.
    Assets(AssetsArgs),
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
    /// Run the pocopine language server over stdio (used by the VSCode
    /// extension and other LSP clients). Backed by pocopine-template-parser.
    Lsp(LspArgs),
    /// Keep templates in canonical form (RFC-117): small ones inline as
    /// `poco! { … }`, large ones in their own `.poco` file.
    Fmt(FmtArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct FmtArgs {
    /// Project directory (defaults to the current one).
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Report what would change and exit non-zero if anything would —
    /// writes nothing. The CI shape.
    #[arg(long)]
    pub check: bool,
    /// Also apply rules configured as `warn`, for this run only.
    #[arg(long, conflicts_with = "check")]
    pub fix: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct LspArgs {
    /// Communicate over stdio. This is the default and currently the only
    /// supported transport; the flag exists so editor clients can pass it
    /// explicitly.
    #[arg(long)]
    pub stdio: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct NewArgs {
    /// Name of the new project (used as the directory and crate name).
    pub name: String,
    /// Parent directory to create the project in (defaults to current dir).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Add the pocopine-skills agent guides at `.claude/skills/` as a git
    /// submodule (needs network + access to the repo). Off by default.
    #[arg(long)]
    pub skills: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct SkillsArgs {
    /// Path to the project (defaults to current dir).
    #[arg(long, default_value = ".", global = true)]
    pub path: PathBuf,
    #[command(subcommand)]
    pub cmd: SkillsCmd,
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum SkillsCmd {
    /// Fetch the agent guides into `.claude/skills/` (first-time install).
    Install,
    /// Refetch the latest guides, reporting any newly added or removed skills.
    Update,
    /// Check (cheaply, no clone) whether a newer revision is available upstream.
    Check,
    /// List the skills currently installed in `.claude/skills/`.
    List,
}

#[derive(Parser, Debug, Clone)]
pub struct BuildArgs {
    /// Path to the crate to build (defaults to current dir).
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Build in release mode.
    #[arg(long)]
    pub release: bool,
    /// Force the Pine Stylekit CSS stage on, even where it would
    /// otherwise defer (e.g. a Tailwind-only project). On by default.
    #[arg(long)]
    pub stylekit: bool,
    /// Skip the Pine Stylekit CSS stage (it runs by default, RFC 092).
    #[arg(long = "no-stylekit", conflicts_with = "stylekit")]
    pub no_stylekit: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct ServeArgs {
    /// Path to the crate (defaults to current dir). Static files are
    /// served from this directory, and the server bin (if any) is spawned
    /// from here.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Override the listen port (no `Cargo.toml` edit needed). In static
    /// mode the CLI listens here (default 5243; if taken, the next free
    /// port up to +20 is used). In server-bin mode the configured bin is
    /// launched with `PORT=<this>`, overriding
    /// `[package.metadata.pocopine].port` — pocopine server bins read `PORT`.
    #[arg(long)]
    pub port: Option<u16>,
    /// Build in release mode.
    #[arg(long)]
    pub release: bool,
    /// Force the Pine Stylekit CSS stage on. On by default.
    #[arg(long)]
    pub stylekit: bool,
    /// Skip the Pine Stylekit CSS stage (it runs by default, RFC 092).
    #[arg(long = "no-stylekit", conflicts_with = "stylekit")]
    pub no_stylekit: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct NativeArgs {
    /// Path to the project crate (defaults to current dir).
    #[arg(long, default_value = ".", global = true)]
    pub path: PathBuf,
    #[command(subcommand)]
    pub cmd: NativeCmd,
}

#[derive(Subcommand, Debug, Clone)]
pub enum NativeCmd {
    /// Scaffold the `src-tauri` host crate for this project (idempotent;
    /// never overwrites existing files).
    Init,
    /// Build the wasm bundle + CSS, then run the native window with the
    /// live project directory as the asset root — a rebuild is picked up
    /// on reload. Scaffolds `src-tauri/` first if it is missing.
    Dev(NativeDevArgs),
    /// Build the wasm bundle (release) + CSS, then build the native
    /// binary (and the installer bundle via `cargo tauri build` when the
    /// Tauri CLI is available).
    Build(NativeBuildArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct NativeDevArgs {
    /// Build the wasm bundle in release mode (default: debug, for a fast
    /// edit loop).
    #[arg(long)]
    pub release: bool,
    /// Forward the app's `#[server]` calls to this deployed server URL
    /// ("server" mode). Omitted → "standalone": the functions run
    /// in-process. Point `dev` at a local `pocopine run` or staging.
    #[arg(long)]
    pub backend: Option<String>,
    /// Force the Pine Stylekit CSS stage on. On by default.
    #[arg(long)]
    pub stylekit: bool,
    /// Skip the Pine Stylekit CSS stage (it runs by default, RFC 092).
    #[arg(long = "no-stylekit", conflicts_with = "stylekit")]
    pub no_stylekit: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct NativeBuildArgs {
    /// Build the wasm bundle in debug mode (default: release).
    #[arg(long)]
    pub debug: bool,
    /// Forward the app's `#[server]` calls to this deployed server URL
    /// ("server" mode). Omitted → "standalone": the functions run
    /// in-process. The URL comes from `pocopine deploy status`.
    #[arg(long)]
    pub backend: Option<String>,
    /// Force the Pine Stylekit CSS stage on. On by default.
    #[arg(long)]
    pub stylekit: bool,
    /// Skip the Pine Stylekit CSS stage (it runs by default, RFC 092).
    #[arg(long = "no-stylekit", conflicts_with = "stylekit")]
    pub no_stylekit: bool,
    /// Skip the installer/bundle step (`cargo tauri build`) even when the
    /// Tauri CLI is present; build only the host binary with `cargo`.
    #[arg(long)]
    pub no_bundle: bool,
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
pub struct AssetsArgs {
    /// Path to the project crate (defaults to current dir).
    #[arg(long, default_value = ".", global = true)]
    pub path: PathBuf,
    #[command(subcommand)]
    pub cmd: AssetsCmd,
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum AssetsCmd {
    /// Hash-diff sync `assets/` to the bucket declared in
    /// `[package.metadata.pocopine.assets]`: every file uploads to
    /// `assets/<hash8>/<path>` with its MIME type and an immutable
    /// cache header; keys that already exist are skipped. Also runs
    /// automatically during `pocopine deploy`, before the app flip.
    Push,
    /// Store the bucket access keys (`~/.pocopine/credentials.toml`,
    /// mode 0600). `POCOPINE_ASSETS_ACCESS_KEY_ID` /
    /// `POCOPINE_ASSETS_SECRET_ACCESS_KEY` override the file in CI.
    Auth,
}

#[derive(Parser, Debug, Clone)]
pub struct DeployArgs {
    /// Path to the project crate (defaults to current dir).
    #[arg(long, default_value = ".", global = true)]
    pub path: PathBuf,

    /// Subcommand. If omitted, runs a deploy with the flags below.
    #[command(subcommand)]
    pub cmd: Option<DeployCmd>,

    /// Target host adapter (e.g. `cf-pages`, `railway`, or `render`).
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

    /// Skip the local build. Use this when CI (or a previous deploy)
    /// already produced the static dist or pushed the container image;
    /// the adapter reuses that artefact and only runs the host API.
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
    /// Validate local deploy prerequisites and configured host tokens.
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
        /// Host name (`cf-pages`, `render`, `railway`, …).
        host: String,
        /// Field name (`account_id`, `owner_id`, `workspace_id`, `region`, …).
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
    /// Host name (e.g. `cf-pages` or `railway`). Required unless
    /// `--list` or `--revoke`.
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
