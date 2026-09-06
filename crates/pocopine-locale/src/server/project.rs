use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use super::{
    CatalogSource, CfgSet, Compilation, Diagnostic, Extraction, Severity, SourceContext, Span,
    compile_catalogs, extract_rust, extract_template,
};
use crate::{CatalogAudience, LocaleConfig};

const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_FILES: usize = 10_000;

/// A Cargo target selected by the build driver. Library roots normally appear
/// once for browser cfg and once for host cfg; server/worker binaries are host
/// roots. The driver supplies rustc cfg and the resolved feature set for each.
#[derive(Clone, Debug)]
pub struct SourceTarget {
    pub path: PathBuf,
    pub cfg: CfgSet,
    pub audience: CatalogAudience,
}

#[derive(Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    pub source: String,
}

#[derive(Debug, Default)]
pub struct ProjectDiscovery {
    pub config: Option<LocaleConfig>,
    /// Sorted absolute filenames. Spans index this list, including catalogs
    /// and configuration, so consumers use the same diagnostics everywhere.
    pub files: Vec<SourceFile>,
    pub catalogs: Vec<CatalogSource>,
    pub extracted: Extraction,
}

impl ProjectDiscovery {
    pub fn has_errors(&self) -> bool {
        self.extracted
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    pub fn compile(&self) -> Compilation {
        if self.has_errors() {
            return Compilation {
                diagnostics: self.extracted.diagnostics.clone(),
                ..Default::default()
            };
        }
        let Some(config) = &self.config else {
            return Compilation {
                diagnostics: vec![Diagnostic::error(
                    "pocopine.toml requires a [locale] section",
                )],
                ..Default::default()
            };
        };
        let locales = match config.validate() {
            Ok(locales) => locales,
            Err(error) => {
                return Compilation {
                    diagnostics: vec![Diagnostic::error(error.to_string())],
                    ..Default::default()
                };
            }
        };
        let mut compiled = compile_catalogs(&locales, &self.catalogs, &self.extracted.references);
        compiled
            .diagnostics
            .extend(self.extracted.diagnostics.iter().cloned());
        compiled.diagnostics.sort_by(|a, b| {
            (a.span.file, a.span.start, &a.message).cmp(&(b.span.file, b.span.start, &b.message))
        });
        compiled
    }
}

/// Read config/catalogs and follow the selected Rust module graphs. Unused .rs
/// and .poco files, tests behind cfg(test), and disabled components contribute
/// no references. This is the IO boundary; compile_catalogs remains pure.
pub fn discover_project(root: &Path, targets: &[SourceTarget]) -> ProjectDiscovery {
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            return ProjectDiscovery {
                extracted: Extraction {
                    diagnostics: vec![Diagnostic::error(format!(
                        "cannot open project {}: {error}",
                        root.display()
                    ))],
                    ..Default::default()
                },
                ..Default::default()
            };
        }
    };
    let mut walker = ProjectWalker {
        root,
        by_path: BTreeMap::new(),
        active: Vec::new(),
        out: ProjectDiscovery::default(),
    };
    walker.config();
    // Sort the supplied roots too: the result must not depend on Cargo's order.
    let mut targets = targets.iter().collect::<Vec<_>>();
    targets.sort_by_key(|target| (&target.path, target.audience == CatalogAudience::Host));
    for target in targets {
        let path = walker.root.join(&target.path);
        let module_dir = path.parent().unwrap_or(&walker.root).to_owned();
        walker.rust(
            &path,
            &module_dir,
            SourceContext {
                file: 0,
                module: String::new(),
                audience: target.audience,
                offset: 0,
            },
            &target.cfg,
            Span::UNKNOWN,
        );
    }
    walker.finish()
}

struct ProjectWalker {
    root: PathBuf,
    by_path: BTreeMap<PathBuf, u32>,
    active: Vec<PathBuf>,
    out: ProjectDiscovery,
}

impl ProjectWalker {
    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.out
            .extracted
            .diagnostics
            .push(Diagnostic::error(message).at(span));
    }

    fn read(&mut self, path: &Path, span: Span) -> Option<u32> {
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.error(format!("cannot read {}: {error}", path.display()), span);
                return None;
            }
        };
        if let Some(id) = self.by_path.get(&path) {
            return Some(*id);
        }
        if self.out.files.len() >= MAX_SOURCE_FILES {
            self.error("locale discovery exceeds 10000 source files", span);
            return None;
        }
        let result = fs::metadata(&path).and_then(|meta| {
            if meta.len() > MAX_SOURCE_BYTES {
                return Err(std::io::Error::other("locale source exceeds 16 MiB"));
            }
            fs::read_to_string(&path)
        });
        match result {
            Ok(source) => {
                let id = self.out.files.len() as u32;
                self.by_path.insert(path.clone(), id);
                self.out.files.push(SourceFile { path, source });
                Some(id)
            }
            Err(error) => {
                self.error(format!("cannot read {}: {error}", path.display()), span);
                None
            }
        }
    }

    fn config(&mut self) {
        #[derive(Deserialize)]
        struct Config {
            locale: Option<LocaleConfig>,
        }
        let Some(file) = self.read(&self.root.join("pocopine.toml"), Span::UNKNOWN) else {
            return;
        };
        let source = &self.out.files[file as usize].source;
        let config: Config = match toml::from_str(source) {
            Ok(config) => config,
            Err(error) => {
                let range = error.span().unwrap_or(0..source.len());
                self.error(
                    error.to_string(),
                    Span {
                        file,
                        start: range.start as u32,
                        end: range.end as u32,
                    },
                );
                return;
            }
        };
        let location = Span {
            file,
            start: 0,
            end: source.len() as u32,
        };
        let Some(config) = config.locale else {
            self.error("pocopine.toml requires a [locale] section", location);
            return;
        };
        if let Err(error) = config.validate() {
            self.error(error.to_string(), location);
            return;
        }
        for locale in &config.locales {
            let path = self.root.join("locales").join(format!("{locale}.json"));
            if let Some(file) = self.read(&path, location) {
                self.out.catalogs.push(CatalogSource {
                    locale: locale.clone(),
                    file,
                    source: self.out.files[file as usize].source.clone(),
                });
            }
        }
        self.out.config = Some(config);
    }

    fn rust(
        &mut self,
        path: &Path,
        module_dir: &Path,
        mut context: SourceContext,
        cfg: &CfgSet,
        span: Span,
    ) {
        let Some(file) = self.read(path, span) else {
            return;
        };
        let path = self.out.files[file as usize].path.clone();
        if self.active.contains(&path) || self.active.len() >= 128 {
            self.error(
                format!(
                    "recursive or excessively deep Rust module graph at {}",
                    path.display()
                ),
                span,
            );
            return;
        }
        context.file = file;
        let found = extract_rust(&self.out.files[file as usize].source, &context, cfg);
        self.out.extracted.append(found.extracted);
        self.active.push(path.clone());
        for template in found.templates {
            let Some(template_path) = self.template_path(&path, &template.path, template.span)
            else {
                continue;
            };
            if let Some(file) = self.read(&template_path, template.span) {
                let mut context = template.context;
                context.file = file;
                context.offset = 0;
                self.out.extracted.append(extract_template(
                    &self.out.files[file as usize].source,
                    &context,
                ));
            }
        }
        for include in found.includes {
            let included = path.parent().unwrap().join(&include.path);
            let mut base = if include.directory_from_file {
                path.parent().unwrap().to_owned()
            } else {
                module_dir.to_owned()
            };
            for parent in include.parents {
                base.push(parent);
            }
            self.rust(&included, &base, include.context, cfg, include.span);
        }
        for module in found.modules {
            let mut base = if module.directory_from_file {
                path.parent().unwrap().to_owned()
            } else {
                module_dir.to_owned()
            };
            for parent in &module.parents {
                base.push(parent);
            }
            let next = if let Some(override_path) = &module.path {
                // #[path] outside inline modules is relative to the current
                // source file; inside inline modules it uses their directory.
                if module.parents.is_empty() {
                    path.parent().unwrap().join(override_path)
                } else {
                    base.join(override_path)
                }
            } else {
                let flat = base.join(format!("{}.rs", module.name));
                let nested = base.join(&module.name).join("mod.rs");
                match (flat.is_file(), nested.is_file()) {
                    (true, false) => flat,
                    (false, true) => nested,
                    (true, true) => {
                        self.error(
                            format!(
                                "ambiguous Rust module: both {} and {} exist",
                                flat.display(),
                                nested.display()
                            ),
                            module.span,
                        );
                        continue;
                    }
                    (false, false) => {
                        self.error(
                            format!(
                                "Rust module source missing: expected {} or {}",
                                flat.display(),
                                nested.display()
                            ),
                            module.span,
                        );
                        continue;
                    }
                }
            };
            let parent = next.parent().unwrap_or(module_dir);
            let child_dir = if next.file_name().is_some_and(|name| name == "mod.rs") {
                parent.to_owned()
            } else {
                parent.join(next.file_stem().unwrap_or_default())
            };
            self.rust(&next, &child_dir, module.context, cfg, module.span);
        }
        self.active.pop();
    }

    fn template_path(&mut self, source: &Path, value: &str, span: Span) -> Option<PathBuf> {
        let local = source.parent().unwrap().join(value);
        if local.is_file() {
            return Some(local);
        }
        let direct = self.root.join(value);
        if direct.is_file() {
            return Some(direct);
        }
        // Match the component macro's IDE/manifest fallback. Never choose one
        // of several same-basename templates based on filesystem order.
        let basename = Path::new(value).file_name()?;
        let mut matches = Vec::new();
        find_templates(&self.root, basename, 0, &mut matches);
        if matches.len() == 1 {
            return matches.pop();
        }
        self.error(
            format!(
                "cannot resolve component template {value:?}: expected {} ({} fallback matches)",
                local.display(),
                matches.len()
            ),
            span,
        );
        None
    }

    fn finish(mut self) -> ProjectDiscovery {
        let mut order = (0..self.out.files.len()).collect::<Vec<_>>();
        order.sort_by_key(|&index| &self.out.files[index].path);
        let mut remap = vec![0; order.len()];
        for (new, &old) in order.iter().enumerate() {
            remap[old] = new as u32;
        }
        for reference in &mut self.out.extracted.references {
            reference.span.file = remap[reference.span.file as usize];
        }
        for diagnostic in &mut self.out.extracted.diagnostics {
            if let Some(&file) = remap.get(diagnostic.span.file as usize) {
                diagnostic.span.file = file;
            }
        }
        for catalog in &mut self.out.catalogs {
            catalog.file = remap[catalog.file as usize];
        }
        self.out.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.out.catalogs.sort_by(|a, b| a.locale.cmp(&b.locale));
        self.out.extracted.references.sort_by(|a, b| {
            (
                a.span.file,
                a.span.start,
                &a.key,
                a.audience == CatalogAudience::Host,
            )
                .cmp(&(
                    b.span.file,
                    b.span.start,
                    &b.key,
                    b.audience == CatalogAudience::Host,
                ))
        });
        self.out.extracted.diagnostics.sort_by(|a, b| {
            (a.span.file, a.span.start, &a.message).cmp(&(b.span.file, b.span.start, &b.message))
        });
        self.out.extracted.diagnostics.dedup_by(|a, b| {
            a.span == b.span && a.message == b.message && a.severity == b.severity
        });
        self.out
    }
}

fn find_templates(dir: &Path, basename: &std::ffi::OsStr, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some("target" | "node_modules" | ".git" | "pkg" | "dist" | ".idea" | ".vscode")
        ) {
            continue;
        }
        if path.is_file() && name == basename {
            out.push(path);
        } else if path.is_dir() {
            find_templates(&path, basename, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, path: &str, source: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    fn target(path: &str, audience: CatalogAudience) -> SourceTarget {
        SourceTarget {
            path: path.into(),
            audience,
            cfg: CfgSet::from_rustc(if audience == CatalogAudience::Browser {
                "target_arch=\"wasm32\""
            } else {
                "target_arch=\"x86_64\""
            })
            .unwrap(),
        }
    }

    #[test]
    fn discovers_only_active_graphs_and_compiles_target_separated_catalogs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "pocopine.toml",
            "[locale]\ndefault='en'\nlocales=['en','fr']\n",
        );
        write(
            root,
            "locales/en.json",
            r#"{"auth.submit":"Sign in","auth.denied":"Denied","auth.worker":"Email","auth.orphan":"Unused"}"#,
        );
        write(root, "locales/fr.json", r#"{"auth.submit":"Connexion"}"#);
        write(
            root,
            "src/lib.rs",
            "mod auth; #[cfg(test)] mod missing_test;",
        );
        write(
            root,
            "src/auth.rs",
            r#"#[component] struct Form; #[cfg(not(target_arch="wasm32"))] mod server;"#,
        );
        write(
            root,
            "src/Form.poco",
            r#"<button pp-text="$t.auth.submit"></button>"#,
        );
        write(
            root,
            "src/auth/server.rs",
            "fn check() { t::auth::denied(locale); }",
        );
        write(
            root,
            "src/bin/worker.rs",
            "mod auth { fn send() { t::auth::worker(locale); } }",
        );
        write(
            root,
            "src/unused.rs",
            "fn unused() { t::auth::orphan(locale); }",
        );
        write(
            root,
            "src/Unused.poco",
            r#"<p pp-text="$t.auth.orphan"></p>"#,
        );
        let mut targets = vec![
            target("src/lib.rs", CatalogAudience::Browser),
            target("src/lib.rs", CatalogAudience::Host),
            target("src/bin/worker.rs", CatalogAudience::Host),
        ];
        let found = discover_project(root, &targets);
        assert!(!found.has_errors(), "{:?}", found.extracted.diagnostics);
        assert!(
            !found
                .files
                .iter()
                .any(|f| f.path.ends_with("unused.rs") || f.path.ends_with("Unused.poco"))
        );
        let compiled = found.compile();
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        assert_eq!(compiled.messages.len(), 3);
        assert!(compiled.messages["auth.submit"].browser);
        assert!(!compiled.messages["auth.denied"].browser);
        assert!(!compiled.messages["auth.worker"].browser);
        targets.reverse();
        let reversed = discover_project(root, &targets).compile();
        assert_eq!(compiled.build_id, reversed.build_id);
        assert_eq!(
            compiled
                .catalogs
                .iter()
                .map(|c| &c.bytes)
                .collect::<Vec<_>>(),
            reversed
                .catalogs
                .iter()
                .map(|c| &c.bytes)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn path_attributes_inline_modules_and_ambiguity_follow_rust_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "pocopine.toml",
            "[locale]\ndefault='en'\nlocales=['en']\n",
        );
        write(root, "locales/en.json", "{}");
        write(root, "src/lib.rs", "mod cart;");
        write(
            root,
            "src/cart.rs",
            r#"#[path="special.rs"] mod special; mod inner { #[path="selected.rs"] mod chosen; }"#,
        );
        write(root, "src/special.rs", "");
        write(root, "src/cart/inner/selected.rs", "");
        let targets = [target("src/lib.rs", CatalogAudience::Browser)];
        let found = discover_project(root, &targets);
        assert!(!found.has_errors(), "{:?}", found.extracted.diagnostics);
        assert!(
            found
                .files
                .iter()
                .any(|f| f.path.ends_with("src/special.rs"))
        );
        assert!(
            found
                .files
                .iter()
                .any(|f| f.path.ends_with("src/cart/inner/selected.rs"))
        );
        write(root, "src/cart/mod.rs", "");
        let found = discover_project(root, &targets);
        assert!(found.has_errors());
        assert!(found.compile().catalogs.is_empty());
    }

    #[test]
    fn outer_inline_path_resets_non_mod_directory_and_includes_are_reached() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "pocopine.toml",
            "[locale]\ndefault='en'\nlocales=['en']\n",
        );
        write(root, "locales/en.json", r#"{"auth.shared":"Reached"}"#);
        write(root, "src/lib.rs", "mod auth;");
        write(
            root,
            "src/auth.rs",
            r#"include!("shared.rs"); #[path="redirect"] mod storage { mod child; }"#,
        );
        write(
            root,
            "src/shared.rs",
            "fn shared() {t::auth::shared(locale);}",
        );
        write(root, "src/redirect/child.rs", "");
        let targets = [target("src/lib.rs", CatalogAudience::Host)];
        let out = discover_project(root, &targets);
        assert!(!out.has_errors(), "{:?}", out.extracted.diagnostics);
        assert_eq!(out.extracted.references[0].key, "auth.shared");
        assert!(
            out.files
                .iter()
                .any(|f| f.path.ends_with("src/redirect/child.rs"))
        );
        write(root, "src/shared.rs", r#"include!("auth.rs");"#);
        let out = discover_project(root, &targets);
        assert!(out.has_errors());
        assert!(out.compile().catalogs.is_empty());
    }

    #[test]
    fn public_function_reexports_are_retained_but_namespace_imports_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "pocopine.toml",
            "[locale]\ndefault='en'\nlocales=['en']\n",
        );
        write(
            root,
            "locales/en.json",
            r#"{"auth.denied":"Denied","auth.orphan":"Unused"}"#,
        );
        write(
            root,
            "src/lib.rs",
            r#"mod auth { pub use crate::t::auth::denied as failure; use crate::t::auth as messages; }
            fn main() {auth::failure(locale);}"#,
        );
        let out = discover_project(root, &[target("src/lib.rs", CatalogAudience::Host)]).compile();
        assert!(!out.has_errors(), "{:?}", out.diagnostics);
        assert_eq!(
            out.messages.keys().map(String::as_str).collect::<Vec<_>>(),
            ["auth.denied"]
        );
    }
}
