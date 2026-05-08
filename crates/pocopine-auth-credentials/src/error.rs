//! [`CredentialsError`] — structured failures for the credentials
//! routes and storage backends.

use std::fmt;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Outward-facing failure type for the credentials surface.
///
/// Variants map onto stable HTTP responses (see [`IntoResponse`])
/// and onto stable closed-set `reason` strings for observability.
/// Storage backends produce these by funnelling their own errors
/// through `Self::Storage` (one variant for any backend hiccup).
#[derive(Debug)]
pub enum CredentialsError {
    /// Login form rejected — wrong identifier or password. Same
    /// variant for both branches so a timing/shape probe can't tell
    /// which.
    InvalidCredentials,
    /// Signup rejected — login identifier already in use. Returned
    /// with a 409. Stable closed-set reason regardless of whether
    /// the identifier is an email, phone, or username.
    LoginIdTaken,
    /// Password failed the configured complexity validator
    /// (default: minimum length).
    WeakPassword(&'static str),
    /// Login identifier failed the configured
    /// [`crate::LoginIdValidator`]. The static reason carries the
    /// validator's failure class (`"missing_at_sign"`,
    /// `"too_short"`, …) for observability; the HTTP body still
    /// uses the closed-set `invalid_login_id` reason.
    InvalidLoginId(&'static str),
    /// Storage backend hiccup. Wraps the `UserStore` / `TokenStore`
    /// `Error` (`std::error::Error + Send + Sync`).
    Storage(Box<dyn std::error::Error + Send + Sync + 'static>),
    /// Argon2 hash / verify failed for a non-credential reason
    /// (params invalid, OS RNG unavailable, etc.).
    PasswordHashing(&'static str),
    /// JWT issuer rejected the token. Wraps the underlying
    /// `JwtAuthError` message; no claims content is included.
    SessionIssue(String),
    /// Builder validation failed at construction time.
    Configuration(&'static str),
}

impl fmt::Display for CredentialsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCredentials => f.write_str("invalid login or password"),
            Self::LoginIdTaken => f.write_str("login identifier already registered"),
            Self::WeakPassword(reason) => write!(f, "password rejected: {reason}"),
            Self::InvalidLoginId(reason) => write!(f, "login identifier rejected: {reason}"),
            Self::Storage(_) => f.write_str("credentials store unavailable"),
            Self::PasswordHashing(reason) => write!(f, "password hashing failure: {reason}"),
            Self::SessionIssue(reason) => write!(f, "session token issue failed: {reason}"),
            Self::Configuration(reason) => write!(f, "credentials configuration: {reason}"),
        }
    }
}

impl std::error::Error for CredentialsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

impl CredentialsError {
    /// Stable closed-set identifier suitable for `tracing` /
    /// observability fields. Never includes user input.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::InvalidCredentials => "invalid_credentials",
            Self::LoginIdTaken => "login_id_taken",
            Self::WeakPassword(_) => "weak_password",
            Self::InvalidLoginId(_) => "invalid_login_id",
            Self::Storage(_) => "storage_error",
            Self::PasswordHashing(_) => "password_hashing_error",
            Self::SessionIssue(_) => "session_issue_error",
            Self::Configuration(_) => "configuration_error",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::InvalidCredentials => StatusCode::UNAUTHORIZED,
            Self::LoginIdTaken => StatusCode::CONFLICT,
            Self::WeakPassword(_) | Self::InvalidLoginId(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Storage(_) | Self::PasswordHashing(_) | Self::SessionIssue(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Configuration(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for CredentialsError {
    fn into_response(self) -> Response {
        // Log the detailed error for operators; return only the
        // closed-set reason and a fixed-shape error-string to the
        // client. RFC-077 §6 — body strings never carry user PII or
        // backend internals.
        tracing::warn!(
            target: "pocopine.log",
            error = %self,
            reason = self.reason(),
            "credentials route rejected"
        );
        let status = self.status();
        let body = Json(json!({
            "error": self.reason(),
        }));
        (status, body).into_response()
    }
}
