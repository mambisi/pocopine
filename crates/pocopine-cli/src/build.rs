use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Output;

use anyhow::{Context, Result, bail};

use crate::config::PocopineConfig;
use crate::tools;

// `wasm-pack` mutates a shared `pkg/` in several non-atomic steps. In
// particular it removes `package.json` near the start, then interprets any
// file that reappears before its final manifest step as a wasm-bindgen NPM
// dependency map. An overlapping build can write the normal manifest in that
// window, making wasm-pack deserialize `"files": [...]` as a String. Keep the
// lock outside `pkg/` because wasm-pack owns that directory, and hold it until
// our post-build hashing has finished renaming the generated pair too.
const WASM_BUILD_LOCK_FILE: &str = ".pocopine-wasm-build.lock";

enum WasmBuildLockAttempt {
    Acquired(File),
    Contended(File),
}

pub fn wasm(path: &Path, release: bool) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("could not resolve project path: {}", path.display()))?;
    with_wasm_build_lock(&path, || {
        println!("▶ wasm-pack build ({})", path.display());
        let project_tools = tools::ProjectTools::load(&path)?;
        let mut cmd = project_tools.wasm_pack().command();
        cmd.arg("build").arg("--target").arg("web");
        if release {
            // A project may name its own profile so the wasm can be tuned for
            // size without dragging the server build with it; see
            // `PocopineConfig::wasm_profile`.
            match crate::config::load(&path).ok().and_then(|c| c.wasm_profile) {
                Some(profile) if profile != "release" => {
                    cmd.arg("--profile").arg(profile);
                }
                _ => {
                    cmd.arg("--release");
                }
            }
        } else {
            cmd.arg("--dev");
        }
        cmd.current_dir(&path);
        // RFC-100 §6 — export the assets/ fingerprint so `asset!`
        // re-expands (and re-hashes) when the assets tree changed.
        crate::assets_sync::apply_fingerprint_env(&mut cmd, &path);
        let status = cmd
            .status()
            .context("failed to invoke wasm-pack (is it on $PATH?)")?;
        if !status.success() {
            bail!("wasm-pack build failed with status {status}");
        }
        hash_pkg_bundle(&path)?;
        Ok(())
    })
}

fn with_wasm_build_lock<T>(project: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let _lock = acquire_wasm_build_lock(project)?;
    operation()
}

fn acquire_wasm_build_lock(project: &Path) -> Result<File> {
    match try_wasm_build_lock(project)? {
        WasmBuildLockAttempt::Acquired(file) => Ok(file),
        WasmBuildLockAttempt::Contended(file) => {
            println!("⏳ waiting for another wasm build ({})", project.display());
            file.lock().with_context(|| {
                format!(
                    "wait for wasm build lock: {}",
                    wasm_build_lock_path(project).display()
                )
            })?;
            Ok(file)
        }
    }
}

fn try_wasm_build_lock(project: &Path) -> Result<WasmBuildLockAttempt> {
    let lock_path = wasm_build_lock_path(project);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create wasm build lock directory: {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open wasm build lock: {}", lock_path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(WasmBuildLockAttempt::Acquired(file)),
        Err(TryLockError::WouldBlock) => Ok(WasmBuildLockAttempt::Contended(file)),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("lock wasm build: {}", lock_path.display()))
        }
    }
}

fn wasm_build_lock_path(project: &Path) -> std::path::PathBuf {
    project.join("target").join(WASM_BUILD_LOCK_FILE)
}

/// Content-hash the wasm-pack output pair so the JS glue and the wasm
/// it instantiates can never skew in HTTP caches.
///
/// Browsers and CDNs cache `pkg/<name>.js` and `pkg/<name>_bg.wasm`
/// independently (Cloudflare's default rules edge-cache `.js` for
/// hours but pass `.wasm` through), so after a deploy a stale cached
/// glue can pair with a fresh wasm — `WebAssembly.instantiate` then
/// fails with `LinkError: … function import requires a callable`.
///
/// Renaming BOTH files with one hash — the wasm's 8-hex sha256 prefix,
/// the shared asset-hash shape — versions them atomically:
///
/// * `pkg/<name>.js`       → `pkg/<name>.<hash8>.js` (with its
///   internal `<name>_bg.wasm` URL rewritten to the hashed name)
/// * `pkg/<name>_bg.wasm`  → `pkg/<name>_bg.<hash8>.wasm`
/// * a copy of the project's `index.html` with its `pkg/<name>.js`
///   script reference re-pointed at the hashed glue is written to
///   `pkg/index.html` — the SOURCE `index.html` is never touched
///   (it keeps the stable unhashed reference; the hash is build
///   output and lives with the build output, gitignored).
///
/// Servers prefer the generated `pkg/index.html` over the source one
/// when it exists (see `server::handle` for the dev server and
/// `pocopine_server::static_files` for production), so a checkout
/// without a build still serves the source file.
///
/// A new bundle is a new URL pair; servers mark the hashed names
/// `immutable` and HTML `no-cache`, so the only revalidated fetch is
/// the tiny index.html. The non-runtime wasm-pack artefacts (`.d.ts`,
/// `package.json`) keep their unhashed names — they reference the
/// pair for tooling only.
fn hash_pkg_bundle(project: &Path) -> Result<()> {
    let pkg = project.join("pkg");
    let Ok(entries) = std::fs::read_dir(&pkg) else {
        return Ok(());
    };

    // Fresh wasm-pack output: `<name>_bg.wasm` with a `<name>.js`
    // sibling. Already-hashed pairs from a previous build don't match
    // the `_bg.wasm` suffix and are cleaned up per-name below.
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(file) = file.to_str() else { continue };
        let Some(name) = file.strip_suffix("_bg.wasm") else {
            continue;
        };
        if pkg.join(format!("{name}.js")).is_file() {
            names.push(name.to_string());
        }
    }

    let mut hashed = Vec::new();
    for name in names {
        let wasm_path = pkg.join(format!("{name}_bg.wasm"));
        let js_path = pkg.join(format!("{name}.js"));
        let bytes =
            std::fs::read(&wasm_path).with_context(|| format!("read {}", wasm_path.display()))?;
        let mut hash = pocopine_crypto::sha256_hex(&bytes);
        hash.truncate(8);

        remove_stale_hashed_pair(&pkg, &name);

        let hashed_js = format!("{name}.{hash}.js");
        let hashed_wasm = format!("{name}_bg.{hash}.wasm");
        let glue = std::fs::read_to_string(&js_path)
            .with_context(|| format!("read {}", js_path.display()))?;
        // The glue resolves the module relative to itself
        // (`new URL('<name>_bg.wasm', import.meta.url)`); every
        // occurrence moves to the hashed name so the pair stays
        // self-consistent.
        let glue = glue.replace(&format!("{name}_bg.wasm"), &hashed_wasm);
        std::fs::write(pkg.join(&hashed_js), glue)
            .with_context(|| format!("write pkg/{hashed_js}"))?;
        std::fs::remove_file(&js_path).with_context(|| format!("remove {}", js_path.display()))?;
        std::fs::rename(&wasm_path, pkg.join(&hashed_wasm))
            .with_context(|| format!("rename to pkg/{hashed_wasm}"))?;

        println!("✓ hashed bundle pair: pkg/{hashed_js} + pkg/{hashed_wasm}");
        precompress(&pkg.join(&hashed_wasm))?;
        precompress(&pkg.join(&hashed_js))?;
        hashed.push((name, hash));
    }
    write_pkg_index_html(project, &hashed)?;
    Ok(())
}

/// Write `.br` and `.gz` siblings so the server can serve a compressed
/// bundle without compressing it again on every request.
///
/// The alternative — compressing at the edge — costs CPU per cold fetch
/// and, on a multi-megabyte wasm module, becomes the download bottleneck
/// itself: nginx emits brotli at roughly 2 MB/s, which caps a client
/// that could otherwise pull far faster. Doing it here happens once per
/// build, so the maximum brotli level is free rather than something to
/// trade away against latency.
///
/// Only the hashed pair is compressed. Those two files are immutable
/// under their URL, so a stale sibling is impossible; compressing the
/// whole tree would risk one.
fn precompress(path: &Path) -> Result<()> {
    use std::io::Write as _;

    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;

    // Level 11 with a 24-bit window: the largest ratio brotli offers.
    // Affordable precisely because this is not on the request path.
    let mut brotli_out = Vec::new();
    brotli::BrotliCompress(
        &mut bytes.as_slice(),
        &mut brotli_out,
        &brotli::enc::BrotliEncoderParams {
            quality: 11,
            lgwin: 24,
            size_hint: bytes.len(),
            ..Default::default()
        },
    )
    .with_context(|| format!("brotli-compress {}", path.display()))?;
    let brotli_path = append_extension(path, "br");
    std::fs::write(&brotli_path, &brotli_out)
        .with_context(|| format!("write {}", brotli_path.display()))?;

    // gzip as well: a client that cannot take brotli — an old browser,
    // or a proxy that strips the header — should still not be handed
    // the raw module.
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder
        .write_all(&bytes)
        .and_then(|()| encoder.finish())
        .map(|gzip_out| -> Result<()> {
            let gzip_path = append_extension(path, "gz");
            std::fs::write(&gzip_path, &gzip_out)
                .with_context(|| format!("write {}", gzip_path.display()))?;
            println!(
                "✓ precompressed {}: {} → {} br / {} gz",
                path.file_name().unwrap_or_default().to_string_lossy(),
                human_bytes(bytes.len()),
                human_bytes(brotli_out.len()),
                human_bytes(gzip_out.len()),
            );
            Ok(())
        })
        .with_context(|| format!("gzip-compress {}", path.display()))??;
    Ok(())
}

/// `foo.wasm` + `br` → `foo.wasm.br`. `Path::set_extension` would
/// replace `wasm`, breaking the name the server looks for.
fn append_extension(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

fn human_bytes(bytes: usize) -> String {
    #[expect(clippy::cast_precision_loss, reason = "display only")]
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 1.0 {
        format!("{mib:.2} MiB")
    } else {
        #[expect(clippy::cast_precision_loss, reason = "display only")]
        let kib = bytes as f64 / 1024.0;
        format!("{kib:.0} KiB")
    }
}

/// Drop hashed `<name>.<hash8>.js` / `<name>_bg.<hash8>.wasm` leftovers
/// from previous builds so `pkg/` holds exactly one pair per bundle.
fn remove_stale_hashed_pair(pkg: &Path, name: &str) {
    let Ok(entries) = std::fs::read_dir(pkg) else {
        return;
    };
    let js_prefix = format!("{name}.");
    let wasm_prefix = format!("{name}_bg.");
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(file) = file.to_str() else { continue };
        let stale = is_hashed_variant(file, &js_prefix, ".js")
            || is_hashed_variant(file, &wasm_prefix, ".wasm");
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `website.0a1b2c3d.js` ⊢ (`website.`, `.js`) → true.
fn is_hashed_variant(file: &str, prefix: &str, suffix: &str) -> bool {
    file.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .is_some_and(crate::server::is_asset_hash)
}

/// Write `pkg/index.html` — a copy of the project's source
/// `index.html` with every bundle script reference re-pointed at the
/// freshly hashed glue. The source file is read-only input here: it
/// keeps the stable unhashed `pkg/<name>.js` reference (no git churn,
/// no stale hash committed), while the generated copy lives next to
/// the pair it names and is regenerated by every build.
///
/// Tolerates a hashed reference in the source too (`pkg/<name>.
/// <hash8>.js`, e.g. a checkout that predates the generated copy).
/// No fresh pair (`hashed` empty) → nothing written: a previous
/// build's `pkg/index.html` still names the pair sitting in `pkg/`.
fn write_pkg_index_html(project: &Path, hashed: &[(String, String)]) -> Result<()> {
    if hashed.is_empty() {
        return Ok(());
    }
    let source = project.join("index.html");
    let Ok(mut html) = std::fs::read_to_string(&source) else {
        return Ok(());
    };
    for (name, hash) in hashed {
        html = rewrite_bundle_refs(&html, name, hash);
    }
    let loader = crate::config::load(project)
        .ok()
        .and_then(|c| c.loader)
        .unwrap_or_default();
    let suffix = if loader.enabled {
        html = inject_loader(&html, &loader);
        " + boot loader"
    } else {
        ""
    };
    std::fs::write(project.join("pkg/index.html"), html)
        .context("write generated pkg/index.html")?;
    println!("✓ pkg/index.html → hashed bundle reference(s){suffix}");
    Ok(())
}

/// Inject the boot loader: the splash + top-bar styles into `<head>`, the
/// full-screen logo splash into `<body>` (so it paints on the first frame),
/// and the controller script. The script is classic (not a module) so it
/// runs before the app's deferred module boot and can wrap `fetch` to
/// report the wasm download as progress. No-op if already injected.
fn inject_loader(html: &str, loader: &crate::config::LoaderConfig) -> String {
    if html.contains("id=\"pp-loader-style\"") {
        return html.to_string();
    }
    const CSS: &str = include_str!("../assets/loader.css");
    const JS: &str = include_str!("../assets/loader.js");
    let mut out = html.to_string();

    if let Some(at) = out.find("</head>") {
        let mut head = format!("  <style id=\"pp-loader-style\">\n{CSS}  </style>\n");
        if loader.splash {
            // Show the splash once per tab session. This runs before <body>
            // is parsed, so on a reload / full-page navigation within the
            // session it hides the splash with no flash (the bar still shows).
            head.push_str("  <script>(function(){try{var k=\"pp-splash-shown\";if(sessionStorage.getItem(k)){document.documentElement.setAttribute(\"data-pp-no-splash\",\"\");}else{sessionStorage.setItem(k,\"1\");}}catch(e){}})();</script>\n");
        }
        out.insert_str(at, &head);
    }
    if loader.splash
        && let Some(logo) = loader.logo.clone().or_else(|| icon_href(html))
    {
        let splash = format!(
            "\n  <div id=\"pp-splash\"><img class=\"pp-splash__logo\" src=\"{logo}\" alt=\"\" /></div>\n"
        );
        if let Some(start) = out.find("<body")
            && let Some(gt) = out[start..].find('>')
        {
            out.insert_str(start + gt + 1, &splash);
        }
    }
    if let Some(at) = out.find("</body>") {
        out.insert_str(at, &format!("  <script>\n{JS}  </script>\n"));
    }
    out
}

/// Best-effort `<link rel="icon" … href="X">` href — the splash logo when
/// none is configured explicitly.
fn icon_href(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("rel=\"icon\"") {
        let abs = from + rel;
        let tag_start = lower[..abs].rfind('<').unwrap_or(abs);
        let tag_end = lower[abs..].find('>').map_or(html.len(), |e| abs + e);
        let tag = &html[tag_start..tag_end];
        if let Some(h) = tag.find("href=\"") {
            let rest = &tag[h + 6..];
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_string());
            }
        }
        from = tag_end;
    }
    None
}

/// Replace every `pkg/<name>.js` / `pkg/<name>.<hash8>.js` occurrence
/// with `pkg/<name>.<hash>.js`. Other `pkg/<name>.*` references
/// (e.g. `pkg/<name>.json`, `pkg/<name>.d.ts`) are left alone.
fn rewrite_bundle_refs(html: &str, name: &str, hash: &str) -> String {
    let needle = format!("pkg/{name}.");
    let replacement = format!("pkg/{name}.{hash}.js");
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find(&needle) {
        let tail = &rest[at + needle.len()..];
        match bundle_js_ref_len(tail) {
            Some(len) => {
                out.push_str(&rest[..at]);
                out.push_str(&replacement);
                rest = &tail[len..];
            }
            None => {
                out.push_str(&rest[..at + needle.len()]);
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Length of the script-reference tail right after `pkg/<name>.`:
/// `js` (2) or `<hash8>.js` (11), each followed by a non-identifier
/// boundary so `pkg/<name>.json` or `…js.map` never match.
fn bundle_js_ref_len(tail: &str) -> Option<usize> {
    fn boundary(rest: &str) -> bool {
        rest.chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '.')
    }
    if let Some(rest) = tail.strip_prefix("js")
        && boundary(rest)
    {
        return Some(2);
    }
    if tail.is_char_boundary(8)
        && crate::server::is_asset_hash(&tail[..8])
        && let Some(rest) = tail[8..].strip_prefix(".js")
        && boundary(rest)
    {
        return Some(11);
    }
    None
}

pub fn configured_bins(path: &Path, cfg: &PocopineConfig, release: bool) -> Result<()> {
    for bin in configured_bin_names(cfg) {
        build_bin(path, bin, release)?;
    }
    Ok(())
}

fn configured_bin_names(cfg: &PocopineConfig) -> Vec<&str> {
    let mut bins = Vec::new();
    if let Some(bin) = cfg.bin.as_deref() {
        bins.push(bin);
    }
    if let Some(worker) = cfg.worker_bin.as_deref()
        && !bins.contains(&worker)
    {
        bins.push(worker);
    }
    bins
}

fn build_bin(path: &Path, bin: &str, release: bool) -> Result<()> {
    let project = path
        .canonicalize()
        .with_context(|| format!("resolve {}", path.display()))?;
    let project_tools = tools::ProjectTools::load(&project)?;
    let mut cmd = project_tools.cargo().command();
    cmd.arg("build").arg("--bin").arg(bin);
    if release {
        cmd.arg("--release");
    }
    cmd.current_dir(&project);
    // RFC-100 §6 — same fingerprint export as the wasm build; server
    // bins can call `asset!` too.
    crate::assets_sync::apply_fingerprint_env(&mut cmd, &project);
    println!("▶ building `{bin}`");
    let output = cmd
        .output()
        .with_context(|| format!("failed to build configured bin `{bin}`"))?;
    if !output.status.success() {
        print_captured_output(&output);
        bail!(
            "configured bin `{bin}` failed to build with {}",
            output.status
        );
    }
    Ok(())
}

fn print_captured_output(output: &Output) {
    if !output.stdout.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // sha256("wasm-bytes") starts with 7db53183.
    const HASH: &str = "7db53183";

    #[test]
    fn wasm_build_lock_serializes_and_releases_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();

        let value = with_wasm_build_lock(project, || {
            assert!(matches!(
                try_wasm_build_lock(project).unwrap(),
                WasmBuildLockAttempt::Contended(_)
            ));
            Ok(42)
        })
        .unwrap();

        assert_eq!(value, 42);
        assert!(matches!(
            try_wasm_build_lock(project).unwrap(),
            WasmBuildLockAttempt::Acquired(_)
        ));
    }

    #[test]
    fn wasm_build_lock_releases_after_failure() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();

        let result: Result<()> = with_wasm_build_lock(project, || bail!("sentinel failure"));

        assert_eq!(result.unwrap_err().to_string(), "sentinel failure");
        assert!(matches!(
            try_wasm_build_lock(project).unwrap(),
            WasmBuildLockAttempt::Acquired(_)
        ));
    }

    #[test]
    fn wasm_build_lock_is_scoped_to_one_project() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        with_wasm_build_lock(first.path(), || {
            assert!(matches!(
                try_wasm_build_lock(second.path()).unwrap(),
                WasmBuildLockAttempt::Acquired(_)
            ));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn rewrite_bundle_refs_handles_fresh_and_rehashed_references() {
        // Fresh (unhashed) reference, absolute and relative forms.
        assert_eq!(
            rewrite_bundle_refs(r#"import init from "/pkg/website.js";"#, "website", HASH),
            format!(r#"import init from "/pkg/website.{HASH}.js";"#)
        );
        assert_eq!(
            rewrite_bundle_refs(r#"src="./pkg/website.js""#, "website", HASH),
            format!(r#"src="./pkg/website.{HASH}.js""#)
        );
        // Previously hashed reference is re-pointed (idempotent build).
        assert_eq!(
            rewrite_bundle_refs(r#""/pkg/website.deadbeef.js""#, "website", HASH),
            format!(r#""/pkg/website.{HASH}.js""#)
        );
        // Same hash → no change.
        let current = format!(r#""/pkg/website.{HASH}.js""#);
        assert_eq!(rewrite_bundle_refs(&current, "website", HASH), current);
    }

    #[test]
    fn rewrite_bundle_refs_leaves_other_pkg_files_alone() {
        for untouched in [
            r#""/pkg/website.json""#,
            r#""/pkg/website.d.ts""#,
            r#""/pkg/website.js.map""#,
            r#""/pkg/website.DEADBEEF.js""#,
            r#""/pkg/other.js""#,
        ] {
            assert_eq!(
                rewrite_bundle_refs(untouched, "website", HASH),
                untouched,
                "should not rewrite {untouched}"
            );
        }
    }

    #[test]
    fn hash_pkg_bundle_renames_pair_rewrites_glue_and_emits_pkg_index() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir(project.join("pkg")).unwrap();
        std::fs::write(
            project.join("pkg/app.js"),
            "input = new URL('app_bg.wasm', import.meta.url);",
        )
        .unwrap();
        std::fs::write(project.join("pkg/app_bg.wasm"), b"wasm-bytes").unwrap();
        // Leftovers from a previous hashed build must be removed —
        // including the generated index, which gets regenerated.
        std::fs::write(project.join("pkg/app.deadbeef.js"), "old").unwrap();
        std::fs::write(project.join("pkg/app_bg.deadbeef.wasm"), "old").unwrap();
        std::fs::write(project.join("pkg/index.html"), "stale generated copy").unwrap();
        let source_html =
            r#"<script type="module">import init from "/pkg/app.js";</script>"#.to_string();
        std::fs::write(project.join("index.html"), &source_html).unwrap();

        hash_pkg_bundle(project).unwrap();

        // Pair renamed under the wasm's hash; unhashed names gone.
        assert!(project.join(format!("pkg/app.{HASH}.js")).is_file());
        assert!(project.join(format!("pkg/app_bg.{HASH}.wasm")).is_file());
        assert!(!project.join("pkg/app.js").exists());
        assert!(!project.join("pkg/app_bg.wasm").exists());
        assert!(!project.join("pkg/app.deadbeef.js").exists());
        assert!(!project.join("pkg/app_bg.deadbeef.wasm").exists());

        // Glue points at the hashed wasm.
        let glue = std::fs::read_to_string(project.join(format!("pkg/app.{HASH}.js"))).unwrap();
        assert!(glue.contains(&format!("app_bg.{HASH}.wasm")));
        assert!(!glue.contains("app_bg.wasm',"));

        // The SOURCE index.html is never touched — the hash lives in
        // the generated pkg/index.html next to the pair it names.
        assert_eq!(
            std::fs::read_to_string(project.join("index.html")).unwrap(),
            source_html
        );
        let generated = std::fs::read_to_string(project.join("pkg/index.html")).unwrap();
        assert!(generated.contains(&format!("/pkg/app.{HASH}.js")));
        assert!(!generated.contains("/pkg/app.js"));
    }

    #[test]
    fn hash_pkg_bundle_tolerates_a_hashed_reference_in_the_source_index() {
        // A checkout from before pkg/index.html existed may still
        // carry a hashed reference in the source file; the generated
        // copy re-points it instead of leaving it stale.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir(project.join("pkg")).unwrap();
        std::fs::write(project.join("pkg/app.js"), "glue").unwrap();
        std::fs::write(project.join("pkg/app_bg.wasm"), b"wasm-bytes").unwrap();
        std::fs::write(
            project.join("index.html"),
            r#"<script type="module">import init from "/pkg/app.deadbeef.js";</script>"#,
        )
        .unwrap();

        hash_pkg_bundle(project).unwrap();

        let generated = std::fs::read_to_string(project.join("pkg/index.html")).unwrap();
        assert!(generated.contains(&format!("/pkg/app.{HASH}.js")));
        assert!(!generated.contains("deadbeef"));
    }

    #[test]
    fn hash_pkg_bundle_is_a_no_op_without_a_fresh_pair() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir(project.join("pkg")).unwrap();
        // Already-hashed output only (e.g. `pocopine run --skip-build`
        // style reuse) — nothing to do, nothing removed, and the
        // previous build's generated index keeps naming that pair.
        std::fs::write(project.join(format!("pkg/app.{HASH}.js")), "glue").unwrap();
        std::fs::write(project.join(format!("pkg/app_bg.{HASH}.wasm")), "wasm").unwrap();
        let generated = format!(r#"import init from "/pkg/app.{HASH}.js";"#);
        std::fs::write(project.join("pkg/index.html"), &generated).unwrap();
        std::fs::write(
            project.join("index.html"),
            r#"import init from "/pkg/app.js";"#,
        )
        .unwrap();

        hash_pkg_bundle(project).unwrap();

        assert!(project.join(format!("pkg/app.{HASH}.js")).is_file());
        assert!(project.join(format!("pkg/app_bg.{HASH}.wasm")).is_file());
        assert_eq!(
            std::fs::read_to_string(project.join("pkg/index.html")).unwrap(),
            generated
        );
    }
}
