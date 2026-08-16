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
//! - **CLI**: the `agenkitty skills` subcommand (in the `agenkitty` binary)
//!   exposes the same loader as `validate` / `list` / `inspect` / `index`
//!   with stable versioned JSON output.
//!
//! The `skill.*` tool family in the `agenkitty` crate builds on this; nothing
//! here executes scripts, dispatches hooks, or spawns anything.
//!
//! The crate is host-only (filesystem access); the target gate below is the
//! only `#[cfg(target_arch = …)]` in the crate, per the
//! client-server-modules convention.

#[cfg(not(target_arch = "wasm32"))]
mod server;

#[cfg(not(target_arch = "wasm32"))]
pub use server::*;
