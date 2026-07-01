use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub agents_md: Option<PathBuf>,
    pub agenkitty_instructions: Option<PathBuf>,
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
}
