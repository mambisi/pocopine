//! Skill-root path confinement (RFC-121 S3).
//!
//! The same discipline as the `agenkitty` fs family: validate-relative →
//! reject-secret → join + canonicalize → ensure-inside-root → re-check the
//! *resolved* relative path against the secret policy, so a symlink that
//! resolves outside the skill root (or onto a credential file inside it) reads
//! as a refusal, not a file. Duplicated rather than imported because
//! `agenkitty` depends on this crate, not the reverse; the secret-path list
//! matches `tools/fs/common.rs` exactly.

use std::path::{Component, Path, PathBuf};

use super::error::SkillError;

/// Resolve `rel` inside the (already canonical) skill root, refusing
/// escapes and secret-looking paths.
pub(crate) fn confined_resource_path(root: &Path, rel: &str) -> Result<PathBuf, SkillError> {
    let relative = validate_relative_path(rel)?;
    reject_secret_path(relative)?;
    let canonical = root.join(relative).canonicalize().map_err(|err| {
        SkillError::NotFound(format!("resource `{}`: {err}", normalize_slashes(rel)))
    })?;
    if !canonical.starts_with(root) {
        return Err(SkillError::Policy(format!(
            "resource `{}` escapes the skill directory",
            normalize_slashes(rel)
        )));
    }
    let resolved_relative = canonical.strip_prefix(root).map_err(|_| {
        SkillError::Policy(format!(
            "resource `{}` escapes the skill directory",
            normalize_slashes(rel)
        ))
    })?;
    reject_secret_path(resolved_relative)?;
    Ok(canonical)
}

fn validate_relative_path(path: &str) -> Result<&Path, SkillError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(SkillError::Validation(
            "resource path must not be empty".to_string(),
        ));
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SkillError::Policy(
            "resource path must be relative and stay inside the skill directory".to_string(),
        ));
    }
    Ok(path)
}

fn reject_secret_path(path: &Path) -> Result<(), SkillError> {
    if is_secret_path(path) {
        return Err(SkillError::Policy(format!(
            "resource `{}` is blocked by secret-file policy",
            normalize_slashes(&path.display().to_string())
        )));
    }
    Ok(())
}

pub(crate) fn is_secret_path(path: &Path) -> bool {
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

pub(crate) fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}
