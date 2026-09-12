use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

pub(crate) const CONFIG_FILE: &str = ".pocopine.toml";

#[derive(Debug, Clone)]
pub(crate) struct ProjectTools {
    config_path: PathBuf,
    loaded: bool,
    aliases: ToolAliases,
}

impl ProjectTools {
    pub(crate) fn load(project: &Path) -> Result<Self> {
        let config_path = project.join(CONFIG_FILE);
        if !config_path.is_file() {
            return Ok(Self {
                config_path,
                loaded: false,
                aliases: ToolAliases::default(),
            });
        }

        let text = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        let config: ToolConfig =
            toml::from_str(&text).with_context(|| format!("parse {}", config_path.display()))?;
        Ok(Self {
            config_path,
            loaded: true,
            aliases: config.tools,
        })
    }

    pub(crate) fn empty(project: &Path) -> Self {
        Self {
            config_path: project.join(CONFIG_FILE),
            loaded: false,
            aliases: ToolAliases::default(),
        }
    }

    pub(crate) fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub(crate) fn loaded(&self) -> bool {
        self.loaded
    }

    pub(crate) fn cargo(&self) -> ToolCommand {
        self.aliases
            .cargo
            .clone()
            .unwrap_or_else(|| ToolCommand::simple("cargo"))
    }

    pub(crate) fn rustc(&self) -> ToolCommand {
        self.aliases
            .rustc
            .clone()
            .unwrap_or_else(|| ToolCommand::simple("rustc"))
    }

    pub(crate) fn wasm_pack(&self) -> ToolCommand {
        self.aliases
            .wasm_pack
            .clone()
            .unwrap_or_else(|| ToolCommand::simple("wasm-pack"))
    }

    pub(crate) fn node(&self) -> ToolCommand {
        self.aliases
            .node
            .clone()
            .unwrap_or_else(|| ToolCommand::simple("node"))
    }

    pub(crate) fn package_manager(&self, default_binary: &str) -> ToolCommand {
        self.aliases
            .package_manager
            .clone()
            .unwrap_or_else(|| ToolCommand::simple(default_binary))
    }

    pub(crate) fn package_manager_override(&self) -> Option<ToolCommand> {
        self.aliases.package_manager.clone()
    }

    pub(crate) fn tailwindcss(&self) -> Option<ToolCommand> {
        self.aliases.tailwindcss.clone()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum ToolCommand {
    Simple(String),
    Detailed {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

impl ToolCommand {
    fn simple(command: impl Into<String>) -> Self {
        Self::Simple(command.into())
    }

    pub(crate) fn command(&self) -> Command {
        let mut cmd = Command::new(self.program());
        cmd.args(self.default_args());
        cmd
    }

    pub(crate) fn program(&self) -> &str {
        match self {
            Self::Simple(command) => command,
            Self::Detailed { command, .. } => command,
        }
    }

    pub(crate) fn default_args(&self) -> &[String] {
        match self {
            Self::Simple(_) => &[],
            Self::Detailed { args, .. } => args,
        }
    }

    pub(crate) fn display(&self) -> String {
        let mut parts = vec![self.program().to_string()];
        parts.extend(self.default_args().iter().cloned());
        parts.join(" ")
    }

    pub(crate) fn program_name(&self) -> &str {
        let base = self
            .program()
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_else(|| self.program());
        base.strip_suffix(".exe").unwrap_or(base)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ToolConfig {
    #[serde(default)]
    tools: ToolAliases,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ToolAliases {
    cargo: Option<ToolCommand>,
    rustc: Option<ToolCommand>,
    wasm_pack: Option<ToolCommand>,
    node: Option<ToolCommand>,
    package_manager: Option<ToolCommand>,
    tailwindcss: Option<ToolCommand>,
}

/// Minimal `which` - walks `$PATH` for an executable by that name.
pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let with_ext = dir.join(format!("{name}.exe"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

pub(crate) fn resolve_program(tool: &ToolCommand) -> Option<PathBuf> {
    let program = tool.program();
    let path = Path::new(program);
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    which(program)
}

pub(crate) fn format_command(cmd: &Command) -> String {
    let mut parts = vec![cmd.get_program().to_string_lossy().to_string()];
    parts.extend(cmd.get_args().map(|arg| arg.to_string_lossy().to_string()));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_tools_accept_simple_and_detailed_aliases() {
        let unique = format!(
            "pocopine-tools-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(CONFIG_FILE),
            r#"
[tools]
cargo = { command = "cargo", args = ["+stable"] }
wasm-pack = "/opt/tools/wasm-pack"
package-manager = "pnpm"
"#,
        )
        .unwrap();

        let tools = ProjectTools::load(&root).unwrap();
        assert!(tools.loaded());
        assert_eq!(tools.cargo().display(), "cargo +stable");
        assert_eq!(tools.wasm_pack().display(), "/opt/tools/wasm-pack");
        assert_eq!(tools.package_manager("npm").display(), "pnpm");
        assert!(tools.package_manager_override().is_some());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tool_command_reports_program_name_without_path_or_exe() {
        let tool = ToolCommand::Simple("/opt/bin/pnpm.exe".into());
        assert_eq!(tool.program_name(), "pnpm");
    }
}
