//! Public skill metadata types: validated standard fields, typed Claude Code
//! extension fields, diagnostics, and loader limits.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

/// The agentskills.io standard frontmatter fields, validated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillMeta {
    /// 1–64 chars, `a-z0-9-`, no leading/trailing/consecutive hyphens, equal
    /// to the skill directory name.
    pub name: String,
    /// 1–1024 chars, non-empty after trim.
    pub description: String,
    pub license: Option<String>,
    /// 1–500 chars if present.
    pub compatibility: Option<String>,
    /// String → string map. Scalar values are coerced to strings with a
    /// warning; nested values are dropped with a warning.
    pub metadata: BTreeMap<String, String>,
    /// Normalized from a space/comma-separated string or a YAML list.
    /// Semantics are host policy — attenuation-only per RFC-121 S5.
    pub allowed_tools: Vec<String>,
}

/// Claude Code extension frontmatter, parsed and surfaced — never executed by
/// the loader (RFC-121 compatibility matrix).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClaudeExt {
    /// Appended to `description` in the rendered index (1536-byte combined cap).
    pub when_to_use: Option<String>,
    /// `true` excludes the skill from the rendered index; hosts must also
    /// refuse model-initiated activation.
    pub disable_model_invocation: bool,
    /// `false` means only the model/agent should invoke it; surfaced for host
    /// slash-menu UX. Default `true`.
    pub user_invocable: bool,
    /// Attenuation-only, like `allowed_tools`.
    pub disallowed_tools: Vec<String>,
    /// Host hint: model override while the skill is active.
    pub model: Option<String>,
    /// Host hint: effort override while the skill is active.
    pub effort: Option<String>,
    /// `context: fork` (+ `agent`, `background`) — a subagent-execution hint
    /// for the host orchestrator (RFC-118: subagent orchestration is app
    /// territory).
    pub fork: Option<ForkHint>,
    /// Glob patterns gating automatic activation; surfaced only.
    pub paths: Vec<String>,
    /// Named positional arguments for `$name` substitution.
    pub arguments: Vec<String>,
    /// Autocomplete hint, e.g. `[issue-number]`.
    pub argument_hint: Option<String>,
}

/// The `context: fork` execution hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForkHint {
    /// Subagent type to use, when specified.
    pub agent: Option<String>,
    /// `background: false` means the invoker should wait for the fork.
    /// Default `true` (Claude Code parity).
    pub background: bool,
}

impl Default for ForkHint {
    fn default() -> Self {
        Self {
            agent: None,
            background: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Spec violation — the skill is excluded from the catalog.
    Error,
    /// Degraded but loadable (redacted index text, dropped metadata entry…).
    Warning,
    /// Informational (shadowed skill, ignored extension field…).
    Info,
}

/// One structured loader finding. Discovery never fails hard on a single
/// skill — every exclusion or degradation is one of these, enumerable via the
/// library and `list`/`validate --json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillDiagnostic {
    pub severity: Severity,
    /// The skill directory the finding is about (a root, for root-level IO).
    pub dir: PathBuf,
    /// The skill name when one could be parsed.
    pub name: Option<String>,
    /// Stable machine-readable rule id, e.g. `name.charset`,
    /// `frontmatter.parse`, `ext.unsupported`.
    pub rule: &'static str,
    pub message: String,
}

impl SkillDiagnostic {
    pub fn new(
        severity: Severity,
        dir: impl Into<PathBuf>,
        name: Option<&str>,
        rule: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            dir: dir.into(),
            name: name.map(str::to_string),
            rule,
            message: message.into(),
        }
    }
}

/// Byte budgets and caps applied by the loader and the read APIs. All outputs
/// of the catalog are bounded by construction (RFC-121 S4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkillLimits {
    /// Hard cap on `SKILL.md` size; larger files are excluded loudly.
    pub skill_md_max_bytes: usize,
    /// Per-skill cap on the index entry (description + when_to_use).
    pub entry_byte_cap: usize,
    /// Default budget for [`render_index`](crate::SkillCatalog::render_index)
    /// callers that don't pass their own.
    pub index_byte_budget: usize,
    /// Cap on the body returned by [`body`](crate::SkillCatalog::body);
    /// excess is truncated with a marker and flagged.
    pub body_byte_limit: usize,
    /// Default window for [`read_resource`](crate::SkillCatalog::read_resource).
    pub resource_read_default_bytes: usize,
    /// Maximum window a caller may request per resource read.
    pub resource_read_max_bytes: usize,
    /// Cap on the per-skill resource inventory; excess files stay readable
    /// but are not listed.
    pub resource_inventory_max: usize,
}

impl Default for SkillLimits {
    fn default() -> Self {
        Self {
            skill_md_max_bytes: 1024 * 1024,
            entry_byte_cap: 1536,
            index_byte_budget: 16 * 1024,
            body_byte_limit: 64 * 1024,
            resource_read_default_bytes: 64 * 1024,
            resource_read_max_bytes: 256 * 1024,
            resource_inventory_max: 512,
        }
    }
}
