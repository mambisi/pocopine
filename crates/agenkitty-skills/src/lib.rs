//! Agent Skills loader for the Agenkitty framework (RFC-121).
//!
//! Implements the agentskills.io open standard — `SKILL.md` frontmatter
//! validation, ordered-root discovery, progressive-disclosure reads — with
//! Claude Code extension fields parsed and surfaced but never executed.
//!
//! Two consumption modes:
//!
//! - **Library**: embed [`SkillLoader`] / [`SkillCatalog`] directly. Sync,
//!   deterministic, no runtime dependencies; every output is sanitized and
//!   byte-bounded by construction.
//! - **Binary**: the `agenkitty-skills` CLI (feature `cli`) exposes the same
//!   loader as `validate` / `list` / `inspect` / `index` with stable
//!   versioned JSON output.
//!
//! The `skill.*` tool family in the `agenkitty` crate builds on this; nothing
//! here executes scripts, dispatches hooks, or spawns anything.

#[cfg(not(target_arch = "wasm32"))]
mod catalog;
#[cfg(not(target_arch = "wasm32"))]
mod confine;
#[cfg(not(target_arch = "wasm32"))]
mod discover;
#[cfg(not(target_arch = "wasm32"))]
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod frontmatter;
#[cfg(not(target_arch = "wasm32"))]
mod meta;
#[cfg(not(target_arch = "wasm32"))]
mod sanitize;
#[cfg(not(target_arch = "wasm32"))]
mod subst;

#[cfg(all(not(target_arch = "wasm32"), feature = "cli"))]
pub mod cli;

#[cfg(not(target_arch = "wasm32"))]
pub use catalog::{LoadedSkill, ReadOpts, ResourceChunk, SkillBody, SkillCatalog};
#[cfg(not(target_arch = "wasm32"))]
pub use discover::{SkillLoader, default_roots};
#[cfg(not(target_arch = "wasm32"))]
pub use error::SkillError;
#[cfg(not(target_arch = "wasm32"))]
pub use meta::{ClaudeExt, ForkHint, Severity, SkillDiagnostic, SkillLimits, SkillMeta};
#[cfg(not(target_arch = "wasm32"))]
pub use sanitize::{sanitize_multiline, sanitize_single_line};
