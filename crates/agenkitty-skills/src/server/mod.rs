//! Host-only implementation of the skill loader. The whole crate is
//! host-side (filesystem discovery + reads), so per the client-server-modules
//! convention everything lives under this one module and the target gate
//! appears exactly once, in `lib.rs`.

mod catalog;
mod confine;
mod discover;
mod error;
mod frontmatter;
mod meta;
mod sanitize;
mod subst;

pub use catalog::{
    LoadedSkill, ReadOpts, ResourceChunk, SKILLS_PROMPT_HEADER, SkillBody, SkillCatalog,
};
pub use discover::{SkillLoader, default_roots, load_skill_dir};
pub use error::SkillError;
pub use meta::{ClaudeExt, ForkHint, Severity, SkillDiagnostic, SkillLimits, SkillMeta};
pub use sanitize::{sanitize_multiline, sanitize_single_line};
