use std::fmt;

/// Result type used by the storage extension.
pub type StorageResult<T> = Result<T, StorageError>;

/// Errors produced by storage protocol validation, client uploads, and
/// backend adapters.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StorageError {
    /// A protocol value was empty or malformed.
    InvalidValue { field: String, value: String },
    /// A requested scope is not registered on the storage server.
    UnknownScope { scope: String },
    /// A scope references a backend that is not registered.
    UnknownBackend { backend: String },
    /// A requested upload session is unknown or no longer retained.
    UnknownUploadSession { session: String },
    /// Authentication is required for this storage operation.
    Unauthorized { message: String },
    /// Authentication succeeded, but this actor cannot perform the operation.
    Forbidden { message: String },
    /// The upload violates the registered scope policy.
    PolicyRejected { reason: String },
    /// The upload body or declared size exceeds a server limit.
    PayloadTooLarge { limit: u64 },
    /// The current backend or first implementation slice does not support this.
    Unsupported { message: String },
    /// Sequential upload offset did not match the backend's committed offset.
    OffsetMismatch { expected: u64, provided: u64 },
    /// Provider-side state changed concurrently with this operation.
    Conflict { message: String },
    /// The upload is already complete.
    UploadComplete { session: String },
    /// The upload was aborted or expired.
    UploadClosed { session: String },
    /// Browser, fetch, or runtime integration failed.
    Client { message: String },
    /// Host-side backend or filesystem failure.
    Backend { message: String },
}

impl StorageError {
    pub(crate) fn invalid_value(field: impl Into<String>, value: impl Into<String>) -> Self {
        Self::InvalidValue {
            field: field.into(),
            value: value.into(),
        }
    }

    pub fn unknown_scope(scope: impl Into<String>) -> Self {
        Self::UnknownScope {
            scope: scope.into(),
        }
    }

    pub fn unknown_backend(backend: impl Into<String>) -> Self {
        Self::UnknownBackend {
            backend: backend.into(),
        }
    }

    pub fn unknown_upload_session(session: impl Into<String>) -> Self {
        Self::UnknownUploadSession {
            session: session.into(),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized {
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden {
            message: message.into(),
        }
    }

    pub fn policy_rejected(reason: impl Into<String>) -> Self {
        Self::PolicyRejected {
            reason: reason.into(),
        }
    }

    pub fn payload_too_large(limit: u64) -> Self {
        Self::PayloadTooLarge { limit }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
        }
    }

    pub fn client(message: impl Into<String>) -> Self {
        Self::Client {
            message: message.into(),
        }
    }

    pub fn backend(message: impl Into<String>) -> Self {
        Self::Backend {
            message: message.into(),
        }
    }

    pub fn offset_mismatch(expected: u64, provided: u64) -> Self {
        Self::OffsetMismatch { expected, provided }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
        }
    }

    pub fn is_retryable_client_error(&self) -> bool {
        matches!(self, Self::Client { .. })
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { field, value } => {
                write!(f, "invalid storage {field}: {:?}", display_value(value))
            }
            Self::UnknownScope { scope } => write!(f, "unknown storage scope: {scope}"),
            Self::UnknownBackend { backend } => write!(f, "unknown storage backend: {backend}"),
            Self::UnknownUploadSession { session } => {
                write!(f, "unknown upload session: {session}")
            }
            Self::Unauthorized { message } => write!(f, "unauthorized storage request: {message}"),
            Self::Forbidden { message } => write!(f, "forbidden storage request: {message}"),
            Self::PolicyRejected { reason } => {
                write!(f, "storage policy rejected upload: {reason}")
            }
            Self::PayloadTooLarge { limit } => {
                write!(f, "storage upload exceeds maximum size of {limit} bytes")
            }
            Self::Unsupported { message } => write!(f, "unsupported storage operation: {message}"),
            Self::OffsetMismatch { expected, provided } => write!(
                f,
                "storage upload offset mismatch: expected {expected}, got {provided}"
            ),
            Self::Conflict { message } => write!(f, "storage conflict: {message}"),
            Self::UploadComplete { session } => write!(f, "upload session is complete: {session}"),
            Self::UploadClosed { session } => write!(f, "upload session is closed: {session}"),
            Self::Client { message } => write!(f, "storage client error: {message}"),
            Self::Backend { message } => write!(f, "storage backend error: {message}"),
        }
    }
}

impl std::error::Error for StorageError {}

fn display_value(value: &str) -> String {
    const MAX_DISPLAY_CHARS: usize = 80;
    let mut preview = value.chars().take(MAX_DISPLAY_CHARS).collect::<String>();
    if value.chars().count() > MAX_DISPLAY_CHARS {
        preview.push_str("...");
    }
    preview
}
