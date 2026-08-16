//! Skill discovery: ordered-root scanning, validation, shadowing, digests.
//!
//! Roots are an ordered list; earlier roots win name collisions and the loser
//! is kept as a shadowed entry (RFC-121 § Discovery). Discovery is infallible
//! at the catalog level: per-skill failures become diagnostics, IO failure of
//! a whole root becomes a root diagnostic, and a missing root is simply
//! skipped (a default root not existing is the normal case).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use agenkitty_core::secrets::looks_like_secret;
use pocopine_crypto::{sha256, sha256_hex};

use super::catalog::{LoadedSkill, SkillCatalog};
use super::frontmatter::parse_skill_file;
use super::meta::{Severity, SkillDiagnostic, SkillLimits};
use super::sanitize::{bound_text, sanitize_single_line};

/// Discovers skills under an ordered list of roots.
pub struct SkillLoader {
    roots: Vec<PathBuf>,
    limits: SkillLimits,
}

impl SkillLoader {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            limits: SkillLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: SkillLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Scan all roots. Never fails: every problem is a diagnostic on the
    /// returned catalog.
    pub fn discover(&self) -> SkillCatalog {
        let mut catalog = SkillCatalog {
            limits: self.limits,
            ..SkillCatalog::default()
        };
        // Same target reachable through more than one root loads once.
        let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

        for root in &self.roots {
            let canonical_root = match root.canonicalize() {
                Ok(canonical) => canonical,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    catalog.diagnostics.push(SkillDiagnostic {
                        severity: Severity::Warning,
                        dir: root.clone(),
                        name: None,
                        rule: "root.io",
                        message: format!("cannot resolve skills root: {err}"),
                    });
                    continue;
                }
            };
            let entries = match fs::read_dir(&canonical_root) {
                Ok(entries) => entries,
                Err(err) => {
                    catalog.diagnostics.push(SkillDiagnostic {
                        severity: Severity::Warning,
                        dir: root.clone(),
                        name: None,
                        rule: "root.io",
                        message: format!("cannot read skills root: {err}"),
                    });
                    continue;
                }
            };

            let mut names: Vec<String> = entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect();
            names.sort_unstable();

            for entry_name in names {
                let entry_path = canonical_root.join(&entry_name);
                self.load_one(root, &entry_name, &entry_path, &mut seen_dirs, &mut catalog);
            }
        }
        catalog
    }

    fn load_one(
        &self,
        source_root: &Path,
        entry_name: &str,
        entry_path: &Path,
        seen_dirs: &mut HashSet<PathBuf>,
        catalog: &mut SkillCatalog,
    ) {
        // Follow a symlinked skill directory once; the resolved directory
        // becomes the confinement root (RFC-121 § Discovery).
        let skill_dir = match entry_path.canonicalize() {
            Ok(dir) if dir.is_dir() => dir,
            Ok(_) => return, // plain file in a skills root — not a skill
            Err(err) => {
                catalog.diagnostics.push(SkillDiagnostic {
                    severity: Severity::Warning,
                    dir: entry_path.to_path_buf(),
                    name: None,
                    rule: "skill.io",
                    message: format!("cannot resolve skill directory: {err}"),
                });
                return;
            }
        };
        if !seen_dirs.insert(skill_dir.clone()) {
            return; // same resolved directory already loaded via an earlier root
        }

        let skill_md = skill_dir.join("SKILL.md");
        // `fs::metadata` follows symlinks, so a symlinked SKILL.md is sized by
        // its target — the cap cannot be bypassed via a link whose own
        // metadata is only the target-path length.
        let metadata = match fs::metadata(&skill_md) {
            Ok(metadata) => metadata,
            Err(_) => {
                catalog.diagnostics.push(SkillDiagnostic::new(
                    Severity::Info,
                    entry_path,
                    None,
                    "skill.no-skill-md",
                    "directory has no SKILL.md; skipped",
                ));
                return;
            }
        };
        if metadata.len() > self.limits.skill_md_max_bytes as u64 {
            catalog.diagnostics.push(SkillDiagnostic::new(
                Severity::Error,
                entry_path,
                Some(entry_name),
                "skill.oversize",
                format!(
                    "SKILL.md is {} bytes (limit {})",
                    metadata.len(),
                    self.limits.skill_md_max_bytes
                ),
            ));
            return;
        }
        let bytes = match fs::read(&skill_md) {
            Ok(bytes) => bytes,
            Err(err) => {
                catalog.diagnostics.push(SkillDiagnostic::new(
                    Severity::Error,
                    entry_path,
                    Some(entry_name),
                    "skill.io",
                    format!("cannot read SKILL.md: {err}"),
                ));
                return;
            }
        };
        // Digest before the UTF-8 conversion consumes the buffer (no clone);
        // the hex form is computed once here per the codec-crypto house rule.
        let digest = sha256(&bytes);
        let digest_hex = sha256_hex(&bytes);
        let source = match String::from_utf8(bytes) {
            Ok(source) => source,
            Err(_) => {
                catalog.diagnostics.push(SkillDiagnostic::new(
                    Severity::Error,
                    entry_path,
                    Some(entry_name),
                    "skill.encoding",
                    "SKILL.md is not valid UTF-8",
                ));
                return;
            }
        };

        let parsed = parse_skill_file(entry_name, &source);
        for pending in &parsed.diagnostics {
            catalog.diagnostics.push(SkillDiagnostic::new(
                pending.severity,
                entry_path,
                Some(entry_name),
                pending.rule,
                pending.message.clone(),
            ));
        }
        let Some(meta) = parsed.meta else {
            return; // error diagnostics above carry the exclusion
        };

        let index_entry = self.index_entry(
            &meta.description,
            parsed.ext.when_to_use.as_deref(),
            entry_path,
            &meta.name,
            &mut catalog.diagnostics,
        );
        let resources = self.resource_inventory(&skill_dir, entry_path, &mut catalog.diagnostics);

        let skill = LoadedSkill {
            meta,
            ext: parsed.ext,
            raw: parsed.raw,
            root: skill_dir,
            source_root: source_root.to_path_buf(),
            digest,
            digest_hex,
            shadowed_by: None,
            body: parsed.body,
            resources,
            index_entry,
        };

        match catalog.skills.entry(skill.meta.name.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(skill);
            }
            std::collections::btree_map::Entry::Occupied(winner) => {
                catalog.diagnostics.push(SkillDiagnostic::new(
                    Severity::Info,
                    entry_path,
                    Some(&skill.meta.name),
                    "name.shadowed",
                    format!(
                        "shadowed by `{}` from an earlier root",
                        winner.get().root.display()
                    ),
                ));
                let mut shadowed = skill;
                shadowed.shadowed_by = Some(winner.get().root.clone());
                catalog.shadowed.push(shadowed);
            }
        }
    }

    /// Build the sanitized single-line index entry: description
    /// (+ when_to_use), redacted wholesale when it trips the shared
    /// secret-content classifier (RFC-121 S2), capped at `entry_byte_cap`.
    fn index_entry(
        &self,
        description: &str,
        when_to_use: Option<&str>,
        entry_path: &Path,
        name: &str,
        diagnostics: &mut Vec<SkillDiagnostic>,
    ) -> String {
        let mut combined = description.to_string();
        if let Some(when) = when_to_use {
            combined.push(' ');
            combined.push_str(when);
        }
        let sanitized = sanitize_single_line(&combined);
        if looks_like_secret(&sanitized) {
            diagnostics.push(SkillDiagnostic {
                severity: Severity::Warning,
                dir: entry_path.to_path_buf(),
                name: Some(name.to_string()),
                rule: "index.redacted",
                message: "description carries credential-looking content; redacted in the index"
                    .to_string(),
            });
            return "[redacted: credential-looking description]".to_string();
        }
        let (entry, _) = bound_text(&sanitized, self.limits.entry_byte_cap);
        entry
    }

    /// Bounded inventory of non-`SKILL.md` files, relative slash-normalized
    /// paths, sorted. Symlinked directories are not descended (loop safety);
    /// files remain reachable via `read_resource`, which confines on the
    /// resolved path. Dot-directories (`.git`, …) are not descended, and
    /// paths the confinement layer would refuse (`.env*`, key material) are
    /// not advertised — `skill.use` must never list a resource `skill.read`
    /// will reject.
    fn resource_inventory(
        &self,
        skill_dir: &Path,
        entry_path: &Path,
        diagnostics: &mut Vec<SkillDiagnostic>,
    ) -> Vec<String> {
        let mut resources = Vec::new();
        let mut truncated = false;
        let mut stack = vec![skill_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            let mut children: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            children.sort_unstable();
            for child in children {
                if resources.len() >= self.limits.resource_inventory_max {
                    truncated = true;
                    break;
                }
                let Ok(metadata) = fs::symlink_metadata(&child) else {
                    continue;
                };
                if metadata.is_dir() {
                    let hidden = child
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with('.'));
                    if !hidden {
                        stack.push(child);
                    }
                    continue;
                }
                let Ok(relative) = child.strip_prefix(skill_dir) else {
                    continue;
                };
                if super::confine::is_secret_path(relative) {
                    continue;
                }
                let display = super::confine::normalize_slashes(&relative.display().to_string());
                if display == "SKILL.md" {
                    continue;
                }
                resources.push(display);
            }
            if truncated {
                break;
            }
        }
        resources.sort_unstable();
        if truncated {
            diagnostics.push(SkillDiagnostic {
                severity: Severity::Info,
                dir: entry_path.to_path_buf(),
                name: None,
                rule: "resources.truncated",
                message: format!(
                    "resource inventory capped at {} entries; further files remain readable by path",
                    self.limits.resource_inventory_max
                ),
            });
        }
        resources
    }
}

/// The default project-relative discovery roots: the vendor-neutral standard
/// path first, then the Claude Code compatibility path (RFC-121 § Discovery).
pub fn default_roots(project_root: &Path) -> Vec<PathBuf> {
    vec![
        project_root.join(".agents/skills"),
        project_root.join(".claude/skills"),
    ]
}

/// Run the full load/validate pipeline on exactly one skill directory — the
/// `skills-ref validate ./my-skill` counterpart. The directory name is the
/// name the skill must declare.
pub fn load_skill_dir(path: &Path, limits: SkillLimits) -> SkillCatalog {
    let mut catalog = SkillCatalog {
        limits,
        ..SkillCatalog::default()
    };
    let Some(entry_name) = path.file_name().and_then(|name| name.to_str()) else {
        catalog.diagnostics.push(SkillDiagnostic {
            severity: Severity::Error,
            dir: path.to_path_buf(),
            name: None,
            rule: "skill.io",
            message: "path has no usable directory name".to_string(),
        });
        return catalog;
    };
    let loader = SkillLoader {
        roots: Vec::new(),
        limits,
    };
    let source_root = path.parent().unwrap_or(path).to_path_buf();
    let mut seen_dirs = HashSet::new();
    loader.load_one(&source_root, entry_name, path, &mut seen_dirs, &mut catalog);
    catalog
}
