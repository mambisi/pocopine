//! Shared client-module discovery and generated-binding scaffolding.
//!
//! The CLI and `build.rs` helper both need the same view of managed
//! client modules. Keeping discovery and the Rust-facing schema here
//! prevents the bundler path and rust-analyzer path from drifting.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const GENERATED_BINDINGS_FILE: &str = "pocopine_client_modules.rs";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryPolicy {
    /// Stable managed modules: `.client.ts` only. `.client.js` is
    /// rejected because generated Rust bindings need an explicit typed
    /// API surface.
    TypedOnly,
    /// Transitional runtime bundling mode. Kept for older examples or
    /// experiments that still use `.client.js`; do not use for typed
    /// code generation.
    LegacyJsAndTs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ClientModuleSource {
    TypeScript,
    JavaScript,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientModule {
    path: PathBuf,
    name: String,
    rust_module: String,
    source: ClientModuleSource,
}

impl ClientModule {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn rust_module(&self) -> &str {
        &self.rust_module
    }

    pub fn source(&self) -> ClientModuleSource {
        self.source
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientModuleSchema {
    pub modules: Vec<ModuleSchema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModuleSchema {
    pub name: String,
    pub rust_module: String,
    pub methods: Vec<ClientMethod>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientMethod {
    pub name: String,
    pub rust_name: String,
    pub kind: ClientMethodKind,
    pub params: Vec<ClientParam>,
    pub output: TypeExpr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ClientMethodKind {
    Async,
    Subscription,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientParam {
    pub name: String,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TypeExpr {
    Null,
    Bool,
    String,
    Number,
    Option(Box<TypeExpr>),
    Vec(Box<TypeExpr>),
    Object(Vec<TypeField>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TypeField {
    pub name: String,
    pub rust_name: String,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug)]
pub struct BindingOptions {
    /// Path to the runtime crate used by generated code. App crates
    /// normally use `::pocopine`.
    pub runtime_crate: String,
}

impl Default for BindingOptions {
    fn default() -> Self {
        Self {
            runtime_crate: "::pocopine".to_string(),
        }
    }
}

pub fn discover_client_modules(
    project: impl AsRef<Path>,
    policy: DiscoveryPolicy,
) -> Result<Vec<ClientModule>> {
    let project = project.as_ref();
    let src = project.join("src");
    if !src.is_dir() {
        return Ok(Vec::new());
    }
    let mut modules = Vec::new();
    discover_in(&src, policy, &mut modules)?;
    modules.sort_by(|a, b| a.path.cmp(&b.path));

    let mut seen = HashSet::new();
    for module in &modules {
        if !seen.insert(module.name.clone()) {
            bail!(
                "duplicate client module name `{}` from {}; rename one file so each .client module maps to a unique component tag",
                module.name,
                module.path.display()
            );
        }
    }
    Ok(modules)
}

pub fn write_runtime_entry(
    project: impl AsRef<Path>,
    modules: &[ClientModule],
    generated_dir: impl AsRef<Path>,
    entry_file: &str,
) -> Result<PathBuf> {
    let project = project.as_ref();
    let generated = project.join(generated_dir);
    std::fs::create_dir_all(&generated)
        .with_context(|| format!("create {}", generated.display()))?;
    let entry = generated.join(entry_file);
    let mut source = String::new();
    for (idx, module) in modules.iter().enumerate() {
        let rel = relative_import_path(&generated, module.path());
        source.push_str(&format!("import * as __pp_client_{idx} from \"{rel}\";\n"));
    }
    source.push_str("\nconst R = (window.__pp_client_modules ??= {});\n");
    for (idx, module) in modules.iter().enumerate() {
        let name = serde_json::to_string(module.name())?;
        source.push_str(&format!(
            "if (\"default\" in __pp_client_{idx}) R[{name}] = __pp_client_{idx}.default;\n"
        ));
    }
    source.push_str("export default R;\n");
    std::fs::write(&entry, source).with_context(|| format!("write {}", entry.display()))?;
    Ok(entry)
}

pub fn write_rust_bindings(
    project: impl AsRef<Path>,
    out_dir: impl AsRef<Path>,
) -> Result<PathBuf> {
    write_rust_bindings_with_options(project, out_dir, &BindingOptions::default())
}

pub fn write_rust_bindings_with_options(
    project: impl AsRef<Path>,
    out_dir: impl AsRef<Path>,
    options: &BindingOptions,
) -> Result<PathBuf> {
    let modules = discover_client_modules(project, DiscoveryPolicy::TypedOnly)?;
    let out_dir = out_dir.as_ref();
    std::fs::create_dir_all(out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    let path = out_dir.join(GENERATED_BINDINGS_FILE);
    std::fs::write(&path, generate_rust_bindings(&modules, options))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

pub fn generate_rust_bindings(modules: &[ClientModule], options: &BindingOptions) -> String {
    let runtime = &options.runtime_crate;
    let mut out = String::new();
    out.push_str("// @generated by pocopine-client-codegen. Do not edit by hand.\n\n");

    if modules.is_empty() {
        out.push_str("// No managed .client.ts modules were discovered.\n");
        return out;
    }

    for module in modules {
        let js_name = serde_json::to_string(module.name()).expect("module names serialize");
        out.push_str(&format!("pub mod {} {{\n", module.rust_module()));
        out.push_str("    #[derive(Clone, Debug)]\n");
        out.push_str("    pub struct Module {\n");
        out.push_str(&format!("        inner: {runtime}::ClientModule,\n"));
        out.push_str("    }\n\n");
        out.push_str(&format!(
            "    pub fn required() -> Result<Module, {runtime}::ClientModuleError> {{\n"
        ));
        out.push_str(&format!(
            "        {runtime}::ClientModule::required({js_name}).map(|inner| Module {{ inner }})\n"
        ));
        out.push_str("    }\n\n");
        out.push_str(&format!(
            "    pub fn optional() -> Result<Option<Module>, {runtime}::ClientModuleError> {{\n"
        ));
        out.push_str(&format!(
            "        {runtime}::ClientModule::optional({js_name}).map(|module| module.map(|inner| Module {{ inner }}))\n"
        ));
        out.push_str("    }\n\n");
        out.push_str("    impl Module {\n");
        out.push_str(&format!(
            "        pub fn raw(&self) -> &{runtime}::ClientModule {{\n"
        ));
        out.push_str("            &self.inner\n");
        out.push_str("        }\n");
        out.push('\n');
        out.push_str(&format!(
            "        pub async fn call_async<T>(&self, method: impl AsRef<str>) -> Result<T, {runtime}::ClientModuleError>\n"
        ));
        out.push_str("        where\n");
        out.push_str("            T: ::serde::de::DeserializeOwned,\n");
        out.push_str("        {\n");
        out.push_str("            self.inner.call_async(method).await\n");
        out.push_str("        }\n");
        out.push('\n');
        out.push_str(&format!(
            "        pub fn subscribe<T>(&self, scope: {runtime}::ScopeId, method: impl AsRef<str>, handler: impl FnMut(Result<T, {runtime}::ClientModuleError>) + 'static) -> Result<(), {runtime}::ClientModuleError>\n"
        ));
        out.push_str("        where\n");
        out.push_str("            T: ::serde::de::DeserializeOwned + 'static,\n");
        out.push_str("        {\n");
        out.push_str("            self.inner.subscribe(scope, method, handler)\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    out
}

pub fn client_module_name(file_name: &str) -> Result<String> {
    let base = file_name
        .strip_suffix(".client.ts")
        .or_else(|| file_name.strip_suffix(".client.js"))
        .unwrap_or(file_name);
    let name = to_kebab_case(base);
    if name.is_empty() {
        bail!("client module filename `{file_name}` does not contain a component name");
    }
    Ok(name)
}

pub fn rust_module_name(module_name: &str) -> String {
    let mut out = String::new();
    let mut prev_is_sep = true;
    for ch in module_name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_is_sep = false;
        } else if !prev_is_sep {
            out.push('_');
            prev_is_sep = true;
        }
    }
    let mut out = out.trim_matches('_').to_string();
    if out.is_empty() {
        out = "module".into();
    }
    if is_rust_keyword(&out) {
        out.push('_');
    }
    out
}

pub fn relative_import_path(from_dir: &Path, target: &Path) -> String {
    let from = normal_components(from_dir);
    let to = normal_components(target);
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".into());
    }
    parts.extend(to[common..].iter().cloned());
    let mut path = parts.join("/");
    if !path.starts_with('.') {
        path = format!("./{path}");
    }
    path
}

fn discover_in(dir: &Path, policy: DiscoveryPolicy, out: &mut Vec<ClientModule>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "node_modules" | ".git" | "pkg" | "dist" | ".idea" | ".vscode"
            ) {
                continue;
            }
            discover_in(&path, policy, out)?;
            continue;
        }
        if !entry.file_type()?.is_file() {
            continue;
        }

        let file_name = name.as_ref();
        if file_name.ends_with(".client.jsx") || file_name.ends_with(".client.tsx") {
            bail!(
                "unsupported client module `{}`: Pocopine supports managed .client.ts modules only; JSX/TSX and UI-framework islands are intentionally out of scope",
                path.display()
            );
        }
        if file_name.ends_with(".client.js") {
            if policy == DiscoveryPolicy::TypedOnly {
                bail!(
                    "unsupported managed client module `{}`: Pocopine managed modules must be typed; rename it to .client.ts and add explicit API types",
                    path.display()
                );
            }
            out.push(module_from_path(
                path,
                file_name,
                ClientModuleSource::JavaScript,
            )?);
        } else if file_name.ends_with(".client.ts") {
            out.push(module_from_path(
                path,
                file_name,
                ClientModuleSource::TypeScript,
            )?);
        }
    }
    Ok(())
}

fn module_from_path(
    path: PathBuf,
    file_name: &str,
    source: ClientModuleSource,
) -> Result<ClientModule> {
    let name = client_module_name(file_name)?;
    let rust_module = rust_module_name(&name);
    Ok(ClientModule {
        path,
        name,
        rust_module,
        source,
    })
}

fn to_kebab_case(input: &str) -> String {
    let mut out = String::new();
    let mut prev_is_sep = true;
    let mut prev_is_lower_or_digit = false;
    for ch in input.chars() {
        if ch == '_' || ch == '-' || ch == ' ' {
            if !prev_is_sep {
                out.push('-');
            }
            prev_is_sep = true;
            prev_is_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() {
            if prev_is_lower_or_digit && !prev_is_sep {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            prev_is_sep = false;
            prev_is_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_is_sep = false;
            prev_is_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out.trim_matches('-').to_string()
}

fn normal_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            Component::ParentDir => Some("..".into()),
            Component::CurDir => None,
            Component::RootDir | Component::Prefix(_) => {
                Some(c.as_os_str().to_string_lossy().to_string())
            }
        })
        .collect()
}

fn is_rust_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn client_names_follow_component_kebab_case() {
        assert_eq!(
            client_module_name("FirebaseAuth.client.ts").unwrap(),
            "firebase-auth"
        );
        assert_eq!(client_module_name("PostHog.client.ts").unwrap(), "post-hog");
        assert_eq!(
            client_module_name("pine-post-hog.client.ts").unwrap(),
            "pine-post-hog"
        );
    }

    #[test]
    fn rust_module_names_are_snake_case_and_keyword_safe() {
        assert_eq!(rust_module_name("firebase-auth"), "firebase_auth");
        assert_eq!(rust_module_name("type"), "type_");
    }

    #[test]
    fn typed_discovery_rejects_js_and_tsx() {
        let root = unique_root("pocopine-codegen-discover");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/Firebase.client.js"), "export default {};").unwrap();

        let err = discover_client_modules(&root, DiscoveryPolicy::TypedOnly)
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be typed"));

        std::fs::remove_file(root.join("src/Firebase.client.js")).unwrap();
        std::fs::write(root.join("src/Firebase.client.tsx"), "export default {};").unwrap();
        let err = discover_client_modules(&root, DiscoveryPolicy::TypedOnly)
            .unwrap_err()
            .to_string();
        assert!(err.contains("TSX"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_typed_modules() {
        let root = unique_root("pocopine-codegen-discover-typed");
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::write(
            root.join("src/nested/FirebaseAuth.client.ts"),
            "export default {};",
        )
        .unwrap();

        let modules = discover_client_modules(&root, DiscoveryPolicy::TypedOnly).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name(), "firebase-auth");
        assert_eq!(modules[0].rust_module(), "firebase_auth");
        assert_eq!(modules[0].source(), ClientModuleSource::TypeScript);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relative_imports_are_stable_for_generated_entry() {
        let from = Path::new("/app/target/pocopine/client-modules");
        let to = Path::new("/app/src/components/FirebaseAuth.client.ts");
        assert_eq!(
            relative_import_path(from, to),
            "../../../src/components/FirebaseAuth.client.ts"
        );
    }

    #[test]
    fn writes_runtime_entry() {
        let root = unique_root("pocopine-codegen-entry");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let module_path = root.join("src/Firebase.client.ts");
        std::fs::write(&module_path, "export default {};").unwrap();
        let modules = discover_client_modules(&root, DiscoveryPolicy::TypedOnly).unwrap();

        let entry = write_runtime_entry(
            &root,
            &modules,
            "target/pocopine/client-modules",
            "entry.js",
        )
        .unwrap();
        let source = std::fs::read_to_string(entry).unwrap();
        assert!(source.contains("import * as __pp_client_0"));
        assert!(source.contains("R[\"firebase\"] = __pp_client_0.default"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generated_bindings_include_module_facade() {
        let module = ClientModule {
            path: PathBuf::from("src/FirebaseAuth.client.ts"),
            name: "firebase-auth".into(),
            rust_module: "firebase_auth".into(),
            source: ClientModuleSource::TypeScript,
        };

        let source = generate_rust_bindings(&[module], &BindingOptions::default());
        assert!(source.contains("pub mod firebase_auth"));
        assert!(source.contains("ClientModule::required(\"firebase-auth\")"));
        assert!(source.contains("pub fn raw(&self)"));
    }
}
