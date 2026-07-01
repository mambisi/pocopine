use std::ffi::OsString;
use std::fs::{self, FileType, Metadata};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FsPathKind {
    File,
    Directory,
    Symlink,
    Other,
}

pub(crate) fn canonical_root(root: &Path) -> AgenkitResult<PathBuf> {
    root.canonicalize().map_err(|err| {
        AgenkitError::config(format!("canonicalize root `{}`: {err}", root.display()))
    })
}

pub(crate) fn canonical_existing_path(
    root: &Path,
    path: &str,
    tool_id: &str,
) -> AgenkitResult<PathBuf> {
    let relative = validate_relative_path(path, tool_id)?;
    reject_secret_path(relative, tool_id)?;
    let canonical = root.join(relative).canonicalize().map_err(|err| {
        AgenkitError::not_found(format!(
            "{tool_id} path `{}`: {err}",
            normalize_slashes(path)
        ))
    })?;
    ensure_inside_root(root, &canonical, path, tool_id)?;
    let resolved_relative = canonical.strip_prefix(root).map_err(|_| {
        AgenkitError::tool_policy(format!(
            "{tool_id} path `{}` escapes the project root",
            normalize_slashes(path)
        ))
    })?;
    reject_secret_path(resolved_relative, tool_id)?;
    Ok(canonical)
}

pub(crate) fn checked_target_path(
    root: &Path,
    path: &str,
    tool_id: &str,
) -> AgenkitResult<PathBuf> {
    let relative = validate_relative_path(path, tool_id)?;
    reject_secret_path(relative, tool_id)?;
    let target = root.join(relative);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AgenkitError::tool_policy(format!(
                "{tool_id} path `{}` is a symlink and cannot be used as a write target",
                normalize_slashes(path)
            )));
        }
        Ok(_) => {
            let canonical = target.canonicalize().map_err(|err| {
                AgenkitError::not_found(format!(
                    "{tool_id} path `{}`: {err}",
                    normalize_slashes(path)
                ))
            })?;
            ensure_inside_root(root, &canonical, path, tool_id)?;
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let parent = target.parent().ok_or_else(|| {
                AgenkitError::validation(format!("{tool_id} path `{path}` has no parent"))
            })?;
            let parent_metadata = fs::symlink_metadata(parent).map_err(|err| {
                AgenkitError::not_found(format!(
                    "{tool_id} parent for `{}`: {err}",
                    normalize_slashes(path)
                ))
            })?;
            if parent_metadata.file_type().is_symlink() {
                return Err(AgenkitError::tool_policy(format!(
                    "{tool_id} parent for `{}` is a symlink and cannot be used as a write target",
                    normalize_slashes(path)
                )));
            }
            let canonical_parent = parent.canonicalize().map_err(|err| {
                AgenkitError::not_found(format!(
                    "{tool_id} parent for `{}`: {err}",
                    normalize_slashes(path)
                ))
            })?;
            ensure_inside_root(root, &canonical_parent, path, tool_id)?;
        }
        Err(err) => {
            return Err(AgenkitError::not_found(format!(
                "{tool_id} path `{}`: {err}",
                normalize_slashes(path)
            )));
        }
    }
    Ok(target)
}

pub(crate) fn checked_descendant_target_path(
    root: &Path,
    path: &str,
    tool_id: &str,
) -> AgenkitResult<PathBuf> {
    let relative = validate_relative_path(path, tool_id)?;
    reject_secret_path(relative, tool_id)?;
    let target = root.join(relative);
    let mut ancestor = target.parent().ok_or_else(|| {
        AgenkitError::validation(format!("{tool_id} path `{path}` has no parent"))
    })?;

    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AgenkitError::tool_policy(format!(
                    "{tool_id} parent for `{}` is a symlink and cannot be used as a write target",
                    normalize_slashes(path)
                )));
            }
            Ok(_) => {
                let canonical_ancestor = ancestor.canonicalize().map_err(|err| {
                    AgenkitError::not_found(format!(
                        "{tool_id} parent for `{}`: {err}",
                        normalize_slashes(path)
                    ))
                })?;
                ensure_inside_root(root, &canonical_ancestor, path, tool_id)?;
                return Ok(target);
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or_else(|| {
                    AgenkitError::not_found(format!(
                        "{tool_id} no existing parent for `{}`",
                        normalize_slashes(path)
                    ))
                })?;
            }
            Err(err) => {
                return Err(AgenkitError::not_found(format!(
                    "{tool_id} parent for `{}`: {err}",
                    normalize_slashes(path)
                )));
            }
        }
    }
}

pub(crate) fn checked_existing_entry_path(
    root: &Path,
    path: &str,
    tool_id: &str,
    allow_final_symlink: bool,
) -> AgenkitResult<PathBuf> {
    let relative = validate_relative_path(path, tool_id)?;
    reject_secret_path(relative, tool_id)?;
    let components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::CurDir => None,
            _ => None,
        })
        .collect::<Vec<OsString>>();
    if components.is_empty() {
        return Err(AgenkitError::validation(format!(
            "{tool_id} path `{path}` must name an entry below the project root"
        )));
    }

    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|err| {
            AgenkitError::not_found(format!(
                "{tool_id} path `{}`: {err}",
                normalize_slashes(path)
            ))
        })?;
        if metadata.file_type().is_symlink() {
            if allow_final_symlink && index + 1 == components.len() {
                return Ok(current);
            }
            return Err(AgenkitError::tool_policy(format!(
                "{tool_id} path `{}` contains a symlink ancestor",
                normalize_slashes(path)
            )));
        }
    }

    let canonical = current.canonicalize().map_err(|err| {
        AgenkitError::not_found(format!(
            "{tool_id} path `{}`: {err}",
            normalize_slashes(path)
        ))
    })?;
    ensure_inside_root(root, &canonical, path, tool_id)?;
    let resolved_relative = canonical.strip_prefix(root).map_err(|_| {
        AgenkitError::tool_policy(format!(
            "{tool_id} path `{}` escapes the project root",
            normalize_slashes(path)
        ))
    })?;
    reject_secret_path(resolved_relative, tool_id)?;
    Ok(canonical)
}

pub(crate) fn validate_relative_path<'a>(path: &'a str, tool_id: &str) -> AgenkitResult<&'a Path> {
    let path = path.trim();
    if path.is_empty() {
        return Err(AgenkitError::validation(format!(
            "{tool_id} path must not be empty"
        )));
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AgenkitError::tool_policy(format!(
            "{tool_id} path must be relative and stay inside the project root"
        )));
    }
    Ok(path)
}

pub(crate) fn clamp_limit(value: Option<usize>, default: usize, max: usize) -> usize {
    value.unwrap_or(default).clamp(1, max)
}

pub(crate) fn relative_display_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    format!("./{}", normalize_slashes(&relative.display().to_string()))
}

pub(crate) fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

pub(crate) fn metadata_kind(metadata: &Metadata) -> FsPathKind {
    file_type_kind(metadata.file_type())
}

pub(crate) fn symlink_metadata_kind(path: &Path) -> AgenkitResult<FsPathKind> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        AgenkitError::internal(format!("read metadata `{}`: {err}", path.display()))
    })?;
    Ok(file_type_kind(metadata.file_type()))
}

pub(crate) fn reject_secret_path(path: &Path, tool_id: &str) -> AgenkitResult<()> {
    if is_secret_path(path) {
        return Err(AgenkitError::tool_policy(format!(
            "{tool_id} path `{}` is blocked by secret-file policy",
            normalize_slashes(&path.display().to_string())
        )));
    }
    Ok(())
}

fn is_secret_path(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str().map(|name| name.to_ascii_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.iter().any(|name| name.starts_with(".env")) {
        return true;
    }
    let Some(file_name) = components.last().map(String::as_str) else {
        return false;
    };
    if matches!(
        file_name,
        ".npmrc"
            | ".pypirc"
            | ".netrc"
            | "_netrc"
            | "credentials"
            | "credentials.toml"
            | "application_default_credentials.json"
            | "credentials.db"
            | "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
            | "identity"
    ) {
        return true;
    }
    if file_name.ends_with(".pem") {
        return true;
    }
    if file_name.ends_with(".json")
        && (file_name.contains("service-account") || file_name.contains("service_account"))
    {
        return true;
    }
    path_has_prefix(&components, &[".aws"])
        || path_has_prefix(&components, &[".azure"])
        || path_has_prefix(&components, &[".config", "gcloud"])
        || path_has_prefix(&components, &[".docker", "config.json"])
        || path_has_prefix(&components, &[".cargo", "credentials"])
        || path_has_prefix(&components, &[".cargo", "credentials.toml"])
        || path_has_prefix(&components, &[".ssh"])
}

fn path_has_prefix(components: &[String], prefix: &[&str]) -> bool {
    components.len() >= prefix.len()
        && components
            .windows(prefix.len())
            .any(|window| window.iter().zip(prefix).all(|(left, right)| left == right))
}

fn ensure_inside_root(
    root: &Path,
    canonical: &Path,
    original: &str,
    tool_id: &str,
) -> AgenkitResult<()> {
    if !canonical.starts_with(root) {
        return Err(AgenkitError::tool_policy(format!(
            "{tool_id} path `{}` escapes the project root",
            normalize_slashes(original)
        )));
    }
    Ok(())
}

fn file_type_kind(file_type: FileType) -> FsPathKind {
    if file_type.is_file() {
        FsPathKind::File
    } else if file_type.is_dir() {
        FsPathKind::Directory
    } else if file_type.is_symlink() {
        FsPathKind::Symlink
    } else {
        FsPathKind::Other
    }
}
