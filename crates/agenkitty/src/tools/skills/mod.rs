//! The skill tool family (RFC-121): progressive disclosure of Agent Skills.
//!
//! Layering: the `agenkitty-skills` crate owns discovery, validation,
//! sanitization, and confinement (an agentskills.io-conformant loader with a
//! standalone CLI); this family is a thin consumer that adds what only the
//! host layer can — the `context_token` handshake, per-session activation
//! state, attenuation-only visibility for subagents, and policy specs.
//!
//! Level 1 (the name+description index) is injected into the system prompt by
//! the host via [`SkillRuntime::system_prompt_part`]; `skill.use` serves level
//! 2 (the body) and unlocks `skill.read` for level 3 (bundled files, confined
//! to the skill directory). Nothing here executes anything: a skill body that
//! calls for running `scripts/…` leads the model to the `process.*` family
//! and its sandbox, unchanged.

mod common;
mod read;
mod registry;
mod use_skill;

pub use common::{CurrentSkillContext, SkillRuntime, limits_from_config, resolve_skill_roots};
pub use read::{SKILL_READ_TOOL_ID, SkillReadInput, SkillReadOutput, SkillReadTool};
pub use registry::{known_skill_tool_ids, register_skill_tools, resolve_skill_tool_ids};
pub use use_skill::{SKILL_USE_TOOL_ID, SkillUseInput, SkillUseOutput, SkillUseTool};
