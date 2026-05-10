use std::fmt;

/// Result type used by the sync extension.
pub type SyncResult<T> = Result<T, SyncError>;

/// Errors produced by sync protocol validation and extension adapters.
#[derive(Debug)]
pub enum SyncError {
    /// A protocol value was empty or malformed.
    InvalidValue { field: &'static str, value: String },
    /// A requested shape is not registered on the sync server.
    UnknownShape(String),
    /// The requested operation is not implemented by the current source.
    Unsupported(String),
    /// The server can no longer resume from the supplied cursor.
    Gap(String),
    /// JSON serialization or deserialization failed.
    Json(serde_json::Error),
    /// Browser, fetch, or runtime integration failed.
    Client(String),
    /// Host-side source or lock failure.
    Backend(String),
}

impl SyncError {
    pub(crate) fn invalid_value(field: &'static str, value: impl Into<String>) -> Self {
        Self::InvalidValue {
            field,
            value: value.into(),
        }
    }

    /// Build an unsupported-operation error.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    /// Build a client integration error.
    pub fn client(msg: impl Into<String>) -> Self {
        Self::Client(msg.into())
    }

    /// Build a backend/source error.
    pub fn backend(msg: impl Into<String>) -> Self {
        Self::Backend(msg.into())
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { field, value } => write!(f, "invalid sync {field}: {value:?}"),
            Self::UnknownShape(shape) => write!(f, "unknown sync shape: {shape}"),
            Self::Unsupported(msg) => write!(f, "unsupported sync operation: {msg}"),
            Self::Gap(cursor) => write!(f, "sync cursor is no longer resumable: {cursor}"),
            Self::Json(err) => write!(f, "sync json error: {err}"),
            Self::Client(msg) => write!(f, "sync client error: {msg}"),
            Self::Backend(msg) => write!(f, "sync backend error: {msg}"),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<serde_json::Error> for SyncError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}
