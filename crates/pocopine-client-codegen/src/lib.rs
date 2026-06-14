//! Shared client-module discovery, runtime entry generation, and typed facade extraction.
//!
//! The CLI and `#[client_module]` macro both need the same view of managed
//! client modules. Keeping discovery and TypeScript API extraction here
//! prevents the bundler path and macro path from drifting.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Declaration, ExportDefaultDeclarationKind, Expression, Function,
    ObjectExpression, ObjectProperty, ObjectPropertyKind, Program, PropertyKey, Statement, TSType,
    TSTypeName,
};
use oxc_parser::Parser as OxcParser;
use oxc_span::{GetSpan, SourceType};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryPolicy {
    /// Stable managed modules: `.client.ts` only. `.client.js` is
    /// rejected because generated Rust facades need an explicit typed
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientFacadeMethod {
    pub name: String,
    pub rust_name: String,
    pub kind: ClientFacadeMethodKind,
    pub output: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientFacadeMethodKind {
    Async,
    Subscription,
}

pub fn extract_client_facade_methods(
    source: &str,
    path: impl AsRef<Path>,
) -> Result<Vec<ClientFacadeMethod>> {
    let source_type = SourceType::from_path(path).unwrap_or(SourceType::ts());
    extract_client_facade_methods_from_source(source, source_type)
}

pub fn extract_client_facade_methods_from_source(
    source: &str,
    source_type: SourceType,
) -> Result<Vec<ClientFacadeMethod>> {
    let allocator = Allocator::default();
    let parsed = OxcParser::new(&allocator, source, source_type).parse();
    if parsed.panicked {
        bail!("the TypeScript parser panicked while recovering from syntax errors");
    }
    if !parsed.errors.is_empty() {
        bail!("{} syntax error(s)", parsed.errors.len());
    }

    Ok(client_facade_methods_from_program(&parsed.program, source))
}

fn client_facade_methods_from_program(
    program: &Program<'_>,
    source: &str,
) -> Vec<ClientFacadeMethod> {
    let type_aliases = extract_type_aliases(program);
    let mut methods = Vec::new();

    for statement in &program.body {
        let Statement::ExportDefaultDeclaration(default_export) = statement else {
            continue;
        };
        let ExportDefaultDeclarationKind::ObjectExpression(object) = &default_export.declaration
        else {
            continue;
        };

        collect_client_module_object_methods(object, source, &type_aliases, &mut methods);
    }

    methods
}

fn extract_type_aliases<'a>(program: &'a Program<'a>) -> HashMap<String, &'a TSType<'a>> {
    let mut aliases = HashMap::new();
    for statement in &program.body {
        match statement {
            Statement::TSTypeAliasDeclaration(alias) => {
                aliases.insert(alias.id.name.as_str().to_string(), &alias.type_annotation);
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(Declaration::TSTypeAliasDeclaration(alias)) = &export.declaration {
                    aliases.insert(alias.id.name.as_str().to_string(), &alias.type_annotation);
                }
            }
            _ => {}
        }
    }
    aliases
}

fn collect_client_module_object_methods(
    object: &ObjectExpression<'_>,
    source: &str,
    type_aliases: &HashMap<String, &TSType<'_>>,
    methods: &mut Vec<ClientFacadeMethod>,
) {
    for property in &object.properties {
        let ObjectPropertyKind::ObjectProperty(property) = property else {
            continue;
        };
        let Some(name) = property_key_name(&property.key) else {
            continue;
        };
        let Some(function) = object_property_function(property) else {
            continue;
        };

        if function.r#async {
            if let Some(output) = async_method_output(function, source) {
                methods.push(ClientFacadeMethod {
                    name: name.to_string(),
                    rust_name: rust_method_name(name),
                    kind: ClientFacadeMethodKind::Async,
                    output,
                });
            }
        } else if let Some(output) = subscription_event_type(function, source, type_aliases) {
            methods.push(ClientFacadeMethod {
                name: name.to_string(),
                rust_name: rust_method_name(name),
                kind: ClientFacadeMethodKind::Subscription,
                output,
            });
        }
    }
}

fn property_key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(identifier) => Some(identifier.name.as_str()),
        PropertyKey::StringLiteral(literal) => Some(literal.value.as_str()),
        _ => None,
    }
}

fn object_property_function<'a>(property: &'a ObjectProperty<'a>) -> Option<&'a Function<'a>> {
    match &property.value {
        Expression::FunctionExpression(function) => Some(function),
        _ => None,
    }
}

fn async_method_output(function: &Function<'_>, source: &str) -> Option<String> {
    let return_type = &function.return_type.as_ref()?.type_annotation;
    let output = promise_type_argument(return_type)?;
    Some(rust_type_from_oxc_ts_type(output, source))
}

fn promise_type_argument<'a>(ty: &'a TSType<'a>) -> Option<&'a TSType<'a>> {
    match ty {
        TSType::TSTypeReference(reference) if ts_type_name(&reference.type_name)? == "Promise" => {
            reference.type_arguments.as_ref()?.params.first()
        }
        TSType::TSParenthesizedType(parenthesized) => {
            promise_type_argument(&parenthesized.type_annotation)
        }
        _ => None,
    }
}

fn subscription_event_type(
    function: &Function<'_>,
    source: &str,
    type_aliases: &HashMap<String, &TSType<'_>>,
) -> Option<String> {
    let callback = function
        .params
        .items
        .iter()
        .find(|param| binding_pattern_name(&param.pattern) == Some("callback"))?;
    let callback_ty = &callback.type_annotation.as_ref()?.type_annotation;
    callback_event_type(callback_ty, source, type_aliases)
}

fn binding_pattern_name<'a>(pattern: &'a BindingPattern<'a>) -> Option<&'a str> {
    match pattern {
        BindingPattern::BindingIdentifier(identifier) => Some(identifier.name.as_str()),
        _ => None,
    }
}

fn callback_event_type(
    callback_ty: &TSType<'_>,
    source: &str,
    type_aliases: &HashMap<String, &TSType<'_>>,
) -> Option<String> {
    match callback_ty {
        TSType::TSFunctionType(function) => {
            let event = &function
                .params
                .items
                .first()?
                .type_annotation
                .as_ref()?
                .type_annotation;
            Some(rust_type_from_oxc_ts_type(event, source))
        }
        TSType::TSTypeReference(reference) => {
            let name = ts_type_name(&reference.type_name)?;
            let alias = type_aliases.get(&name)?;
            callback_event_type(alias, source, type_aliases)
        }
        TSType::TSParenthesizedType(parenthesized) => {
            callback_event_type(&parenthesized.type_annotation, source, type_aliases)
        }
        _ => None,
    }
}

fn rust_type_from_oxc_ts_type(ty: &TSType<'_>, source: &str) -> String {
    match ty {
        TSType::TSStringKeyword(_) => "String".to_string(),
        TSType::TSBooleanKeyword(_) => "bool".to_string(),
        TSType::TSNumberKeyword(_) => "f64".to_string(),
        TSType::TSVoidKeyword(_) | TSType::TSUndefinedKeyword(_) | TSType::TSNullKeyword(_) => {
            "()".to_string()
        }
        TSType::TSArrayType(array) => {
            format!(
                "Vec<{}>",
                rust_type_from_oxc_ts_type(&array.element_type, source)
            )
        }
        TSType::TSTypeReference(reference) => {
            let Some(name) = ts_type_name(&reference.type_name) else {
                return ty.span().source_text(source).trim().to_string();
            };
            if name == "Array"
                && let Some(argument) = reference
                    .type_arguments
                    .as_ref()
                    .and_then(|arguments| arguments.params.first())
            {
                return format!("Vec<{}>", rust_type_from_oxc_ts_type(argument, source));
            }
            name
        }
        TSType::TSUnionType(union) => rust_type_from_oxc_union(union, source),
        TSType::TSParenthesizedType(parenthesized) => {
            rust_type_from_oxc_ts_type(&parenthesized.type_annotation, source)
        }
        _ => ty.span().source_text(source).trim().to_string(),
    }
}

fn rust_type_from_oxc_union(union: &oxc_ast::ast::TSUnionType<'_>, source: &str) -> String {
    let mut nullable = false;
    let mut non_null = Vec::new();
    for ty in &union.types {
        match ty {
            TSType::TSNullKeyword(_) | TSType::TSUndefinedKeyword(_) => nullable = true,
            _ => non_null.push(ty),
        }
    }

    if nullable && non_null.len() == 1 {
        return format!(
            "Option<{}>",
            rust_type_from_oxc_ts_type(non_null[0], source)
        );
    }

    union.span.source_text(source).trim().to_string()
}

fn ts_type_name(type_name: &TSTypeName<'_>) -> Option<String> {
    match type_name {
        TSTypeName::IdentifierReference(identifier) => Some(identifier.name.as_str().to_string()),
        TSTypeName::QualifiedName(qualified) => Some(format!(
            "{}.{}",
            ts_type_name(&qualified.left)?,
            qualified.right.name
        )),
        TSTypeName::ThisExpression(_) => None,
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

pub fn rust_method_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    let mut prev_is_sep = true;
    let mut prev_is_lower_or_digit = false;
    for ch in name.chars() {
        if ch == '-' || ch == '_' || ch == ' ' {
            if !prev_is_sep {
                out.push('_');
            }
            prev_is_sep = true;
            prev_is_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() {
            if prev_is_lower_or_digit && !prev_is_sep {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_is_sep = false;
            prev_is_lower_or_digit = false;
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_is_sep = false;
            prev_is_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    let mut out = out.trim_matches('_').to_string();
    if out.is_empty() {
        out = "method".into();
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
    fn rust_method_names_are_snake_case_and_keyword_safe() {
        assert_eq!(rust_method_name("signIn"), "sign_in");
        assert_eq!(
            rust_method_name("onAuthStateChanged"),
            "on_auth_state_changed"
        );
        assert_eq!(rust_method_name("type"), "type_");
    }

    #[test]
    fn extracts_async_and_subscription_methods_from_client_ts() {
        let source = r#"
            type AuthStateCallback = (user: FirebaseAuthUser | null) => void;

            export default {
              async signIn(): Promise<FirebaseAuthUser | null> {
                return null;
              },

              async signOut(): Promise<FirebaseAuthUser | null> {
                return null;
              },

              onAuthStateChanged(callback: AuthStateCallback): Unsubscribe {
                return () => {};
              },
            };
        "#;

        let methods = extract_client_facade_methods_from_source(source, SourceType::ts()).unwrap();
        assert_eq!(methods.len(), 3);
        assert_eq!(
            methods[0],
            ClientFacadeMethod {
                name: "signIn".to_string(),
                rust_name: "sign_in".to_string(),
                kind: ClientFacadeMethodKind::Async,
                output: "Option<FirebaseAuthUser>".to_string(),
            }
        );
        assert_eq!(
            methods[2],
            ClientFacadeMethod {
                name: "onAuthStateChanged".to_string(),
                rust_name: "on_auth_state_changed".to_string(),
                kind: ClientFacadeMethodKind::Subscription,
                output: "Option<FirebaseAuthUser>".to_string(),
            }
        );
    }

    #[test]
    fn maps_basic_typescript_types_to_rust() {
        let source = r#"
            export default {
              async stringValue(): Promise<string> {
                return "";
              },
              async boolValue(): Promise<boolean> {
                return true;
              },
              async numberValue(): Promise<number> {
                return 0;
              },
              async nullableValue(): Promise<FirebaseAuthUser | null> {
                return null;
              },
              async arrayValue(): Promise<string[]> {
                return [];
              },
            };
        "#;

        let methods = extract_client_facade_methods_from_source(source, SourceType::ts()).unwrap();
        let outputs: Vec<_> = methods
            .iter()
            .map(|method| method.output.as_str())
            .collect();

        assert_eq!(
            outputs,
            vec![
                "String",
                "bool",
                "f64",
                "Option<FirebaseAuthUser>",
                "Vec<String>"
            ]
        );
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
}
