//! Error type for `pocopine-collab` (hand-rolled, matching the workspace
//! convention in `pocopine-events` / `pocopine-realtime`).

use std::fmt;

pub type CollabResult<T> = Result<T, CollabError>;

/// Errors surfaced by the collab engine and store.
#[derive(Debug)]
pub enum CollabError {
    /// A protocol/schema identity is malformed or incompatible.
    Compatibility(String),
    /// A CRDT update or state vector could not be decoded.
    Decode(String),
    /// A decoded update could not be applied to the document.
    Apply(String),
    /// A `CollabStore` backend failed.
    Store(String),
}

impl CollabError {
    /// Build a [`CollabError::Store`] from anything stringy.
    pub fn store(msg: impl Into<String>) -> Self {
        Self::Store(msg.into())
    }
}

impl fmt::Display for CollabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compatibility(msg) => write!(f, "collab compatibility error: {msg}"),
            Self::Decode(msg) => write!(f, "collab decode error: {msg}"),
            Self::Apply(msg) => write!(f, "collab apply error: {msg}"),
            Self::Store(msg) => write!(f, "collab store error: {msg}"),
        }
    }
}

impl std::error::Error for CollabError {}
