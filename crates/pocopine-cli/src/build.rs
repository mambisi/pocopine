use std::path::Path;
use std::process::Output;

use anyhow::{bail, Context, Result};

use crate::config::PocopineConfig;
use crate::tools;

pub fn wasm(path: &Path, release: bool) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("could not resolve project path: {}", path.display()))?;
    println!("▶ wasm-pack build ({})", path.display());
    let project_tools = tools::ProjectTools::load(&path)?;
    let mut cmd = project_tools.wasm_pack().command();
    cmd.arg("build").arg("--target").arg("web");
    if release {
        cmd.arg("--release");
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
/// * `index.html`'s `pkg/<name>.js` (or a previous `pkg/<name>.<hash8>
///   .js`) script reference is rewritten in place.
///
/// A new bundle is a new URL pair; servers mark the hashed names
/// `immutable` and HTML `no-cache` (see `server::handle` for the dev
/// server and `pocopine_server::static_files` for production), so the
/// only revalidated fetch is the tiny index.html. The non-runtime
/// wasm-pack artefacts (`.d.ts`, `package.json`) keep their unhashed
/// names — they reference the pair for tooling only.
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

        rewrite_index_html(project, &name, &hash)?;
        println!("✓ hashed bundle pair: pkg/{hashed_js} + pkg/{hashed_wasm}");
    }
    Ok(())
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

/// Point `index.html`'s script reference at the freshly hashed glue.
/// Idempotent: matches both the unhashed `pkg/<name>.js` (first build,
/// raw wasm-pack residue) and a previously hashed `pkg/<name>.<hash8>.js`.
fn rewrite_index_html(project: &Path, name: &str, hash: &str) -> Result<()> {
    let path = project.join("index.html");
    let Ok(html) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let rewritten = rewrite_bundle_refs(&html, name, hash);
    if rewritten != html {
        std::fs::write(&path, rewritten).context("rewrite index.html bundle reference")?;
        println!("✓ index.html → pkg/{name}.{hash}.js");
    }
    Ok(())
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
    if let Some(rest) = tail.strip_prefix("js") {
        if boundary(rest) {
            return Some(2);
        }
    }
    if tail.is_char_boundary(8) && crate::server::is_asset_hash(&tail[..8]) {
        if let Some(rest) = tail[8..].strip_prefix(".js") {
            if boundary(rest) {
                return Some(11);
            }
        }
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
    if let Some(worker) = cfg.worker_bin.as_deref() {
        if !bins.contains(&worker) {
            bins.push(worker);
        }
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
    fn hash_pkg_bundle_renames_pair_rewrites_glue_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir(project.join("pkg")).unwrap();
        std::fs::write(
            project.join("pkg/app.js"),
            "input = new URL('app_bg.wasm', import.meta.url);",
        )
        .unwrap();
        std::fs::write(project.join("pkg/app_bg.wasm"), b"wasm-bytes").unwrap();
        // Leftovers from a previous hashed build must be removed.
        std::fs::write(project.join("pkg/app.deadbeef.js"), "old").unwrap();
        std::fs::write(project.join("pkg/app_bg.deadbeef.wasm"), "old").unwrap();
        std::fs::write(
            project.join("index.html"),
            r#"<script type="module">import init from "/pkg/app.deadbeef.js";</script>"#,
        )
        .unwrap();

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

        // index.html re-pointed at the fresh glue.
        let html = std::fs::read_to_string(project.join("index.html")).unwrap();
        assert!(html.contains(&format!("/pkg/app.{HASH}.js")));
    }

    #[test]
    fn hash_pkg_bundle_is_a_no_op_without_a_fresh_pair() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir(project.join("pkg")).unwrap();
        // Already-hashed output only (e.g. `pocopine run --skip-build`
        // style reuse) — nothing to do, nothing removed.
        std::fs::write(project.join(format!("pkg/app.{HASH}.js")), "glue").unwrap();
        std::fs::write(project.join(format!("pkg/app_bg.{HASH}.wasm")), "wasm").unwrap();

        hash_pkg_bundle(project).unwrap();

        assert!(project.join(format!("pkg/app.{HASH}.js")).is_file());
        assert!(project.join(format!("pkg/app_bg.{HASH}.wasm")).is_file());
    }
}
