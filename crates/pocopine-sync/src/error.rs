use std::fmt;

/// Result type used by the sync extension.
pub type SyncResult<T> = Result<T, SyncError>;

/// Errors produced by sync protocol validation and extension adapters.
#[derive(Debug)]
pub enum SyncError {
    /// A protocol value was empty or malformed.
    InvalidValue { field: &'static str, value: String },
    /// A requested stream is not registered on the sync server.
    UnknownStream(String),
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
    /// The request lacks the authorization scope the source requires.
    ///
    /// Built by app-side `CrudSource` / `SyncStreamSource` impls when the
    /// per-request context is missing the credentials the source needs
    /// (for example a tenant header, a JWT subject, or a session role).
    /// Surfaces as `ServerError::Unauthorized` so the wire response and
    /// the existing framework-level guard responses share one shape.
    Unauthorized(String),
    /// The client and server disagree on the application-level schema
    /// version for a stream, and the source has not registered a migrator
    /// that can bridge the two.
    ///
    /// Surfaces as `ServerError::BadRequest` on the wire so the client
    /// knows the mutation will never accept and should be dropped (or
    /// retried after re-encoding against the new shape). The follow-up
    /// batches will return this from a `SyncStreamSource::migrate_payload`
    /// default impl on the server side when a push arrives carrying an
    /// older `schema_version` than the source advertises, and from the
    /// client when cached rows can't be rehydrated against the new
    /// schema. The variant is introduced here so downstream callers can
    /// already typecheck against it.
    SchemaMigration { stream: String, from: u32, to: u32 },
}

impl SyncError {
    /// Build a typed `InvalidValue` error. Surfaces as
    /// `ServerError::BadRequest` over the wire via `server_error`.
    /// Used by `validate_params` impls for unknown param keys,
    /// missing required params, and wrong-shape values.
    pub fn invalid_value(field: &'static str, value: impl Into<String>) -> Self {
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

    /// Build a per-request authorization failure (missing tenant, missing
    /// session, etc.). Maps to `ServerError::Unauthorized` on the wire.
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::Unauthorized(msg.into())
    }

    /// Build a schema-version mismatch error for a stream. Maps to
    /// `ServerError::BadRequest` on the wire.
    pub fn schema_migration(stream: impl Into<String>, from: u32, to: u32) -> Self {
        Self::SchemaMigration {
            stream: stream.into(),
            from,
            to,
        }
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { field, value } => {
                write!(f, "invalid sync {field}: {:?}", display_value(value))
            }
            Self::UnknownStream(stream) => write!(f, "unknown sync stream: {stream}"),
            Self::Unsupported(msg) => write!(f, "unsupported sync operation: {msg}"),
            Self::Gap(cursor) => write!(f, "sync cursor is no longer resumable: {cursor}"),
            Self::Json(err) => write!(f, "sync json error: {err}"),
            Self::Client(msg) => write!(f, "sync client error: {msg}"),
            Self::Backend(msg) => write!(f, "sync backend error: {msg}"),
            Self::Unauthorized(msg) => write!(f, "sync unauthorized: {msg}"),
            Self::SchemaMigration { stream, from, to } => write!(
                f,
                "sync schema migration required for stream {stream}: client at v{from}, server at v{to}"
            ),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<serde_json::Error> for SyncError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

fn display_value(value: &str) -> String {
    const MAX_DISPLAY_CHARS: usize = 80;
    let mut preview = value.chars().take(MAX_DISPLAY_CHARS).collect::<String>();
    if value.chars().count() > MAX_DISPLAY_CHARS {
        preview.push_str("...");
    }
    preview
}
