use std::fmt;

use serde::{Deserialize, Serialize};

use crate::DocumentRevision;

pub type CodeResult<T> = Result<T, CodeError>;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewStatus {
    #[default]
    Ready,
    Failed(CodeError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitOutcome {
    pub revision: DocumentRevision,
    pub view_status: ViewStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeError {
    InvalidPosition,
    InvalidRange,
    OverlappingChanges,
    StaleRevision {
        expected: DocumentRevision,
        actual: DocumentRevision,
    },
    SizeLimit,
    InvalidConfiguration,
    RevisionExhausted,
    CompositionActive,
    ReadOnly,
    Disabled,
    ReentrantDispatch,
    Finalizing,
    Disposed,
    ViewUnavailable,
    ViewFailure(String),
    UnexpectedDetach,
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleRevision { expected, actual } => {
                write!(f, "stale revision {} (current {})", expected.0, actual.0)
            }
            Self::ViewFailure(message) => write!(f, "editor view failed: {message}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for CodeError {}
