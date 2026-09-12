use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// `[package.metadata.pocopine]` section parsed from a project's
/// `Cargo.toml`. All fields optional - missing = "use defaults".
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PocopineConfig {
    /// Name of the server binary to spawn in `run` / `dev`. When set,
    /// `pocopine` delegates serving entirely to this bin.
    pub bin: Option<String>,
    /// Name of the worker binary to spawn alongside the server/static
    /// server in `run` / `dev`.
    pub worker_bin: Option<String>,
    /// Advisory port shown in log output for server-bin mode. The bin
    /// binds whatever it wants; pocopine does not override it.
    #[allow(dead_code)]
    pub port: Option<u16>,
    /// Cargo profile for the release wasm build, passed to `wasm-pack
    /// --profile`. Defaults to `release`.
    ///
    /// Exists because cargo profiles are workspace-wide while the two
    /// targets want opposite things. A server wants `opt-level = 3` and
    /// unwinding panics it can catch per request; a wasm bundle wants
    /// `opt-level = "z"` and `panic = "abort"`, which costs nothing on
    /// `wasm32-unknown-unknown` because there is no unwinding there to
    /// begin with. Without a separate profile, tuning either one
    /// pessimizes the other.
    ///
    /// ```toml
    /// [package.metadata.pocopine]
    /// wasm-profile = "wasm-release"
    ///
    /// [profile.wasm-release]
    /// inherits = "release"
    /// opt-level = "z"
    /// lto = "fat"
    /// codegen-units = 1
    /// panic = "abort"
    /// ```
    pub wasm_profile: Option<String>,
    /// Opt into bundled Tailwind. When present, `pocopine-cli` runs
    /// the Tailwind standalone CLI alongside `wasm-pack` - one-shot
    /// on `build`/`run`, watch mode on `dev`.
    pub tailwind: Option<TailwindConfig>,
    /// Pine Stylekit (RFC 092). Runs by default; this block customizes
    /// it (input/output/preflight) or opts out via `enabled = false`.
    /// A project with a `[tailwind]` block but no `[stylekit]` block
    /// defers to Tailwind instead.
    pub stylekit: Option<StylekitConfig>,
    /// RFC-117 `pocopine fmt` rules. Absent means the defaults apply.
    pub fmt: Option<FmtConfig>,
    /// RFC-100 asset pipeline. When present, `pocopine assets push`
    /// (and `pocopine deploy`, before the app flip) syncs the
    /// project's `assets/` tree to the configured S3-compatible
    /// bucket under content-addressed keys.
    pub assets: Option<AssetsConfig>,
    /// Boot loader: the full-screen logo splash (cold load) + thin top
    /// progress bar injected into the generated `pkg/index.html`. On by
    /// default; customize the logo / splash, or opt out via
    /// `enabled = false`.
    pub loader: Option<LoaderConfig>,
    // RFC-104 native target has no config block — it's convention
    // (`src-tauri/`) + CLI flags (`--backend`, …). See `native.rs`.
}

/// `[package.metadata.pocopine.assets]` — the RFC-100 asset bucket.
/// (RFC 080 §4.1 note applies: `Pocopine.toml` is the long-term home
/// for project config; cargo metadata is what exists today.)
///
/// ```toml
/// [package.metadata.pocopine.assets]
/// endpoint = "https://<account>.r2.cloudflarestorage.com"  # omit for AWS S3
/// bucket = "myapp-assets"
/// region = "auto"                                # optional
/// public-base = "https://assets.example.com"     # optional → Mode A
/// ```
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AssetsConfig {
    /// S3-compatible endpoint URL (Railway Bucket endpoint, R2, MinIO,
    /// …). Omit for AWS S3 itself — the SDK then derives the endpoint
    /// from `region`. Custom endpoints are addressed path-style.
    pub endpoint: Option<String>,
    /// Bucket name holding the `assets/<hash8>/<path>` keys. Required.
    pub bucket: String,
    /// Bucket region. Optional; defaults to `us-east-1`, which is what
    /// most S3-compatible stores (MinIO, R2 with `auto`, Railway)
    /// accept for signing.
    pub region: Option<String>,
    /// Public base URL under which the bucket contents are reachable
    /// without credentials (public bucket or CDN in front of it).
    /// Presence selects **Mode A**: deploys set
    /// `POCOPINE_ASSET_BASE = public-base` and the app's asset URLs
    /// point straight at the CDN. Absent → **Mode B**: URLs stay on
    /// the default `/assets` base and the web service proxies them
    /// from the (private) bucket.
    pub public_base: Option<String>,
}

/// `[package.metadata.pocopine.stylekit]` - configure the in-process
/// Pine Stylekit compiler (RFC 092). All fields optional.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StylekitConfig {
    /// Entry CSS holding the `@theme` token block. Defaults to
    /// `app.css` at the project root.
    #[serde(default = "default_sk_input")]
    pub input: String,
    /// Output stylesheet path. Defaults to `pkg/stylekit.css` so it
    /// sits alongside the wasm bundle.
    #[serde(default = "default_sk_output")]
    pub output: String,
    /// Directory scanned for `.poco` sources. Defaults to `src`.
    #[serde(default = "default_sk_src")]
    pub src: String,
    /// Prepend the base reset (Preflight). Defaults to `true`; set
    /// `preflight = false` for pages that bring their own reset.
    #[serde(default = "default_sk_preflight")]
    pub preflight: bool,
    /// Whether the stage runs. Defaults to `true`; set `enabled = false`
    /// to keep this config block but opt out of the now-default stage.
    #[serde(default = "default_sk_enabled")]
    pub enabled: bool,
}

impl Default for StylekitConfig {
    fn default() -> Self {
        Self {
            input: default_sk_input(),
            output: default_sk_output(),
            src: default_sk_src(),
            preflight: default_sk_preflight(),
            enabled: default_sk_enabled(),
        }
    }
}

/// `[package.metadata.pocopine.loader]` — the injected boot loader: a
/// full-screen logo splash on cold load plus a thin top progress bar that
/// tracks the wasm download and (via `pocopine::progress`) in-app activity.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LoaderConfig {
    /// Inject the loader into the generated `pkg/index.html` (default true).
    #[serde(default = "default_loader_bool")]
    pub enabled: bool,
    /// Show the full-screen logo splash on cold load (default true). When
    /// false, only the top progress bar is injected.
    #[serde(default = "default_loader_bool")]
    pub splash: bool,
    /// Splash logo URL. Defaults to the page's `<link rel="icon">` href.
    pub logo: Option<String>,
}

impl Default for LoaderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            splash: true,
            logo: None,
        }
    }
}

fn default_loader_bool() -> bool {
    true
}

/// How hard a `pocopine fmt` rule bites. Mirrors clippy's model: the
/// developer decides, per rule, between silence, a report, and a rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FmtLevel {
    /// Rule does not run.
    Off,
    /// Report, but change nothing. `--fix` promotes this to `Fix`.
    Warn,
    /// Rewrite on a plain `pocopine fmt`.
    Fix,
}

/// `[package.metadata.pocopine.fmt]` — RFC-117 structural rules.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FmtConfig {
    /// Line count separating "small enough to live inline" from "big
    /// enough to deserve its own file". `0` disables both rules below.
    #[serde(default = "default_fmt_threshold")]
    pub inline_threshold: usize,
    /// Pull a `.poco` file under the threshold into `template = poco! { … }`.
    #[serde(default = "default_fmt_inline")]
    pub inline_small_templates: FmtLevel,
    /// Report an inline body at or over the threshold; `--fix` writes it
    /// out to `<Struct>.poco`. Defaults to `warn` rather than `fix`
    /// because extracting creates a file, which is the author's call.
    #[serde(default = "default_fmt_extract")]
    pub extract_large_inline: FmtLevel,
    /// Reindent and re-wrap template markup itself, in `.poco` files and in
    /// `poco!` bodies alike.
    #[serde(default = "default_fmt_markup")]
    pub format_markup: FmtLevel,
    /// Column the markup formatter wraps at. 120 rather than 80 on measured
    /// grounds: at 80 the repo's templates grow 38% in line count and gain
    /// 447 dangling `>` lines, against 13% and 185 at 120.
    #[serde(default = "default_fmt_print_width")]
    pub print_width: usize,
}

impl Default for FmtConfig {
    fn default() -> Self {
        Self {
            inline_threshold: default_fmt_threshold(),
            inline_small_templates: default_fmt_inline(),
            extract_large_inline: default_fmt_extract(),
            format_markup: default_fmt_markup(),
            print_width: default_fmt_print_width(),
        }
    }
}

fn default_fmt_threshold() -> usize {
    150
}

fn default_fmt_inline() -> FmtLevel {
    FmtLevel::Fix
}

fn default_fmt_extract() -> FmtLevel {
    FmtLevel::Warn
}

fn default_fmt_markup() -> FmtLevel {
    FmtLevel::Fix
}

fn default_fmt_print_width() -> usize {
    120
}

fn default_sk_input() -> String {
    "app.css".into()
}

fn default_sk_preflight() -> bool {
    true
}

fn default_sk_enabled() -> bool {
    true
}

fn default_sk_output() -> String {
    "pkg/stylekit.css".into()
}

fn default_sk_src() -> String {
    "src".into()
}

/// `[package.metadata.pocopine.tailwind]` - configure the bundled
/// Tailwind build. All fields optional.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TailwindConfig {
    /// Entry CSS passed to `tailwindcss -i`. Defaults to `app.css` at
    /// the project root.
    #[serde(default = "default_tw_input")]
    pub input: String,
    /// Output CSS path passed to `tailwindcss -o`. Defaults to
    /// `pkg/tailwind.css` so it sits alongside the wasm bundle.
    #[serde(default = "default_tw_output")]
    pub output: String,
    /// Release tag on `tailwindlabs/tailwindcss` to download when the
    /// binary isn't on `$PATH`. Defaults to [`DEFAULT_TW_VERSION`]. Only
    /// consumed when pocopine-cli is built for a host target.
    #[allow(dead_code)]
    #[serde(default = "default_tw_version")]
    pub version: String,
    /// Explicit path to a Tailwind binary. When set, skips `$PATH`
    /// lookup and auto-download entirely.
    pub binary: Option<PathBuf>,
}

impl Default for TailwindConfig {
    fn default() -> Self {
        Self {
            input: default_tw_input(),
            output: default_tw_output(),
            version: default_tw_version(),
            binary: None,
        }
    }
}

fn default_tw_input() -> String {
    "app.css".into()
}

fn default_tw_output() -> String {
    "pkg/tailwind.css".into()
}

fn default_tw_version() -> String {
    DEFAULT_TW_VERSION.into()
}

/// Tailwind standalone CLI version used when no `version` override is
/// set in the project config. `"latest"` resolves via GitHub's
/// `/releases/latest/download/` redirect, so we pick up new releases
/// without a code change. Users who need a reproducible build can pin
/// a concrete tag like `"v4.1.2"` in their `Cargo.toml`.
pub const DEFAULT_TW_VERSION: &str = "latest";

pub fn load(path: &Path) -> Result<PocopineConfig> {
    let resolved = path
        .canonicalize()
        .with_context(|| format!("resolve project path: {}", path.display()))?;
    let manifest_path = resolved.join("Cargo.toml");
    if !manifest_path.exists() {
        bail!("no Cargo.toml at {}", manifest_path.display());
    }
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;

    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        package: Package,
    }
    #[derive(Default, Deserialize)]
    struct Package {
        #[serde(default)]
        metadata: Metadata,
    }
    #[derive(Default, Deserialize)]
    struct Metadata {
        #[serde(default)]
        pocopine: PocopineConfig,
    }

    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parse {}", manifest_path.display()))?;
    Ok(manifest.package.metadata.pocopine)
}
