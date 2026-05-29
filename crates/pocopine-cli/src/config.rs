use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
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
    /// Opt into bundled Tailwind. When present, `pocopine-cli` runs
    /// the Tailwind standalone CLI alongside `wasm-pack` - one-shot
    /// on `build`/`run`, watch mode on `dev`.
    pub tailwind: Option<TailwindConfig>,
    /// Opt into Pine Stylekit (RFC 092). When present — or when
    /// `--stylekit` is passed — `pocopine-cli` compiles utility CSS
    /// from `.poco` sources in-process, no external watcher.
    pub stylekit: Option<StylekitConfig>,
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
}

impl Default for StylekitConfig {
    fn default() -> Self {
        Self {
            input: default_sk_input(),
            output: default_sk_output(),
            src: default_sk_src(),
            preflight: default_sk_preflight(),
        }
    }
}

fn default_sk_input() -> String {
    "app.css".into()
}

fn default_sk_preflight() -> bool {
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
