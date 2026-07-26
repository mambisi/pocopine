//! Host-neutral static deployment artefact assembly.
//!
//! Static adapters all upload the same directory tree. This module keeps
//! that assembly independent of any vendor API: configured
//! [`DeploySpec::static_files`] are copied into
//! `<project>/<build_dir>/dist`, then the generated `pkg/index.html` is
//! promoted to the distribution root when present.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{Artefact, DeploySpec};

/// Name of the static artefact directory beneath [`DeploySpec::build_dir`].
pub const STATIC_DIST_DIR: &str = "dist";

/// Workspace-root-relative path of the static artefact for `spec`.
///
/// The deploy CLI runs adapters from the Cargo workspace root. Standalone
/// projects therefore produce `<build_dir>/dist`, while workspace members
/// produce `<workspace_subpath>/<build_dir>/dist`.
pub fn static_dist_path(spec: &DeploySpec) -> PathBuf {
    let mut path = PathBuf::new();
    if !spec.workspace_subpath.is_empty() {
        path.push(&spec.workspace_subpath);
    }
    path.push(spec.build_dir());
    path.push(STATIC_DIST_DIR);
    path
}

/// Assemble a static distribution beneath the current workspace root.
///
/// This is the entry point static [`crate::DeployAdapter`] implementations
/// call from `build_artefact`.
pub fn build_static_dist(spec: &DeploySpec) -> Result<Artefact> {
    let workspace_root =
        std::env::current_dir().context("resolving the workspace root for static deployment")?;
    build_static_dist_at(&workspace_root, spec)
}

/// Assemble a static distribution beneath an explicit workspace root.
///
/// This variant is useful to orchestrators that already resolved the
/// workspace and keeps filesystem behavior deterministic in tests.
pub fn build_static_dist_at(workspace_root: &Path, spec: &DeploySpec) -> Result<Artefact> {
    let workspace_subpath = checked_relative(&spec.workspace_subpath, "workspace_subpath", true)?;
    let build_dir = checked_relative(spec.build_dir(), "build_dir", false)?;
    let project_root = workspace_root.join(&workspace_subpath);

    refuse_symlink_components(workspace_root, &workspace_subpath, "workspace_subpath")?;
    let project_metadata = fs::symlink_metadata(&project_root).with_context(|| {
        format!(
            "static deployment project root does not exist: {}",
            project_root.display()
        )
    })?;
    if project_metadata.file_type().is_symlink() {
        bail!(
            "refusing static deployment through symlinked project root: {}",
            project_root.display()
        );
    }
    if !project_metadata.is_dir() {
        bail!(
            "static deployment project root is not a directory: {}",
            project_root.display()
        );
    }

    refuse_symlink_components(&project_root, &build_dir, "build_dir")?;
    let dist = project_root.join(&build_dir).join(STATIC_DIST_DIR);

    let mut sources = Vec::with_capacity(spec.static_files.len());
    for entry in &spec.static_files {
        let relative = checked_relative(entry, "static_files entry", false)?;
        refuse_symlink_components(&project_root, &relative, "static_files entry")?;
        let source = project_root.join(&relative);

        // Copying an ancestor of the output would recursively copy the
        // distribution into itself. Refuse before stale-output cleanup.
        if dist.starts_with(&source) {
            bail!(
                "static_files entry `{entry}` contains the output directory {}; choose a narrower static path",
                dist.display()
            );
        }
        sources.push((entry.as_str(), relative, source));
    }

    clean_dist(&dist)?;
    fs::create_dir_all(&dist)
        .with_context(|| format!("creating static distribution {}", dist.display()))?;

    for (entry, relative, source) in sources {
        let destination = dist.join(relative);
        copy_path(&source, &destination)
            .with_context(|| format!("copying static_files entry `{entry}`"))?;
    }

    // `pocopine build` writes an index whose script/module URLs point at
    // content-hashed bundle names. The source index intentionally remains
    // stable for development, so static hosts must serve the generated one.
    let generated_index = dist.join("pkg").join("index.html");
    if generated_index.is_file() {
        fs::copy(&generated_index, dist.join("index.html")).with_context(|| {
            format!(
                "promoting generated index {} to the static distribution root",
                generated_index.display()
            )
        })?;
    }

    Ok(Artefact::StaticDist { path: dist })
}

fn checked_relative(value: &str, label: &str, allow_empty: bool) -> Result<PathBuf> {
    if value.is_empty() {
        if allow_empty {
            return Ok(PathBuf::new());
        }
        bail!("{label} must not be empty");
    }

    let mut normalized = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("{label} must be project-relative and contain no `..`: `{value}`");
            }
        }
    }
    Ok(normalized)
}

fn refuse_symlink_components(base: &Path, relative: &Path, label: &str) -> Result<()> {
    let mut path = base.to_path_buf();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            continue;
        };
        path.push(segment);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("{label} traverses symlink {}", path.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspecting static path {}", path.display()));
            }
        }
    }
    Ok(())
}

fn clean_dist(dist: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(dist) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting static distribution {}", dist.display()));
        }
    };

    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to replace symlinked static distribution {}",
            dist.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "static distribution path exists but is not a directory: {}",
            dist.display()
        );
    }

    fs::remove_dir_all(dist)
        .with_context(|| format!("removing stale static distribution {}", dist.display()))
}

fn copy_path(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("static path does not exist: {}", source.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        bail!("refusing to copy symlink {}", source.display());
    }
    if file_type.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating static directory {}", parent.display()))?;
        }
        fs::copy(source, destination).with_context(|| {
            format!(
                "copying static file {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        return Ok(());
    }
    if !file_type.is_dir() {
        bail!(
            "static path is neither a regular file nor directory: {}",
            source.display()
        );
    }

    fs::create_dir_all(destination)
        .with_context(|| format!("creating static directory {}", destination.display()))?;
    let mut entries = fs::read_dir(source)
        .with_context(|| format!("reading static directory {}", source.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("reading entries in static directory {}", source.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        copy_path(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;

    use super::*;
    use crate::Mode;

    fn spec(static_files: &[&str]) -> DeploySpec {
        DeploySpec {
            app_name: "site".into(),
            package_name: "site".into(),
            git_sha: "abc1234".into(),
            git_remote: None,
            mode: Mode::Static,
            processes: BTreeMap::new(),
            services: BTreeMap::new(),
            env: BTreeMap::new(),
            host_overrides: BTreeMap::new(),
            uses_jobs: false,
            uses_collab: false,
            uses_storage: false,
            uses_websocket: false,
            first_deploy: false,
            skip_build: false,
            environment: None,
            workspace_subpath: String::new(),
            has_rust_toolchain: false,
            static_files: static_files.iter().map(|path| (*path).into()).collect(),
            build_dir: None,
        }
    }

    fn dist_path(artefact: Artefact) -> PathBuf {
        match artefact {
            Artefact::StaticDist { path } => path,
            Artefact::OciImage { .. } => panic!("expected a static distribution"),
        }
    }

    fn workspace() -> TempDir {
        tempfile::tempdir().expect("create workspace")
    }

    #[test]
    fn promotes_generated_pkg_index_to_distribution_root() {
        let workspace = workspace();
        fs::create_dir_all(workspace.path().join("pkg")).unwrap();
        fs::write(
            workspace.path().join("index.html"),
            "<script src=\"/pkg/site.js\"></script>",
        )
        .unwrap();
        fs::write(
            workspace.path().join("pkg/index.html"),
            "<script src=\"/pkg/site.abc123.js\"></script>",
        )
        .unwrap();
        fs::write(workspace.path().join("pkg/site.abc123.js"), "bundle").unwrap();

        let dist = dist_path(
            build_static_dist_at(workspace.path(), &spec(&["index.html", "pkg"])).unwrap(),
        );

        assert_eq!(
            fs::read_to_string(dist.join("index.html")).unwrap(),
            "<script src=\"/pkg/site.abc123.js\"></script>"
        );
        assert_eq!(
            fs::read_to_string(dist.join("pkg/index.html")).unwrap(),
            "<script src=\"/pkg/site.abc123.js\"></script>"
        );
    }

    #[test]
    fn recursively_copies_configured_directories() {
        let workspace = workspace();
        fs::create_dir_all(workspace.path().join("assets/icons")).unwrap();
        fs::write(workspace.path().join("assets/icons/pine.svg"), "<svg/>").unwrap();
        fs::write(workspace.path().join("styles.css"), "body {}").unwrap();

        let dist = dist_path(
            build_static_dist_at(workspace.path(), &spec(&["assets", "styles.css"])).unwrap(),
        );

        assert_eq!(
            fs::read_to_string(dist.join("assets/icons/pine.svg")).unwrap(),
            "<svg/>"
        );
        assert_eq!(
            fs::read_to_string(dist.join("styles.css")).unwrap(),
            "body {}"
        );
    }

    #[test]
    fn removes_stale_distribution_contents_before_copying() {
        let workspace = workspace();
        let dist = workspace.path().join(".pocopine/build/dist");
        fs::create_dir_all(&dist).unwrap();
        fs::write(dist.join("stale.js"), "old").unwrap();
        fs::write(workspace.path().join("index.html"), "new").unwrap();

        let built =
            dist_path(build_static_dist_at(workspace.path(), &spec(&["index.html"])).unwrap());

        assert_eq!(built, dist);
        assert!(!built.join("stale.js").exists());
        assert_eq!(fs::read_to_string(built.join("index.html")).unwrap(), "new");
    }

    #[test]
    fn honors_workspace_subpath_and_custom_build_dir() {
        let workspace = workspace();
        let project = workspace.path().join("examples/site");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("index.html"), "site").unwrap();
        let mut deploy_spec = spec(&["index.html"]);
        deploy_spec.workspace_subpath = "examples/site".into();
        deploy_spec.build_dir = Some(".generated/deploy".into());

        let dist = dist_path(build_static_dist_at(workspace.path(), &deploy_spec).unwrap());

        assert_eq!(dist, project.join(".generated/deploy/dist"));
        assert_eq!(fs::read_to_string(dist.join("index.html")).unwrap(), "site");
        assert!(!workspace.path().join(".generated/deploy/dist").exists());
    }

    #[test]
    fn reports_missing_configured_static_path() {
        let workspace = workspace();

        let error = build_static_dist_at(workspace.path(), &spec(&["missing.css"]))
            .expect_err("missing path must fail");
        let message = format!("{error:#}");

        assert!(message.contains("missing.css"), "{message}");
        assert!(message.contains("does not exist"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_in_static_tree() {
        use std::os::unix::fs::symlink;

        let workspace = workspace();
        fs::create_dir_all(workspace.path().join("public")).unwrap();
        fs::write(workspace.path().join("secret.txt"), "secret").unwrap();
        symlink(
            workspace.path().join("secret.txt"),
            workspace.path().join("public/leak.txt"),
        )
        .unwrap();

        let error = build_static_dist_at(workspace.path(), &spec(&["public"]))
            .expect_err("symlink must fail");
        let message = format!("{error:#}");

        assert!(message.contains("symlink"), "{message}");
        assert!(message.contains("leak.txt"), "{message}");
    }
}
