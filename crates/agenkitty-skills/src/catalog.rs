//! The discovered skill set and its progressive-disclosure read APIs.
//!
//! Level 1 is [`SkillCatalog::render_index`] (the always-on prompt block),
//! level 2 is [`SkillCatalog::body`] (loaded on activation), level 3 is
//! [`SkillCatalog::read_resource`] (bundled files on demand). Every output is
//! sanitized and byte-bounded here, in the library, so no consumer can obtain
//! the raw form through a convenience path (RFC-121 S4).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use serde_json::Value as JsonValue;

use crate::error::SkillError;
use crate::meta::{ClaudeExt, SkillDiagnostic, SkillLimits, SkillMeta};
use crate::sanitize::{bound_text, sanitize_multiline};
use crate::{confine, subst};

/// One loaded, validated skill.
#[derive(Clone, Debug)]
pub struct LoadedSkill {
    pub meta: SkillMeta,
    pub ext: ClaudeExt,
    /// Full frontmatter (string-keyed JSON values) for tooling round-trips.
    /// Never rendered into prompts by the framework.
    pub raw: BTreeMap<String, JsonValue>,
    /// Resolved (canonical) skill directory — the confinement root.
    pub root: PathBuf,
    /// The configured discovery root this skill came from.
    pub source_root: PathBuf,
    /// SHA-256 of the `SKILL.md` bytes (change detection, future pinning).
    pub digest: [u8; 32],
    /// Set when an earlier root claimed the same name.
    pub shadowed_by: Option<PathBuf>,
    pub(crate) body: String,
    pub(crate) resources: Vec<String>,
    pub(crate) index_entry: String,
}

impl LoadedSkill {
    /// The sanitized, capped, possibly redacted line body used by
    /// [`SkillCatalog::render_index`].
    pub fn index_entry(&self) -> &str {
        &self.index_entry
    }

    /// Relative paths of bundled resource files (sorted, bounded inventory).
    pub fn resources(&self) -> &[String] {
        &self.resources
    }

    pub fn digest_hex(&self) -> String {
        self.digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Short digest prefix for log fields (RFC-121 S8).
    pub fn digest_short(&self) -> String {
        let hex = self.digest_hex();
        hex[..8.min(hex.len())].to_string()
    }
}

/// The full body returned by [`SkillCatalog::body`] (level 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillBody {
    pub name: String,
    /// Sanitized, argument-substituted, byte-bounded body text.
    pub body: String,
    /// Whether the body was cut at `body_byte_limit`.
    pub truncated: bool,
    /// Bundled resource inventory the caller may `read_resource`.
    pub resources: Vec<String>,
}

/// Byte-window options for [`SkillCatalog::read_resource`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadOpts {
    /// Byte offset into the file. Default 0.
    pub offset: Option<u64>,
    /// Window size in bytes; clamped to the configured maximum.
    pub limit: Option<usize>,
}

/// One bounded window of a bundled resource file (level 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceChunk {
    /// Sanitized text of the window. Non-UTF-8 bytes are replaced.
    pub content: String,
    /// Bytes actually read from the file for this window.
    pub bytes_read: usize,
    pub file_size: u64,
    /// Whether the window reached the end of the file.
    pub eof: bool,
}

/// The result of one [`SkillLoader::discover`](crate::SkillLoader::discover)
/// scan: winners by name, shadowed duplicates, and every finding.
#[derive(Clone, Debug, Default)]
pub struct SkillCatalog {
    pub(crate) skills: BTreeMap<String, LoadedSkill>,
    pub(crate) shadowed: Vec<LoadedSkill>,
    pub(crate) diagnostics: Vec<SkillDiagnostic>,
    pub(crate) limits: SkillLimits,
}

impl SkillCatalog {
    pub fn get(&self, name: &str) -> Option<&LoadedSkill> {
        self.skills.get(name)
    }

    /// Loaded skills in deterministic (name) order.
    pub fn iter(&self) -> impl Iterator<Item = &LoadedSkill> {
        self.skills.values()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Skills that lost a name collision to an earlier root.
    pub fn shadowed(&self) -> &[LoadedSkill] {
        &self.shadowed
    }

    pub fn diagnostics(&self) -> &[SkillDiagnostic] {
        &self.diagnostics
    }

    pub fn limits(&self) -> &SkillLimits {
        &self.limits
    }

    /// Level 1: one `- <name>: <entry>` line per model-invocable skill,
    /// truncated to whole lines within `budget` bytes, with an explicit
    /// omission line when the budget cuts skills off (nothing is silently
    /// invisible). Deterministic by name.
    pub fn render_index(&self, budget: usize) -> String {
        let mut out = String::new();
        let mut omitted = 0usize;
        for skill in self
            .skills
            .values()
            .filter(|skill| !skill.ext.disable_model_invocation)
        {
            let line = format!("- {}: {}\n", skill.meta.name, skill.index_entry);
            if out.len() + line.len() <= budget {
                out.push_str(&line);
            } else {
                omitted += 1;
            }
        }
        if omitted > 0 {
            out.push_str(&format!(
                "(… {omitted} more skill{} omitted — raise skills.index_byte_budget)\n",
                if omitted == 1 { "" } else { "s" }
            ));
        }
        out
    }

    /// Level 2: the sanitized, argument-substituted, bounded body.
    ///
    /// Deliberately does not check `disable-model-invocation` — the host
    /// invocation path uses this too. The runtime `skill.use` tool enforces
    /// that rule for model-initiated activation.
    pub fn body(&self, name: &str, args: Option<&str>) -> Result<SkillBody, SkillError> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| SkillError::NotFound(format!("skill `{name}` not found")))?;
        let sanitized = sanitize_multiline(&skill.body);
        let substituted = match args {
            Some(raw) if !raw.trim().is_empty() => {
                subst::substitute(&sanitized, raw, &skill.ext.arguments)
            }
            _ => sanitized,
        };
        let (body, truncated) = bound_text(&substituted, self.limits.body_byte_limit);
        Ok(SkillBody {
            name: skill.meta.name.clone(),
            body,
            truncated,
            resources: skill.resources.clone(),
        })
    }

    /// Level 3: a confined, bounded window of a bundled resource file.
    pub fn read_resource(
        &self,
        name: &str,
        rel: &str,
        opts: ReadOpts,
    ) -> Result<ResourceChunk, SkillError> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| SkillError::NotFound(format!("skill `{name}` not found")))?;
        let path = confine::confined_resource_path(&skill.root, rel)?;
        let metadata = std::fs::metadata(&path)
            .map_err(|err| SkillError::Io(format!("stat resource `{rel}`: {err}")))?;
        if metadata.is_dir() {
            return Err(SkillError::Validation(format!(
                "resource `{rel}` is a directory"
            )));
        }
        let file_size = metadata.len();
        let offset = opts.offset.unwrap_or(0);
        if offset >= file_size {
            return Ok(ResourceChunk {
                content: String::new(),
                bytes_read: 0,
                file_size,
                eof: true,
            });
        }
        let limit = opts
            .limit
            .unwrap_or(self.limits.resource_read_default_bytes)
            .clamp(1, self.limits.resource_read_max_bytes);

        let mut file = File::open(&path)
            .map_err(|err| SkillError::Io(format!("open resource `{rel}`: {err}")))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| SkillError::Io(format!("seek resource `{rel}`: {err}")))?;
        let mut buffer = Vec::with_capacity(limit.min(file_size as usize));
        file.take(limit as u64)
            .read_to_end(&mut buffer)
            .map_err(|err| SkillError::Io(format!("read resource `{rel}`: {err}")))?;

        let bytes_read = buffer.len();
        let eof = offset + bytes_read as u64 >= file_size;
        let content = sanitize_multiline(&String::from_utf8_lossy(&buffer));
        Ok(ResourceChunk {
            content,
            bytes_read,
            file_size,
            eof,
        })
    }
}
