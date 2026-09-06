use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, anyhow, bail};
use pocopine_locale::{
    CATALOG_FORMAT_VERSION, CatalogAudience, LocaleConfig, LocaleManifest,
    server::{CfgSet, ProjectDiscovery, SourceTarget, discover_project, generate_rust},
};
use serde::Deserialize;

use super::Prepared;
use crate::tools::ProjectTools;

const BUILD_INFO: &str = "target/pocopine/locale/build.json";

pub(super) fn shell_payload(prepared: &Prepared) -> Result<serde_json::Value> {
    let locales = prepared.manifest.config.validate()?;
    Ok(serde_json::json!({
        "manifest": prepared.manifest,
        "fallbacks": pocopine_locale::server::preload_fallbacks(&locales),
    }))
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}
#[derive(Deserialize)]
struct Package {
    manifest_path: PathBuf,
    targets: Vec<Target>,
    dependencies: Vec<Dependency>,
}
#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
    #[serde(default, rename = "required-features")]
    required_features: Vec<String>,
}
#[derive(Deserialize)]
struct Dependency {
    name: String,
    rename: Option<String>,
    kind: Option<String>,
}

fn config(project: &Path) -> Result<Option<LocaleConfig>> {
    #[derive(Deserialize)]
    struct File {
        locale: Option<LocaleConfig>,
    }
    let path = project.join("pocopine.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let parsed: File = toml::from_str(&text).context("parse locale configuration")?;
    if let Some(config) = &parsed.locale {
        config.validate()?;
    }
    Ok(parsed.locale)
}

fn output(command: &mut Command, what: &str) -> Result<String> {
    let output = command.output().with_context(|| format!("run {what}"))?;
    if !output.status.success() {
        bail!(
            "{what} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).with_context(|| format!("decode {what} output"))
}

fn package(project: &Path, tools: &ProjectTools) -> Result<Package> {
    let json = output(
        tools.cargo().command().current_dir(project).args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
        ]),
        "Cargo target discovery",
    )?;
    let metadata: Metadata = serde_json::from_str(&json).context("decode Cargo metadata")?;
    metadata
        .packages
        .into_iter()
        .find(|package| package.manifest_path == project.join("Cargo.toml"))
        .ok_or_else(|| anyhow!("locale compilation requires an application package"))
}

/// Cargo resolves features, rustflags, profiles, target tables, and build-script
/// cfg before invoking rustc. `--print cfg` needs no generated application
/// source, so the real settings can be obtained before translation codegen.
fn probe(
    project: &Path,
    tools: &ProjectTools,
    target: &Target,
    browser: bool,
    profile: &str,
    features: &[String],
) -> Result<CfgSet> {
    let mut command = tools.cargo().command();
    command.current_dir(project).arg("rustc");
    if target.kind.iter().any(|kind| kind == "bin") {
        command.arg("--bin").arg(&target.name);
    } else {
        command.arg("--lib");
    }
    if browser {
        command.args(["--target", "wasm32-unknown-unknown"]);
    }
    command.arg("--profile").arg(profile);
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    command.args(["--", "--print", "cfg"]);
    CfgSet::from_rustc(&output(&mut command, "Cargo locale cfg probe")?).map_err(|e| anyhow!(e))
}

fn targets(
    project: &Path,
    tools: &ProjectTools,
    package: &Package,
    release: bool,
    features: &[String],
) -> Result<Vec<SourceTarget>> {
    let config = crate::config::load(project)?;
    let library = package.targets.iter().find(|target| {
        target
            .kind
            .iter()
            .any(|kind| matches!(kind.as_str(), "lib" | "rlib" | "cdylib" | "staticlib"))
    });
    let host_probe = library
        .or_else(|| {
            package
                .targets
                .iter()
                .find(|t| t.kind.iter().any(|k| k == "bin"))
        })
        .ok_or_else(|| anyhow!("application has no library or binary source target"))?;
    let host = probe(
        project,
        tools,
        host_probe,
        false,
        if release { "release" } else { "dev" },
        features,
    )?;
    let mut roots = Vec::new();
    if let Some(library) = library {
        roots.push(SourceTarget {
            path: library.src_path.clone(),
            cfg: host.clone(),
            audience: CatalogAudience::Host,
        });
        if library.kind.iter().any(|kind| kind == "cdylib") {
            let profile = if release {
                config.wasm_profile.as_deref().unwrap_or("release")
            } else {
                "dev"
            };
            let browser = probe(project, tools, library, true, profile, features)?;
            roots.push(SourceTarget {
                path: library.src_path.clone(),
                cfg: browser,
                audience: CatalogAudience::Browser,
            });
        }
    }
    let configured = [&config.bin, &config.worker_bin]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    for binary in package
        .targets
        .iter()
        .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
    {
        if !configured.is_empty() && !configured.contains(&&binary.name) {
            continue;
        }
        let enabled = binary.required_features.iter().all(|feature| {
            syn::parse_str(&format!("feature={feature:?}"))
                .is_ok_and(|meta| host.matches(&meta).unwrap_or(false))
        });
        if !enabled {
            bail!(
                "locale target {} requires its Cargo features to be enabled",
                binary.name
            );
        }
        // Normal library and binary targets in one package use the same
        // package features, profile and build-script cfg. Probing its library
        // avoids compiling that library before the generated t module exists.
        roots.push(SourceTarget {
            path: binary.src_path.clone(),
            cfg: host.clone(),
            audience: CatalogAudience::Host,
        });
    }
    for name in configured {
        if !package
            .targets
            .iter()
            .any(|target| target.name == *name && target.kind.iter().any(|kind| kind == "bin"))
        {
            bail!("configured locale target {name} is not a Cargo binary");
        }
    }
    Ok(roots)
}

fn runtime(package: &Package, config: &LocaleConfig) -> Result<(String, Vec<String>)> {
    let dependency = |name| {
        package
            .dependencies
            .iter()
            .find(|dep| dep.name == name && dep.kind.is_none())
    };
    let mut features = Vec::new();
    let runtime = if let Some(dep) = dependency("pocopine") {
        let name = dep.rename.as_deref().unwrap_or(&dep.name);
        features.push(format!(
            "{name}/{}",
            if config.strict_parity {
                "locale-strict-parity"
            } else {
                "locale"
            }
        ));
        format!("::{}::locale", name.replace('-', "_"))
    } else if let Some(dep) = dependency("pocopine-locale") {
        let name = dep.rename.as_deref().unwrap_or(&dep.name);
        if config.strict_parity {
            features.push(format!("{name}/strict-parity"));
        }
        format!("::{}", name.replace('-', "_"))
    } else {
        bail!("[locale] requires a pocopine or pocopine-locale dependency");
    };
    if let Some(dep) = dependency("pocopine-server") {
        features.push(format!(
            "{}/locale",
            dep.rename.as_deref().unwrap_or(&dep.name)
        ));
    }
    Ok((runtime, features))
}

fn report(discovery: &ProjectDiscovery, diagnostics: &[pocopine_locale::server::Diagnostic]) {
    let files = discovery
        .files
        .iter()
        .map(|file| pocopine_stylekit::SourceFile {
            path: file.path.clone(),
            source: file.source.clone(),
        })
        .collect::<Vec<_>>();
    for diagnostic in diagnostics {
        eprint!("{}", pocopine_stylekit::render(diagnostic, &files));
    }
}

pub fn prepare(project: &Path, release: bool) -> Result<Option<Prepared>> {
    let project = project.canonicalize()?;
    let Some(config) = config(&project)? else {
        return Ok(None);
    };
    println!("▶ compiling locale catalogs");
    let tools = ProjectTools::load(&project)?;
    let package = package(&project, &tools)?;
    let (runtime, features) = runtime(&package, &config)?;
    let roots = targets(&project, &tools, &package, release, &features)?;
    let discovery = discover_project(&project, &roots);
    let compiled = discovery.compile();
    report(&discovery, &compiled.diagnostics);
    if compiled.has_errors() {
        bail!("locale compilation failed; previous artifacts retained");
    }
    let build_id = compiled
        .build_id
        .as_ref()
        .context("locale compilation produced no build identity")?;
    let directory = project.join("target/pocopine/locale").join(build_id);
    let rust = directory.join("pocopine_locale.rs");
    let mut catalogs = BTreeMap::new();
    for catalog in &compiled.catalogs {
        if catalog.artifact.audience == CatalogAudience::Browser {
            write(&directory.join(&catalog.filename), &catalog.bytes)?;
            catalogs.insert(
                catalog.artifact.locale.clone(),
                format!("/pkg/locales/{}", catalog.filename),
            );
        }
    }
    let code = generate_rust(&compiled, &config.validate()?, &runtime).map_err(|e| anyhow!(e))?;
    write(&rust, code.as_bytes())?;
    let prepared = Prepared {
        rust,
        directory,
        features,
        manifest: LocaleManifest {
            format_version: CATALOG_FORMAT_VERSION,
            build_id: build_id.clone(),
            message_count: compiled.messages.len(),
            directions: pocopine_locale::server::locale_directions(&config.validate()?),
            config,
            catalogs,
        },
    };
    Ok(Some(prepared))
}

pub fn load(project: &Path) -> Result<Option<Prepared>> {
    if config(project)?.is_none() {
        return Ok(None);
    }
    let prepared = fs::read(project.join(BUILD_INFO)).context(
        "locale generation is missing; run the browser build before building configured binaries",
    )?;
    Ok(Some(
        serde_json::from_slice(&prepared).context("read prepared locale build")?,
    ))
}

pub fn publish(project: &Path, prepared: &Prepared) -> Result<()> {
    for path in prepared.manifest.catalogs.values() {
        let filename = path
            .strip_prefix("/pkg/locales/")
            .context("invalid generated catalog path")?;
        let bytes = fs::read(prepared.directory.join(filename))?;
        write(&project.join("pkg/locales").join(filename), &bytes)?;
    }
    write(
        &project.join("pkg/locales/manifest.json"),
        &serde_json::to_vec_pretty(&prepared.manifest)?,
    )?;
    // Publish the bin build input only after wasm compilation has succeeded.
    // A failed browser build must not replace the last usable pairing.
    write(
        &project.join(BUILD_INFO),
        &serde_json::to_vec_pretty(prepared)?,
    )
}

/// Do not truncate a working artifact on disk-full or interrupted writes.
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    let parent = path.parent().context("artifact has no parent directory")?;
    fs::create_dir_all(parent)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (temporary, mut file) = loop {
        let temporary = parent.join(format!(
            ".locale-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create locale artifact in {}", parent.display()));
            }
        }
    };
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("write locale artifact {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_probes_real_build_script_cfg_before_generated_sources_exist() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().canonicalize().unwrap();
        fs::write(
            project.join("Cargo.toml"),
            r#"
            [package]
            name = "locale-cfg-contract"
            version = "0.0.0"
            edition = "2024"
            [workspace]
            [lib]
            path = "lib.rs"
            crate-type = ["cdylib", "rlib"]
            [features]
            selected = []
        "#,
        )
        .unwrap();
        fs::write(
            project.join("lib.rs"),
            "include!(\"not-generated-yet.rs\");",
        )
        .unwrap();
        fs::write(
            project.join("build.rs"),
            r#"
            fn main() {
                println!("cargo:rustc-check-cfg=cfg(pocopine_browser)");
                println!("cargo:rustc-check-cfg=cfg(pocopine_host)");
                if std::env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "wasm32" {
                    println!("cargo:rustc-cfg=pocopine_browser");
                } else { println!("cargo:rustc-cfg=pocopine_host"); }
            }
        "#,
        )
        .unwrap();
        let tools = ProjectTools::empty(&project);
        let package = package(&project, &tools).unwrap();
        let roots = targets(&project, &tools, &package, false, &["selected".into()]).unwrap();
        assert_eq!(roots.len(), 2);
        for root in roots {
            assert!(
                root.cfg
                    .matches(&syn::parse_str("feature=\"selected\"").unwrap())
                    .unwrap()
            );
            let browser = root.audience == CatalogAudience::Browser;
            assert_eq!(
                root.cfg
                    .matches(&syn::parse_str("pocopine_browser").unwrap())
                    .unwrap(),
                browser
            );
            assert_eq!(
                root.cfg
                    .matches(&syn::parse_str("pocopine_host").unwrap())
                    .unwrap(),
                !browser
            );
            assert_eq!(
                root.cfg
                    .matches(&syn::parse_str("target_arch=\"wasm32\"").unwrap())
                    .unwrap(),
                browser
            );
        }
        assert!(!project.join("not-generated-yet.rs").exists());
    }

    #[test]
    fn catalog_publication_excludes_private_rust_and_host_copy() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("private");
        write(&directory.join("en.catalog.json"), b"browser copy").unwrap();
        write(&directory.join("pocopine_locale.rs"), b"host copy").unwrap();
        let prepared = Prepared {
            rust: directory.join("pocopine_locale.rs"),
            directory,
            features: vec![],
            manifest: LocaleManifest {
                format_version: CATALOG_FORMAT_VERSION,
                build_id: "a".repeat(64),
                message_count: 1,
                config: LocaleConfig {
                    default: "en".parse().unwrap(),
                    locales: vec!["en".parse().unwrap()],
                    routing: Default::default(),
                    strict_parity: false,
                },
                catalogs: [("en".parse().unwrap(), "/pkg/locales/en.catalog.json".into())].into(),
                directions: [("en".parse().unwrap(), pocopine_locale::TextDirection::Ltr)].into(),
            },
        };
        publish(temp.path(), &prepared).unwrap();
        assert_eq!(
            fs::read(temp.path().join("pkg/locales/en.catalog.json")).unwrap(),
            b"browser copy"
        );
        assert!(!temp.path().join("pkg/locales/pocopine_locale.rs").exists());
        let manifest: LocaleManifest = serde_json::from_slice(
            &fs::read(temp.path().join("pkg/locales/manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.build_id, prepared.manifest.build_id);
    }
}
