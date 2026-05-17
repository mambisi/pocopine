use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use pocopine_client_codegen::{self as client_codegen, DiscoveryPolicy};
use serde_json::{json, Map, Value};

use crate::tools;

pub const CLIENT_BUNDLE_URL: &str = "/pkg/pocopine-client.js";

const CLIENT_BUNDLE_PATH: &str = "pkg/pocopine-client.js";
const GENERATED_DIR: &str = "target/pocopine/client-modules";
const ENTRY_FILE: &str = "entry.js";
const TSCONFIG_FILE: &str = "tsconfig.json";
const DEFAULT_ESBUILD_VERSION: &str = "^0.25.0";
const DEFAULT_TYPESCRIPT_VERSION: &str = "^5.0.0";

pub fn init(project: &Path) -> Result<()> {
    let project = project
        .canonicalize()
        .with_context(|| format!("resolve {}", project.display()))?;
    ensure_package_json(&project)?;
    println!(
        "✓ client toolkit ready ({})",
        project.join("package.json").display()
    );
    Ok(())
}

pub fn install(project: &Path) -> Result<()> {
    let project = project
        .canonicalize()
        .with_context(|| format!("resolve {}", project.display()))?;
    ensure_package_json(&project)?;
    run_package_manager(&project, PackageAction::Install)
}

pub fn add(project: &Path, packages: &[String], dev: bool) -> Result<()> {
    if packages.is_empty() {
        bail!("pocopine js add requires at least one package");
    }
    let project = project
        .canonicalize()
        .with_context(|| format!("resolve {}", project.display()))?;
    ensure_package_json(&project)?;
    run_package_manager(&project, PackageAction::Add { packages, dev })
}

pub fn build(project: &Path, release: bool) -> Result<usize> {
    let project = project
        .canonicalize()
        .with_context(|| format!("resolve {}", project.display()))?;
    let modules = client_codegen::discover_client_modules(&project, DiscoveryPolicy::TypedOnly)?;
    let bundle_path = project.join(CLIENT_BUNDLE_PATH);
    if modules.is_empty() {
        remove_stale_bundle(&bundle_path)?;
        return Ok(0);
    }

    ensure_package_json(&project)?;
    ensure_node_modules(&project)?;
    let generated_dir = project.join(GENERATED_DIR);
    run_typescript_check(&project, &modules, &generated_dir)?;
    let entry = client_codegen::write_runtime_entry(&project, &modules, GENERATED_DIR, ENTRY_FILE)?;
    if let Some(parent) = bundle_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    println!(
        "▶ bundling {} client module{} → {}",
        modules.len(),
        if modules.len() == 1 { "" } else { "s" },
        bundle_path
            .strip_prefix(&project)
            .unwrap_or(bundle_path.as_path())
            .display()
    );
    run_esbuild(&project, &entry, &bundle_path, release)?;

    Ok(modules.len())
}

pub fn inject_html_if_needed(root: &Path, target: &Path, body: Vec<u8>) -> Vec<u8> {
    if !is_html(target) || !root.join(CLIENT_BUNDLE_PATH).is_file() {
        return body;
    }
    let mut html = match String::from_utf8(body) {
        Ok(html) => html,
        Err(err) => return err.into_bytes(),
    };
    if html.contains(CLIENT_BUNDLE_URL) {
        return html.into_bytes();
    }

    let script = format!(r#"<script type="module" src="{CLIENT_BUNDLE_URL}"></script>"#);
    if let Some(idx) = html.find("</head>") {
        html.insert_str(idx, &format!("  {script}\n"));
    } else if let Some(idx) = html.find("<script") {
        html.insert_str(idx, &format!("{script}\n"));
    } else {
        html.push('\n');
        html.push_str(&script);
        html.push('\n');
    }
    html.into_bytes()
}

pub(crate) struct Status {
    pub(crate) module_count: usize,
    pub(crate) package_json: bool,
    pub(crate) has_esbuild_dependency: bool,
    pub(crate) has_typescript_dependency: bool,
    pub(crate) node_modules: bool,
    pub(crate) local_esbuild: bool,
    pub(crate) local_typescript: bool,
    pub(crate) package_manager: PackageManager,
    pub(crate) package_manager_source: PackageManagerSource,
    pub(crate) package_manager_overridden: bool,
    pub(crate) package_manager_conflicts: Vec<String>,
    pub(crate) package_manager_command: tools::ToolCommand,
}

pub(crate) fn status(project: &Path) -> Result<Status> {
    let project = project
        .canonicalize()
        .with_context(|| format!("resolve {}", project.display()))?;
    let modules = client_codegen::discover_client_modules(&project, DiscoveryPolicy::TypedOnly)?;
    let package_json = project.join("package.json").is_file();
    let has_esbuild_dependency = package_json_has_dependency(&project, "esbuild")?.unwrap_or(false);
    let has_typescript_dependency =
        package_json_has_dependency(&project, "typescript")?.unwrap_or(false);
    let package_manager = detect_package_manager(&project);
    let project_tools = tools::ProjectTools::load(&project)?;
    let package_manager_command = project_tools.package_manager(package_manager.manager.binary());
    Ok(Status {
        module_count: modules.len(),
        package_json,
        has_esbuild_dependency,
        has_typescript_dependency,
        node_modules: project.join("node_modules").is_dir(),
        local_esbuild: local_esbuild(&project).is_some(),
        local_typescript: local_typescript(&project).is_some(),
        package_manager: package_manager.manager,
        package_manager_source: package_manager.source,
        package_manager_overridden: project_tools.package_manager_override().is_some(),
        package_manager_conflicts: package_manager.conflicts,
        package_manager_command,
    })
}

fn run_esbuild(project: &Path, entry: &Path, output: &Path, release: bool) -> Result<()> {
    let mut cmd = esbuild_command(project)?;
    cmd.arg(entry)
        .arg("--bundle")
        .arg("--format=esm")
        .arg("--target=es2020")
        .arg(format!("--outfile={}", output.display()));
    if release {
        cmd.arg("--minify");
    } else {
        cmd.arg("--sourcemap");
    }
    cmd.current_dir(project);
    let status = cmd
        .status()
        .context("invoke esbuild through pocopine client toolkit")?;
    if !status.success() {
        bail!("client module bundle failed with {status}");
    }
    Ok(())
}

fn run_typescript_check(
    project: &Path,
    modules: &[client_codegen::ClientModule],
    generated_dir: &Path,
) -> Result<()> {
    let tsconfig = write_typecheck_config(project, modules, generated_dir)?;
    println!(
        "▶ type-checking {} client module{} with tsc",
        modules.len(),
        if modules.len() == 1 { "" } else { "s" },
    );
    let mut cmd = typescript_command(project)?;
    cmd.arg("--project").arg(&tsconfig);
    cmd.current_dir(project);
    let status = cmd
        .status()
        .context("invoke tsc through pocopine client toolkit")?;
    if !status.success() {
        bail!("client module type check failed with {status}");
    }
    Ok(())
}

fn write_typecheck_config(
    project: &Path,
    modules: &[client_codegen::ClientModule],
    generated_dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(generated_dir)
        .with_context(|| format!("create {}", generated_dir.display()))?;
    let files: Vec<_> = modules
        .iter()
        .map(|module| client_codegen::relative_import_path(generated_dir, module.path()))
        .collect();
    let config = json!({
        "compilerOptions": {
            "allowJs": false,
            "isolatedModules": true,
            "module": "ESNext",
            "moduleResolution": "Bundler",
            "noEmit": true,
            "resolveJsonModule": true,
            "skipLibCheck": true,
            "strict": true,
            "target": "ES2020",
        },
        "files": files,
    });
    let path = project.join(GENERATED_DIR).join(TSCONFIG_FILE);
    let text = serde_json::to_string_pretty(&config)?;
    std::fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

fn esbuild_command(project: &Path) -> Result<Command> {
    if let Some(local) = local_esbuild(project) {
        return Ok(Command::new(local));
    }
    package_manager_command(project, PackageAction::Exec { bin: "esbuild" })
}

fn typescript_command(project: &Path) -> Result<Command> {
    if let Some(local) = local_typescript(project) {
        return Ok(Command::new(local));
    }
    package_manager_command(project, PackageAction::Exec { bin: "tsc" })
}

fn local_esbuild(project: &Path) -> Option<PathBuf> {
    local_node_bin(project, "esbuild")
}

fn local_typescript(project: &Path) -> Option<PathBuf> {
    local_node_bin(project, "tsc")
}

fn local_node_bin(project: &Path, bin: &str) -> Option<PathBuf> {
    let bin = if cfg!(windows) {
        project.join(format!("node_modules/.bin/{bin}.cmd"))
    } else {
        project.join(format!("node_modules/.bin/{bin}"))
    };
    bin.is_file().then_some(bin)
}

fn ensure_package_json(project: &Path) -> Result<()> {
    let path = project.join("package.json");
    let mut changed = false;
    let mut value = if path.exists() {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str::<Value>(&text).with_context(|| format!("parse {}", path.display()))?
    } else {
        changed = true;
        json!({
            "private": true,
            "type": "module",
            "devDependencies": {
                "esbuild": DEFAULT_ESBUILD_VERSION
            }
        })
    };

    let Some(root) = value.as_object_mut() else {
        bail!("{} must contain a JSON object", path.display());
    };
    if !root.contains_key("private") {
        root.insert("private".into(), Value::Bool(true));
        changed = true;
    }
    if !root.contains_key("type") {
        root.insert("type".into(), Value::String("module".into()));
        changed = true;
    }
    if !has_dependency(root, "esbuild") {
        let dev_deps = object_field(root, "devDependencies")?;
        dev_deps.insert(
            "esbuild".into(),
            Value::String(DEFAULT_ESBUILD_VERSION.into()),
        );
        changed = true;
    }
    if !has_dependency(root, "typescript") {
        let dev_deps = object_field(root, "devDependencies")?;
        dev_deps.insert(
            "typescript".into(),
            Value::String(DEFAULT_TYPESCRIPT_VERSION.into()),
        );
        changed = true;
    }

    if changed {
        let text = serde_json::to_string_pretty(&value)?;
        std::fs::write(&path, format!("{text}\n"))
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

fn has_dependency(root: &Map<String, Value>, name: &str) -> bool {
    ["dependencies", "devDependencies"].iter().any(|field| {
        root.get(*field)
            .and_then(Value::as_object)
            .is_some_and(|m| m.contains_key(name))
    })
}

fn object_field<'a>(
    root: &'a mut Map<String, Value>,
    name: &str,
) -> Result<&'a mut Map<String, Value>> {
    let entry = root
        .entry(name.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    entry
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("package.json field `{name}` must be an object"))
}

fn ensure_node_modules(project: &Path) -> Result<()> {
    if project.join("node_modules").is_dir()
        && local_esbuild(project).is_some()
        && local_typescript(project).is_some()
    {
        return Ok(());
    }
    println!("▶ installing Pocopine client toolkit dependencies");
    run_package_manager(project, PackageAction::Install)
}

fn run_package_manager(project: &Path, action: PackageAction<'_>) -> Result<()> {
    let mut cmd = package_manager_command(project, action)?;
    cmd.current_dir(project);
    let display = tools::format_command(&cmd);
    println!("▶ {display}");
    let status = cmd.status().with_context(|| format!("invoke {display}"))?;
    if !status.success() {
        bail!("{display} failed with {status}");
    }
    Ok(())
}

fn package_manager_command(project: &Path, action: PackageAction<'_>) -> Result<Command> {
    let manager = detect_package_manager(project);
    let project_tools = tools::ProjectTools::load(project)?;
    let tool = project_tools.package_manager(manager.manager.binary());
    if tools::resolve_program(&tool).is_none() {
        bail!(
            "{} not found. Install it once or set `[tools].package-manager` in {}.",
            tool.display(),
            project_tools.config_path().display()
        );
    }
    let command_manager = package_manager_for_tool(manager.manager, &tool);
    let mut cmd = tool.command();
    cmd.args(package_manager_args(command_manager, action));
    Ok(cmd)
}

enum PackageAction<'a> {
    Install,
    Add { packages: &'a [String], dev: bool },
    Exec { bin: &'static str },
}

fn package_manager_args(manager: PackageManager, action: PackageAction<'_>) -> Vec<String> {
    match action {
        PackageAction::Install => vec!["install".into()],
        PackageAction::Add { packages, dev } => {
            let mut args = vec![match manager {
                PackageManager::Npm => "install".into(),
                PackageManager::Pnpm | PackageManager::Yarn | PackageManager::Bun => "add".into(),
            }];
            if dev {
                args.push("-D".into());
            }
            args.extend(packages.iter().cloned());
            args
        }
        PackageAction::Exec { bin } => match manager {
            PackageManager::Bun => vec!["x".into(), bin.into()],
            PackageManager::Pnpm | PackageManager::Npm | PackageManager::Yarn => {
                vec!["exec".into(), bin.into()]
            }
        },
    }
}

fn package_manager_for_tool(detected: PackageManager, tool: &tools::ToolCommand) -> PackageManager {
    std::iter::once(tool.program_name())
        .chain(tool.default_args().iter().map(String::as_str))
        .find_map(package_manager_from_name)
        .unwrap_or(detected)
}

fn package_manager_from_name(name: &str) -> Option<PackageManager> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let base = base
        .strip_suffix(".exe")
        .unwrap_or(base)
        .split('@')
        .next()
        .unwrap_or(base)
        .to_ascii_lowercase();
    match base.as_str() {
        "pnpm" => Some(PackageManager::Pnpm),
        "npm" | "npx" => Some(PackageManager::Npm),
        "yarn" => Some(PackageManager::Yarn),
        "bun" | "bunx" => Some(PackageManager::Bun),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageManager {
    Pnpm,
    Npm,
    Yarn,
    Bun,
}

impl PackageManager {
    pub(crate) fn binary(self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageManagerSource {
    Lockfile(&'static str),
    Default,
}

impl PackageManagerSource {
    pub(crate) fn describe(self) -> String {
        match self {
            Self::Lockfile(lockfile) => format!("selected by {lockfile}"),
            Self::Default => "defaulted to pnpm".into(),
        }
    }
}

struct PackageManagerDetection {
    manager: PackageManager,
    source: PackageManagerSource,
    conflicts: Vec<String>,
}

fn package_json_has_dependency(project: &Path, name: &str) -> Result<Option<bool>> {
    let path = project.join("package.json");
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let Some(root) = value.as_object() else {
        bail!("{} must contain a JSON object", path.display());
    };
    Ok(Some(has_dependency(root, name)))
}

fn detect_package_manager(project: &Path) -> PackageManagerDetection {
    let lockfiles = [
        (PackageManager::Pnpm, "pnpm-lock.yaml"),
        (PackageManager::Npm, "package-lock.json"),
        (PackageManager::Yarn, "yarn.lock"),
        (PackageManager::Bun, "bun.lockb"),
        (PackageManager::Bun, "bun.lock"),
    ];
    let found: Vec<_> = lockfiles
        .iter()
        .copied()
        .filter(|(_, lockfile)| project.join(lockfile).exists())
        .collect();

    if let Some((manager, lockfile)) = found.first().copied() {
        let conflicts = found
            .iter()
            .skip(1)
            .map(|(manager, lockfile)| format!("{lockfile} ({})", manager.label()))
            .collect();
        PackageManagerDetection {
            manager,
            source: PackageManagerSource::Lockfile(lockfile),
            conflicts,
        }
    } else {
        PackageManagerDetection {
            manager: PackageManager::Pnpm,
            source: PackageManagerSource::Default,
            conflicts: Vec::new(),
        }
    }
}

fn remove_stale_bundle(bundle_path: &Path) -> Result<()> {
    if bundle_path.exists() {
        std::fs::remove_file(bundle_path)
            .with_context(|| format!("remove stale {}", bundle_path.display()))?;
    }
    Ok(())
}

fn is_html(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("html" | "htm")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_names_follow_component_kebab_case() {
        assert_eq!(
            client_codegen::client_module_name("FirebaseAuth.client.ts").unwrap(),
            "firebase-auth"
        );
        assert_eq!(
            client_codegen::client_module_name("PostHog.client.ts").unwrap(),
            "post-hog"
        );
        assert_eq!(
            client_codegen::client_module_name("pine-post-hog.client.ts").unwrap(),
            "pine-post-hog"
        );
    }

    #[test]
    fn relative_imports_are_stable_for_generated_entry() {
        let from = Path::new("/app/target/pocopine/client-modules");
        let to = Path::new("/app/src/components/FirebaseAuth.client.ts");
        assert_eq!(
            client_codegen::relative_import_path(from, to),
            "../../../src/components/FirebaseAuth.client.ts"
        );
    }

    #[test]
    fn generated_entry_accepts_side_effect_only_modules() {
        let unique = format!(
            "pocopine-cli-entry-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/Firebase.client.ts"), "initializeFirebase();").unwrap();

        let modules =
            client_codegen::discover_client_modules(&root, DiscoveryPolicy::TypedOnly).unwrap();
        let entry = client_codegen::write_runtime_entry(&root, &modules, GENERATED_DIR, ENTRY_FILE)
            .unwrap();
        let source = std::fs::read_to_string(entry).unwrap();
        assert!(source.contains("import * as __pp_client_0"));
        assert!(source.contains("if (\"default\" in __pp_client_0)"));
        assert!(source.contains("R[\"firebase\"] = __pp_client_0.default"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generated_typecheck_config_checks_client_modules() {
        let unique = format!(
            "pocopine-cli-tsconfig-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let generated = root.join(GENERATED_DIR);
        std::fs::create_dir_all(root.join("src/firebase")).unwrap();
        let module_path = root.join("src/firebase/Firebase.client.ts");
        std::fs::write(&module_path, "export default {};").unwrap();
        let modules =
            client_codegen::discover_client_modules(&root, DiscoveryPolicy::TypedOnly).unwrap();

        let config_path = write_typecheck_config(&root, &modules, &generated).unwrap();
        let source = std::fs::read_to_string(config_path).unwrap();
        assert!(source.contains("\"moduleResolution\": \"Bundler\""));
        assert!(source.contains("\"strict\": true"));
        assert!(source.contains("../../../src/firebase/Firebase.client.ts"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn injects_client_bundle_before_html_head_closes() {
        let unique = format!(
            "pocopine-cli-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(root.join("pkg/pocopine-client.js"), "").unwrap();
        let html = b"<html><head></head><body></body></html>".to_vec();
        let out = inject_html_if_needed(&root, &root.join("index.html"), html);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains(CLIENT_BUNDLE_URL));
        assert!(out.find(CLIENT_BUNDLE_URL).unwrap() < out.find("</head>").unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovery_requires_ts_and_rejects_tsx() {
        let unique = format!(
            "pocopine-cli-discover-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/FirebaseAuth.client.ts"),
            "export default () => ({})",
        )
        .unwrap();
        let modules =
            client_codegen::discover_client_modules(&root, DiscoveryPolicy::TypedOnly).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name(), "firebase-auth");

        std::fs::write(
            root.join("src/Legacy.client.js"),
            "export default () => ({})",
        )
        .unwrap();
        let err = client_codegen::discover_client_modules(&root, DiscoveryPolicy::TypedOnly)
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be typed"));
        std::fs::remove_file(root.join("src/Legacy.client.js")).unwrap();

        std::fs::write(
            root.join("src/ReactThing.client.tsx"),
            "export default () => ({})",
        )
        .unwrap();
        let err = client_codegen::discover_client_modules(&root, DiscoveryPolicy::TypedOnly)
            .unwrap_err()
            .to_string();
        assert!(err.contains("TSX"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn package_manager_detection_reports_source_and_conflicts() {
        let unique = format!(
            "pocopine-cli-package-manager-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package-lock.json"), "").unwrap();
        std::fs::write(root.join("yarn.lock"), "").unwrap();

        let detected = detect_package_manager(&root);
        assert_eq!(detected.manager, PackageManager::Npm);
        assert_eq!(
            detected.source,
            PackageManagerSource::Lockfile("package-lock.json")
        );
        assert_eq!(detected.conflicts, vec!["yarn.lock (yarn)"]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn package_manager_actions_use_manager_specific_args() {
        let packages = vec!["firebase".to_string()];
        assert_eq!(
            package_manager_args(
                PackageManager::Pnpm,
                PackageAction::Add {
                    packages: &packages,
                    dev: false,
                }
            ),
            vec!["add", "firebase"]
        );
        assert_eq!(
            package_manager_args(
                PackageManager::Npm,
                PackageAction::Add {
                    packages: &packages,
                    dev: true,
                }
            ),
            vec!["install", "-D", "firebase"]
        );
        assert_eq!(
            package_manager_args(PackageManager::Bun, PackageAction::Exec { bin: "esbuild" }),
            vec!["x", "esbuild"]
        );
    }

    #[test]
    fn package_manager_for_tool_understands_wrappers() {
        let corepack = tools::ToolCommand::Detailed {
            command: "corepack".into(),
            args: vec!["pnpm".into()],
        };
        assert_eq!(
            package_manager_for_tool(PackageManager::Npm, &corepack),
            PackageManager::Pnpm
        );

        let bun = tools::ToolCommand::Simple("/opt/bin/bun.exe".into());
        assert_eq!(
            package_manager_for_tool(PackageManager::Pnpm, &bun),
            PackageManager::Bun
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_uses_local_node_modules_esbuild() {
        use std::os::unix::fs::PermissionsExt;

        let unique = format!(
            "pocopine-cli-local-esbuild-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        std::fs::write(
            root.join("src/Tiny.client.ts"),
            r#"import { value } from "tiny-sdk";
export default () => ({ value });
"#,
        )
        .unwrap();

        let esbuild = root.join("node_modules/.bin/esbuild");
        std::fs::write(
            &esbuild,
            r#"#!/bin/sh
set -eu
entry=""
out=""
for arg in "$@"; do
  case "$arg" in
    --outfile=*) out="${arg#--outfile=}" ;;
    --*) ;;
    *) if [ -z "$entry" ]; then entry="$arg"; fi ;;
  esac
done
grep -q 'Tiny.client.ts' "$entry"
grep -q 'tiny-sdk' src/Tiny.client.ts
printf '/* fake esbuild bundle */\n' > "$out"
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&esbuild).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&esbuild, permissions).unwrap();
        let tsc = root.join("node_modules/.bin/tsc");
        std::fs::write(
            &tsc,
            r#"#!/bin/sh
set -eu
test "$1" = "--project"
test -f "$2"
grep -q 'Tiny.client.ts' "$2"
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&tsc).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&tsc, permissions).unwrap();

        let bundled = build(&root, false).unwrap();
        assert_eq!(bundled, 1);
        assert!(std::fs::read_to_string(root.join(CLIENT_BUNDLE_PATH))
            .unwrap()
            .contains("fake esbuild bundle"));

        let _ = std::fs::remove_dir_all(root);
    }
}
