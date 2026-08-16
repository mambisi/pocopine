//! Error type for on-demand catalog reads.
//!
//! Discovery never returns these — per-skill problems become
//! [`SkillDiagnostic`](crate::SkillDiagnostic)s. Only the explicit read APIs
//! (`body`, `read_resource`) fail.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillError {
    /// Unknown skill name or missing resource file.
    NotFound(String),
    /// Confinement or secret-path policy refusal.
    Policy(String),
    /// Malformed request (empty/absolute path, bad window…).
    Validation(String),
    /// Underlying filesystem failure.
    Io(String),
}

impl SkillError {
    /// Stable kind tag, mirroring `AgenkitError::kind()` vocabulary so the
    /// runtime family maps 1:1.
    pub fn kind(&self) -> &'static str {
        match self {
            SkillError::NotFound(_) => "not_found",
            SkillError::Policy(_) => "tool_policy",
            SkillError::Validation(_) => "validation",
            SkillError::Io(_) => "internal",
        }
    }
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkillError::NotFound(msg)
            | SkillError::Policy(msg)
            | SkillError::Validation(msg)
            | SkillError::Io(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for SkillError {}
