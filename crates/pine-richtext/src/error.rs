use std::fmt;

/// Result type used by `pine-richtext`.
pub type RichTextResult<T> = Result<T, RichTextError>;

/// Errors produced by schema validation, transforms, selections, and serde.
#[derive(Debug)]
pub enum RichTextError {
    /// A schema definition is invalid.
    InvalidSchema(String),
    /// A requested node type is not present in the schema.
    UnknownNodeType(String),
    /// A requested mark type is not present in the schema.
    UnknownMarkType(String),
    /// A node's content does not satisfy its content expression.
    InvalidContent(String),
    /// A document position is outside the valid range or cannot be resolved.
    InvalidPosition { pos: usize, size: usize },
    /// A replacement could not be applied.
    Replace(String),
    /// A transform step could not be applied.
    Transform(String),
    /// A step's JSON representation is missing fields or unrecognised.
    Step(String),
    /// A selection is invalid for the current document.
    Selection(String),
    /// A plugin rejected or failed to produce state.
    Plugin(String),
    /// JSON serialization or deserialization failed.
    Json(serde_json::Error),
}

impl fmt::Display for RichTextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSchema(msg) => write!(f, "invalid schema: {msg}"),
            Self::UnknownNodeType(name) => write!(f, "unknown node type: {name}"),
            Self::UnknownMarkType(name) => write!(f, "unknown mark type: {name}"),
            Self::InvalidContent(msg) => write!(f, "invalid content: {msg}"),
            Self::InvalidPosition { pos, size } => {
                write!(f, "invalid position {pos}; document content size is {size}")
            }
            Self::Replace(msg) => write!(f, "replace failed: {msg}"),
            Self::Transform(msg) => write!(f, "transform failed: {msg}"),
            Self::Step(msg) => write!(f, "step: {msg}"),
            Self::Selection(msg) => write!(f, "invalid selection: {msg}"),
            Self::Plugin(msg) => write!(f, "plugin failed: {msg}"),
            Self::Json(err) => write!(f, "json error: {err}"),
        }
    }
}

impl std::error::Error for RichTextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(err) => Some(err),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for RichTextError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
