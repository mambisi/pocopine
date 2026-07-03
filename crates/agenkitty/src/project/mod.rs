use std::fs;
use std::path::{Path, PathBuf};

use agenkitty_core::config::AgenkittyConfig;
use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub agents_md: Option<PathBuf>,
    pub agenkitty_instructions: Option<PathBuf>,
}

/// Load the project's `.agenkitty/config.toml`. An absent file is the
/// defaults (every tool's spec-declared mode rules); anything else that keeps
/// the file from loading — unreadable, not a file, invalid TOML — is a hard
/// error. A project that *wrote* policy config must never silently run under
/// different policy than it asked for, so only a genuine `NotFound` defaults
/// (`Path::exists()` would swallow `PermissionDenied` and the like as
/// "absent").
pub fn load_project_config(root: &Path) -> Result<AgenkittyConfig> {
    let path = root.join(".agenkitty").join("config.toml");
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AgenkittyConfig::default());
        }
        Err(err) => {
            return Err(err).with_context(|| format!("read project config `{}`", path.display()));
        }
    };
    toml::from_str(&raw).with_context(|| format!("parse project config `{}`", path.display()))
}

impl ProjectContext {
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let root = fs::canonicalize(path.as_ref())
            .with_context(|| format!("canonicalize `{}`", path.as_ref().display()))?;
        let agents = root.join("AGENTS.md");
        let instructions = root.join(".agenkitty").join("instructions.md");
        Ok(Self {
            root,
            agents_md: agents.exists().then_some(agents),
            agenkitty_instructions: instructions.exists().then_some(instructions),
        })
    }

    pub fn system_prompt(&self) -> Result<String> {
        let mut parts = Vec::new();
        parts.push(
            "You are Agenkitty, a local agent framework running on Pocopine Agenkit.".to_string(),
        );
        if let Some(path) = &self.agents_md {
            parts.push(read_instruction_file(path)?);
        }
        if let Some(path) = &self.agenkitty_instructions {
            parts.push(read_instruction_file(path)?);
        }
        Ok(parts.join("\n\n"))
    }
}

fn read_instruction_file(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("read instructions `{}`", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_finds_agents_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
        let project = ProjectContext::discover(dir.path()).unwrap();
        assert!(project.agents_md.is_some());
        assert!(project.system_prompt().unwrap().contains("rules"));
    }

    #[test]
    fn absent_config_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_project_config(dir.path()).unwrap();
        assert_eq!(config, AgenkittyConfig::default());
    }

    #[test]
    fn policy_overrides_load_from_config_file() {
        use agenkitty_core::ToolMode;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agenkitty")).unwrap();
        fs::write(
            dir.path().join(".agenkitty").join("config.toml"),
            "[policy]\nwrite_mode = \"deny\"\n",
        )
        .unwrap();
        let config = load_project_config(dir.path()).unwrap();
        assert_eq!(config.policy.write_mode, Some(ToolMode::Deny));
        assert_eq!(config.policy.read_mode, None);
    }

    #[test]
    fn invalid_config_file_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agenkitty")).unwrap();
        fs::write(
            dir.path().join(".agenkitty").join("config.toml"),
            "[policy]\nwrite_mode = \"yolo\"\n",
        )
        .unwrap();
        let error = load_project_config(dir.path()).unwrap_err();
        assert!(error.to_string().contains("config.toml"));
    }

    #[test]
    fn unreadable_config_is_a_hard_error_not_a_silent_default() {
        // `config.toml` exists but cannot be read as a file (here: it is a
        // directory → EISDIR). Only a genuine NotFound may fall back to the
        // defaults; every other failure must surface, never fail open.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agenkitty").join("config.toml")).unwrap();
        let error = load_project_config(dir.path()).unwrap_err();
        assert!(error.to_string().contains("read project config"));
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_config_is_a_hard_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".agenkitty")).unwrap();
        let path = dir.path().join(".agenkitty").join("config.toml");
        fs::write(&path, "[policy]\nwrite_mode = \"deny\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        // Root bypasses permission bits; skip there (CI/dev run unprivileged).
        if fs::read_to_string(&path).is_ok() {
            eprintln!("skipping: process can read a 0o000 file (running as root?)");
            return;
        }
        let error = load_project_config(dir.path()).unwrap_err();
        assert!(error.to_string().contains("read project config"));
    }
}
