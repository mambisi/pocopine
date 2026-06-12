//! RFC-100 — the asset write path: fingerprinting for rebuild
//! correctness and the hash-diff sync behind `pocopine assets push`.
//!
//! ```text
//! assets/blog/clip.webm ──┐
//! assets/logo.svg ────────┤ scan: sha256-prefix each file
//!                         ▼
//!            [LocalAsset { rel, hash, .. }]
//!                         │
//!        ┌────────────────┴───────────────────┐
//!        ▼                                    ▼
//!  fingerprint()                        push(): list bucket keys
//!  → POCOPINE_ASSETS_FINGERPRINT          under assets/, upload only
//!    on every cargo invocation            missing assets/<hash8>/<rel>
//!    (macro re-expansion)                 (content-type + immutable)
//! ```
//!
//! Both halves hash with `pocopine-crypto` and share the exact key
//! shape the `asset!` macro bakes into URLs (`assets/<hash8>/<rel>`),
//! so a successful push means every URL the app can emit resolves in
//! the bucket. Uploads happen **before** the deploy flip (RFC-100
//! §7): new keys land next to the old ones (content-addressed keys
//! never collide), the app flips, and stale keys are left in place
//! for still-cached clients.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pocopine_assets::{AssetStore, AssetStoreConfig, ASSET_CACHE_CONTROL};
use pocopine_deploy::credentials;

use crate::config::AssetsConfig;

/// Env var carrying the combined `assets/`-tree fingerprint into
/// cargo invocations. Must stay in lockstep with the constant of the
/// same name in `pocopine-macros/src/assets.rs` — the macro gates its
/// rebuild-tracking emission on this var being set.
pub(crate) const FINGERPRINT_ENV: &str = "POCOPINE_ASSETS_FINGERPRINT";

/// Bucket key prefix for content-addressed assets.
const KEY_PREFIX: &str = pocopine_assets::ASSET_KEY_PREFIX;

/// One file under the project's `assets/` directory, hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalAsset {
    /// Path relative to `assets/`, `/`-separated (the URL/key form).
    pub rel: String,
    /// 8-lowercase-hex sha256 prefix of the contents — the same hash
    /// the `asset!` macro bakes into URLs.
    pub hash: String,
    /// Absolute path on disk.
    pub abs: PathBuf,
    /// File size, for the push summary.
    pub len: u64,
}

impl LocalAsset {
    /// The content-addressed bucket key: `assets/<hash8>/<rel>`.
    pub fn key(&self) -> String {
        format!("{KEY_PREFIX}{}/{}", self.hash, self.rel)
    }
}

/// Compute the combined fingerprint of `<project>/assets`, or `None`
/// when the directory does not exist. The fingerprint hashes the
/// sorted relative paths together with each file's content hash, so
/// adding, removing, renaming, or editing any asset changes it.
///
/// `pocopine build`/`run`/`dev`/`deploy` export this as
/// [`FINGERPRINT_ENV`] on every cargo/wasm-pack invocation; `asset!`
/// emits an `option_env!` reference to it, which makes cargo
/// recompile (and therefore re-expand and re-hash) the calling crate
/// whenever the assets tree changes. See RFC-100 §6.
pub(crate) fn fingerprint(project: &Path) -> Result<Option<String>> {
    let assets_dir = project.join("assets");
    if !assets_dir.is_dir() {
        return Ok(None);
    }
    let assets = scan_assets(&assets_dir)?;
    let mut hasher = pocopine_crypto::Hasher::new(pocopine_crypto::Algorithm::Sha256);
    for asset in &assets {
        hasher.update(asset.rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(asset.hash.as_bytes());
        hasher.update(b"\n");
    }
    Ok(Some(hasher.finalize_hex()))
}

/// Set [`FINGERPRINT_ENV`] on `cmd` when the project has an `assets/`
/// directory. Called by every build path right before spawning
/// cargo/wasm-pack. Failures degrade to "no fingerprint" with a
/// warning rather than failing the build — the dev server's `409` is
/// the backstop.
pub(crate) fn apply_fingerprint_env(cmd: &mut std::process::Command, project: &Path) {
    match fingerprint(project) {
        Ok(Some(value)) => {
            cmd.env(FINGERPRINT_ENV, value);
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(
                target: "pocopine.log",
                error = %format!("{err:#}"),
                "could not fingerprint assets/; asset! rebuild tracking disarmed for this build"
            );
        }
    }
}

/// Walk `assets/` and hash every regular file. Results are sorted by
/// relative path so fingerprints and summaries are deterministic.
/// Dotfiles (`.DS_Store`, editor droppings) are skipped — they are
/// not addressable through `asset!` conventions and would churn the
/// fingerprint.
pub(crate) fn scan_assets(assets_dir: &Path) -> Result<Vec<LocalAsset>> {
    let mut out = Vec::new();
    walk(assets_dir, assets_dir, &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<LocalAsset>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            bail!(
                "asset path {} is not valid UTF-8 — asset paths become URLs and must be UTF-8",
                path.display()
            );
        };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, out)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .expect("walked path is under root")
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.push(LocalAsset {
            rel,
            hash: asset_hash_prefix(&bytes),
            abs: path,
            len: bytes.len() as u64,
        });
    }
    Ok(())
}

/// 8-hex-char content hash; same shape as
/// `pocopine_core::assets::asset_hash` (prefix of
/// `pocopine_crypto::sha256_hex`), duplicated because the CLI does
/// not link the wasm runtime crate. Parity with the macro/runtime is
/// pinned by `hash_prefix_matches_known_vectors` below and asserted
/// end-to-end in `crates/pocopine/tests/asset_macro.rs`.
pub(crate) fn asset_hash_prefix(bytes: &[u8]) -> String {
    let mut hex = pocopine_crypto::sha256_hex(bytes);
    hex.truncate(8);
    hex
}

/// Canonical MIME table for asset serving and sync. One table for
/// the dev server route, the bucket sync (`content-type` on upload),
/// and therefore — because the Mode B proxy echoes the stored
/// content type — every serve path.
pub(crate) fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "pdf" => "application/pdf",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Result of one push, for the summary line and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PushSummary {
    /// Keys uploaded this run.
    pub uploaded: usize,
    /// Keys already present (hash-identical content), skipped.
    pub skipped: usize,
    /// Bytes actually transferred.
    pub bytes: u64,
}

/// `pocopine assets push` — sync `<project>/assets` to the configured
/// bucket. Resolves credentials (env over `~/.pocopine/
/// credentials.toml`), ensures the bucket exists, lists existing
/// `assets/` keys once, and uploads only the content-addressed keys
/// that are missing. Prints a summary; returns it for the deploy
/// wiring's log line.
pub(crate) fn push(project: &Path, cfg: &AssetsConfig) -> Result<PushSummary> {
    let assets_dir = project.join("assets");
    if !assets_dir.is_dir() {
        bail!(
            "no `assets/` directory at {} — `[package.metadata.pocopine.assets]` is configured \
             but there is nothing to push",
            assets_dir.display()
        );
    }
    let local = scan_assets(&assets_dir)?;
    let creds = credentials::load_assets()?;
    let store = AssetStore::connect(AssetStoreConfig {
        endpoint: cfg.endpoint.clone(),
        bucket: cfg.bucket.clone(),
        region: cfg.region.clone(),
        access_key_id: creds.access_key_id,
        secret_access_key: creds.secret_access_key,
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting tokio runtime for asset sync")?;
    runtime.block_on(sync(&store, &local))
}

/// The sync algorithm against an already-connected store. Separated
/// from [`push`] so the MinIO integration test can drive it without
/// the credentials file.
pub(crate) async fn sync(store: &AssetStore, local: &[LocalAsset]) -> Result<PushSummary> {
    store
        .ensure_bucket()
        .await
        .with_context(|| format!("ensuring bucket `{}` exists", store.bucket()))?;
    let existing = store
        .list_keys(KEY_PREFIX)
        .await
        .with_context(|| format!("listing `{KEY_PREFIX}` keys in bucket `{}`", store.bucket()))?;

    let mut summary = PushSummary::default();
    for asset in local {
        let key = asset.key();
        if existing.contains(&key) {
            summary.skipped += 1;
            continue;
        }
        let bytes = std::fs::read(&asset.abs)
            .with_context(|| format!("reading {}", asset.abs.display()))?;
        let content_type = mime_of(&asset.abs);
        store
            .put(&key, bytes, content_type, ASSET_CACHE_CONTROL)
            .await
            .with_context(|| format!("uploading `{key}`"))?;
        tracing::info!(
            target: "pocopine.log",
            key = %key,
            content_type,
            bytes = asset.len,
            "uploaded asset"
        );
        println!("  ↑ {key} ({})", human_bytes(asset.len));
        summary.uploaded += 1;
        summary.bytes += asset.len;
    }
    println!(
        "✓ assets push: {} uploaded, {} skipped (already content-addressed), {} transferred",
        summary.uploaded,
        summary.skipped,
        human_bytes(summary.bytes)
    );
    tracing::info!(
        target: "pocopine.log",
        uploaded = summary.uploaded,
        skipped = summary.skipped,
        bytes = summary.bytes,
        bucket = %store.bucket(),
        "assets push complete"
    );
    Ok(summary)
}

/// `pocopine assets auth` — prompt for the bucket access keys and
/// persist them to `~/.pocopine/credentials.toml` (mode 0600),
/// mirroring `pocopine deploy auth <host>`.
pub(crate) fn auth() -> Result<()> {
    eprintln!("Paste the S3 access keys for the asset bucket.");
    eprintln!("(Railway: Bucket → Connect; R2: dashboard → R2 API tokens; AWS: IAM keys)");
    eprintln!();
    let access_key_id = prompt("access key id")?;
    let secret_access_key = prompt("secret access key")?;
    credentials::store_assets(&credentials::AssetsCredentials {
        access_key_id,
        secret_access_key,
    })?;
    eprintln!("Stored asset keys to ~/.pocopine/credentials.toml (mode 0600).");
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    eprint!("{label}: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .with_context(|| format!("reading {label} from stdin"))?;
    let value = line.trim();
    if value.is_empty() {
        bail!("empty {label}");
    }
    Ok(value.to_string())
}

/// Resolve the `[package.metadata.pocopine.assets]` block, with a
/// pointed error when it is missing (push without config is always a
/// user mistake worth a precise message).
pub(crate) fn require_config(cfg: &crate::config::PocopineConfig) -> Result<&AssetsConfig> {
    cfg.assets.as_ref().context(
        "no `[package.metadata.pocopine.assets]` in Cargo.toml — declare at least `bucket` \
         (plus `endpoint` for non-AWS stores) to use the asset pipeline. See RFC 100 §5.",
    )
}

/// `1234` → `1.2 KiB` etc. Coarse on purpose; this is a summary line.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, bytes: &[u8]) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn hash_prefix_matches_known_vectors() {
        // Known vectors mirrored in `pocopine_core::assets::tests` and
        // `pocopine_macros::assets::tests` — the three implementations
        // must stay in lockstep (RFC-100 §11 open question tracks
        // hoisting them into one shared crate).
        assert_eq!(asset_hash_prefix(b""), "e3b0c442");
        assert_eq!(asset_hash_prefix(b"hello world"), "b94d27b9");
    }

    #[test]
    fn scan_assets_sorts_skips_dotfiles_and_builds_keys() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "logo.svg", b"hello world");
        write(dir.path(), "blog/clip.webm", b"hello world");
        write(dir.path(), ".DS_Store", b"junk");
        write(dir.path(), "blog/.hidden", b"junk");

        let assets = scan_assets(dir.path()).unwrap();
        let rels: Vec<&str> = assets.iter().map(|a| a.rel.as_str()).collect();
        assert_eq!(rels, ["blog/clip.webm", "logo.svg"]);
        assert_eq!(assets[0].key(), "assets/b94d27b9/blog/clip.webm");
        assert_eq!(assets[1].key(), "assets/b94d27b9/logo.svg");
    }

    #[test]
    fn fingerprint_none_without_assets_dir_and_changes_on_edit() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(fingerprint(dir.path()).unwrap(), None);

        write(&dir.path().join("assets"), "logo.svg", b"v1");
        let first = fingerprint(dir.path()).unwrap().unwrap();

        // Same tree → same fingerprint (determinism).
        assert_eq!(fingerprint(dir.path()).unwrap().unwrap(), first);

        // Content edit → new fingerprint.
        write(&dir.path().join("assets"), "logo.svg", b"v2");
        let second = fingerprint(dir.path()).unwrap().unwrap();
        assert_ne!(second, first);

        // Added file → new fingerprint.
        write(&dir.path().join("assets"), "new.txt", b"v1");
        let third = fingerprint(dir.path()).unwrap().unwrap();
        assert_ne!(third, second);

        // Rename (same content, different path) → new fingerprint.
        std::fs::rename(
            dir.path().join("assets/new.txt"),
            dir.path().join("assets/renamed.txt"),
        )
        .unwrap();
        assert_ne!(fingerprint(dir.path()).unwrap().unwrap(), third);
    }

    #[test]
    fn mime_table_covers_asset_types() {
        assert_eq!(mime_of(Path::new("a/clip.webm")), "video/webm");
        assert_eq!(mime_of(Path::new("a/clip.mp4")), "video/mp4");
        assert_eq!(mime_of(Path::new("a/logo.svg")), "image/svg+xml");
        assert_eq!(mime_of(Path::new("a/pic.webp")), "image/webp");
        assert_eq!(mime_of(Path::new("a/font.woff2")), "font/woff2");
        assert_eq!(
            mime_of(Path::new("a/unknown.zzz")),
            "application/octet-stream"
        );
    }

    #[test]
    fn human_bytes_picks_sane_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
