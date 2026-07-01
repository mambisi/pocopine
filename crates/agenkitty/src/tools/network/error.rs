//! Structured error taxonomy for the network tools.
//!
//! The framework's [`AgenkitError`] kinds are coarse; the network tools carry a
//! finer-grained code (mirroring Claude WebFetch's taxonomy) so traces and tests
//! can assert the precise reason a request was refused. [`NetError`] maps to an
//! [`AgenkitError`] at the tool boundary.

use pocopine_agenkit_core::AgenkitError;

/// Convenient `Result` alias for the network tools' internal plumbing.
pub type NetResult<T> = Result<T, NetError>;

/// The fine-grained reason a network request was refused or failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetErrorCode {
    /// Malformed, non-http(s), userinfo-bearing, or otherwise unparseable URL.
    InvalidUrl,
    /// The URL exceeded the maximum length (250 chars).
    UrlTooLong,
    /// Blocked by policy: not allowlisted, blocklisted, a private / metadata /
    /// SSRF target, a disallowed scheme or port, or robots-disallowed.
    UrlNotAllowed,
    /// Anti-exfiltration: the URL was not present in conversation context.
    UrlNotInContext,
    /// The host did not resolve, or the connection / HTTP request failed.
    UrlNotAccessible,
    /// The response content type is not fetchable as text (binary → net.download).
    UnsupportedContentType,
    /// A per-request or per-session request budget was exhausted.
    TooManyRequests,
    /// The per-request `max_uses` count was exceeded.
    MaxUsesExceeded,
}

impl NetErrorCode {
    /// The stable snake_case code, surfaced in messages and asserted in tests.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidUrl => "invalid_url",
            Self::UrlTooLong => "url_too_long",
            Self::UrlNotAllowed => "url_not_allowed",
            Self::UrlNotInContext => "url_not_in_context",
            Self::UrlNotAccessible => "url_not_accessible",
            Self::UnsupportedContentType => "unsupported_content_type",
            Self::TooManyRequests => "too_many_requests",
            Self::MaxUsesExceeded => "max_uses_exceeded",
        }
    }
}

/// A network error: a fine-grained [`NetErrorCode`] plus a human/model message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetError {
    pub code: NetErrorCode,
    pub message: String,
}

impl NetError {
    pub fn new(code: NetErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_url(message: impl Into<String>) -> Self {
        Self::new(NetErrorCode::InvalidUrl, message)
    }
    pub fn url_too_long(message: impl Into<String>) -> Self {
        Self::new(NetErrorCode::UrlTooLong, message)
    }
    pub fn not_allowed(message: impl Into<String>) -> Self {
        Self::new(NetErrorCode::UrlNotAllowed, message)
    }
    pub fn not_in_context(message: impl Into<String>) -> Self {
        Self::new(NetErrorCode::UrlNotInContext, message)
    }
    pub fn not_accessible(message: impl Into<String>) -> Self {
        Self::new(NetErrorCode::UrlNotAccessible, message)
    }
    pub fn unsupported_content_type(message: impl Into<String>) -> Self {
        Self::new(NetErrorCode::UnsupportedContentType, message)
    }
    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(NetErrorCode::TooManyRequests, message)
    }
    pub fn max_uses_exceeded(message: impl Into<String>) -> Self {
        Self::new(NetErrorCode::MaxUsesExceeded, message)
    }
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for NetError {}

/// Map the fine-grained network code onto the framework's coarse error kinds at
/// the tool boundary. The original code stays legible in the message (the model
/// is the primary consumer); the framework error type intentionally is not
/// widened with a structured `code` field for one tool family.
impl From<NetError> for AgenkitError {
    fn from(err: NetError) -> Self {
        let message = err.to_string();
        match err.code {
            NetErrorCode::InvalidUrl | NetErrorCode::UrlTooLong => {
                AgenkitError::validation(message)
            }
            NetErrorCode::UrlNotAllowed
            | NetErrorCode::UrlNotInContext
            | NetErrorCode::UnsupportedContentType => AgenkitError::tool_policy(message),
            NetErrorCode::TooManyRequests | NetErrorCode::MaxUsesExceeded => {
                AgenkitError::budget_exhausted(message)
            }
            // A transient upstream failure (DNS / connect / HTTP). `Provider` is
            // the framework's network-call-failed bucket: it is retryable and
            // avoids `NotFound`, which the server boundary rewrites to the
            // misleading "unknown AI flow".
            NetErrorCode::UrlNotAccessible => AgenkitError::provider(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_maps_to_framework_kind() {
        assert_eq!(
            AgenkitError::from(NetError::invalid_url("x")).kind(),
            "validation"
        );
        assert_eq!(
            AgenkitError::from(NetError::not_allowed("x")).kind(),
            "tool_policy"
        );
        assert_eq!(
            AgenkitError::from(NetError::too_many_requests("x")).kind(),
            "budget_exhausted"
        );
        assert_eq!(
            AgenkitError::from(NetError::not_accessible("x")).kind(),
            "provider"
        );
    }

    #[test]
    fn display_carries_the_code() {
        let err = NetError::not_allowed("blocked range");
        assert_eq!(err.to_string(), "url_not_allowed: blocked range");
    }
}
